package ls.postings

import java.lang.foreign.{Arena, MemorySegment, ValueLayout}
import java.nio.ByteOrder
import java.nio.channels.FileChannel
import java.nio.charset.StandardCharsets.UTF_8
import java.nio.file.{Files, Path, StandardOpenOption}
import java.util.zip.CRC32C

import ls.index.{DocOrd, OccFlags, OccurrenceHit, OccurrenceSink, RenameProfile, Role, Span, SymbolOrd, TargetBitset}

/** Read side of segment format v1 (docs/index-format.md).
  *
  * Every file of the segment directory is mapped READ_ONLY into
  * [[MemorySegment]]s owned by a single shared [[Arena]]; [[close]] closes the
  * arena and unmaps everything at once. Opening validates magic, version, the
  * header checksum and the CRC32C of every file recorded in checksums.bin; a
  * mismatch raises [[SegmentCorruptedException]] and nothing is served.
  *
  * Scans are allocation-free per occurrence: results are delivered through
  * the primitive-argument [[OccurrenceSink]]. Group scans apply, in order:
  *   1. block-level exact skip (target bitset intersection via
  *      [[TargetBitset.intersectsWords]], and editable-count skip for rename
  *      scans),
  *   2. per-occurrence target filtering against the allowed bitset,
  *   3. epoch filtering: an occurrence is surfaced only when its stored
  *      doc_epoch equals the doc dictionary epoch of its doc_ord (plan 9.3).
  */
final class SegmentReader private (
    val segmentDir: Path,
    arena: Arena,
    headerSeg: MemorySegment,
    refGroupIdx: MemorySegment,
    defGroupIdx: MemorySegment,
    renGroupIdx: MemorySegment,
    docIdx: MemorySegment,
    symIdx: MemorySegment,
    refPost: MemorySegment,
    defPost: MemorySegment,
    renPost: MemorySegment,
    docPost: MemorySegment,
    blockIdx: MemorySegment
) extends AutoCloseable:
  import SegmentFormat.*
  import SegmentReader.{LeInt, LeLong, readString}

  // --- header fields ---
  val segmentId: Long = headerSeg.get(LeLong, 8)
  val createdAtMs: Long = headerSeg.get(LeLong, 16)
  val refGroupCount: Int = headerSeg.get(LeLong, 24).toInt
  val renameGroupCount: Int = headerSeg.get(LeLong, 32).toInt
  val docCount: Int = headerSeg.get(LeLong, 40).toInt
  val occurrenceCount: Long = headerSeg.get(LeLong, 48)

  // --- doc-index layout ---
  private val docIntervalCount: Int = docIdx.get(LeLong, 8).toInt
  private val docEntryBase: Long = 24L
  private val intervalBase: Long = docEntryBase + DocEntrySize.toLong * docCount
  private val docBlobBase: Long = intervalBase + IntervalEntrySize.toLong * docIntervalCount

  // --- symbol-index layout ---
  val symbolCount: Int = symIdx.get(LeLong, 0).toInt
  val targetCount: Int = symIdx.get(LeLong, 8).toInt
  private val symEntryBase: Long = 24L
  private val targetIdBase: Long = symEntryBase + SymbolEntrySize.toLong * symbolCount
  private val symBlobBase: Long = targetIdBase + 8L * targetCount

  // --- block-index layout ---
  private val blockCount: Int = blockIdx.get(LeLong, 0).toInt
  private val blockWords: Int = blockIdx.get(LeInt, 8)
  private val blockEntrySize: Long = BlockEntryFixedSize.toLong + 8L * blockWords
  private val blockEntryBase: Long = 16L

  /** Current epoch per doc_ord, cached on heap for the per-occurrence epoch
    * filter on the scan hot path.
    */
  private val docEpochs: Array[Int] =
    val a = new Array[Int](docCount)
    var d = 0
    while d < docCount do
      a(d) = docIdx.get(LeInt, docEntryBase + DocEntrySize.toLong * d + 16)
      d += 1
    a

  // --- doc dictionary accessors ---

  private def docEntryOff(docOrd: Int): Long =
    require(docOrd >= 0 && docOrd < docCount, s"docOrd $docOrd out of range [0,$docCount)")
    docEntryBase + DocEntrySize.toLong * docOrd

  def uriOfDoc(docOrd: Int): String =
    val off = docEntryOff(docOrd)
    val uriOff = docIdx.get(LeInt, off)
    val uriLen = docIdx.get(LeInt, off + 4)
    readString(docIdx, docBlobBase + uriOff, uriLen)

  def docIdOf(docOrd: Int): Long = docIdx.get(LeLong, docEntryOff(docOrd) + 8)

  def epochOf(docOrd: Int): Int =
    require(docOrd >= 0 && docOrd < docCount, s"docOrd $docOrd out of range [0,$docCount)")
    docEpochs(docOrd)
  def targetOrdOfDoc(docOrd: Int): Int = docIdx.get(LeInt, docEntryOff(docOrd) + 20)
  def docFlagsOf(docOrd: Int): Int = docIdx.get(LeInt, docEntryOff(docOrd) + 24)

  // --- target dictionary ---

  def targetIdOf(targetOrd: Int): Long =
    require(targetOrd >= 0 && targetOrd < targetCount, s"targetOrd $targetOrd out of range [0,$targetCount)")
    symIdx.get(LeLong, targetIdBase + 8L * targetOrd)

  // --- symbol dictionary ---

  private def symEntryOff(symbolOrd: Int): Long =
    require(symbolOrd >= 0 && symbolOrd < symbolCount, s"symbolOrd $symbolOrd out of range [0,$symbolCount)")
    symEntryBase + SymbolEntrySize.toLong * symbolOrd

  def semanticSymbolOf(symbolOrd: Int): String =
    val off = symEntryOff(symbolOrd)
    readString(symIdx, symBlobBase + symIdx.get(LeInt, off), symIdx.get(LeInt, off + 4))

  def symbolIdOf(symbolOrd: Int): Long = symIdx.get(LeLong, symEntryOff(symbolOrd) + 8)
  def refGroupOrdOf(symbolOrd: Int): Int = symIdx.get(LeInt, symEntryOff(symbolOrd) + 16)
  def renameGroupOrdOf(symbolOrd: Int): Int = symIdx.get(LeInt, symEntryOff(symbolOrd) + 20)
  def defTargetOrdOf(symbolOrd: Int): Int = symIdx.get(LeInt, symEntryOff(symbolOrd) + 24)

  /** Binary search over the UTF-8-sorted on-disk symbol dictionary. Returns
    * the symbol ordinal or -1. No strings are materialized.
    */
  def findSymbolOrd(semanticSymbol: String): Int =
    val query = semanticSymbol.getBytes(UTF_8)
    var lo = 0
    var hi = symbolCount - 1
    while lo <= hi do
      val mid = (lo + hi) >>> 1
      val off = symEntryOff(mid)
      val strOff = symBlobBase + symIdx.get(LeInt, off)
      val strLen = symIdx.get(LeInt, off + 4)
      val cmp = compareUtf8(symIdx, strOff, strLen, query)
      if cmp == 0 then return mid
      else if cmp < 0 then lo = mid + 1
      else hi = mid - 1
    -1

  private def compareUtf8(seg: MemorySegment, off: Long, len: Int, query: Array[Byte]): Int =
    val n = math.min(len, query.length)
    var i = 0
    while i < n do
      val a = java.lang.Byte.toUnsignedInt(seg.get(ValueLayout.JAVA_BYTE, off + i))
      val b = java.lang.Byte.toUnsignedInt(query(i))
      if a != b then return Integer.compare(a, b)
      i += 1
    Integer.compare(len, query.length)

  // --- rename profiles ---

  def renameProfileOf(groupOrd: Int): RenameProfile =
    require(
      groupOrd >= 0 && groupOrd < renameGroupCount,
      s"rename group $groupOrd out of range [0,$renameGroupCount)"
    )
    val base = 8L + GroupIndexEntrySize.toLong * renameGroupCount + RenameProfileEntrySize.toLong * groupOrd
    val flags = renGroupIdx.get(LeInt, base)
    RenameProfile(
      isLocal = (flags & ProfIsLocal) != 0,
      isExternal = (flags & ProfIsExternal) != 0,
      hasGeneratedOccurrences = (flags & ProfHasGenerated) != 0,
      hasReadonlyOccurrences = (flags & ProfHasReadonly) != 0,
      hasOverrideFamily = (flags & ProfHasOverrideFamily) != 0,
      hasCompanion = (flags & ProfHasCompanion) != 0,
      editableOccurrenceCount = renGroupIdx.get(LeInt, base + 4),
      unsafeReasonMask = renGroupIdx.get(LeLong, base + 8)
    )

  // --- group postings scans ---

  def scanRefGroup(groupOrd: Int, allowed: TargetBitset | Null, sink: OccurrenceSink): Unit =
    scanGroup(refGroupIdx, refGroupCount, refPost, groupOrd, allowed, requireEditable = false, sink)

  def scanDefGroup(groupOrd: Int, sink: OccurrenceSink): Unit =
    scanGroup(defGroupIdx, refGroupCount, defPost, groupOrd, null, requireEditable = false, sink)

  def scanRenameGroup(groupOrd: Int, sink: OccurrenceSink): Unit =
    scanGroup(renGroupIdx, renameGroupCount, renPost, groupOrd, null, requireEditable = true, sink)

  private def scanGroup(
      idxSeg: MemorySegment,
      groupCount: Int,
      postSeg: MemorySegment,
      groupOrd: Int,
      allowed: TargetBitset | Null,
      requireEditable: Boolean,
      sink: OccurrenceSink
  ): Unit =
    require(groupOrd >= 0 && groupOrd < groupCount, s"group ordinal $groupOrd out of range [0,$groupCount)")
    val entryOff = 8L + GroupIndexEntrySize.toLong * groupOrd
    val count = idxSeg.get(LeInt, entryOff + 8)
    if count == 0 then return
    val blockFirst = idxSeg.get(LeInt, entryOff + 12)

    val recCount = postSeg.get(LeLong, 0)
    val colDoc = 8L
    val colEpoch = colDoc + 4L * recCount
    val colTarget = colEpoch + 4L * recCount
    val colStart = colTarget + 4L * recCount
    val colEnd = colStart + 4L * recCount
    val colFlags = colEnd + 4L * recCount

    val wordsBuf = new Array[Long](blockWords)
    val nBlocks = (count + BlockSize - 1) / BlockSize
    var b = 0
    while b < nBlocks do
      val be = blockEntryBase + blockEntrySize * (blockFirst + b)
      val bFirst = blockIdx.get(LeLong, be)
      val bCount = blockIdx.get(LeInt, be + 8)
      val bEditable = blockIdx.get(LeInt, be + 12)
      var skip = requireEditable && bEditable == 0
      if !skip && (allowed ne null) then
        var w = 0
        while w < blockWords do
          wordsBuf(w) = blockIdx.get(LeLong, be + BlockEntryFixedSize + 8L * w)
          w += 1
        skip = !allowed.intersectsWords(wordsBuf)
      if !skip then
        var k = 0
        while k < bCount do
          val r = bFirst + k
          val docOrd = postSeg.get(LeInt, colDoc + 4L * r)
          val epoch = postSeg.get(LeInt, colEpoch + 4L * r)
          val targetOrd = postSeg.get(LeInt, colTarget + 4L * r)
          val flags = postSeg.get(LeInt, colFlags + 4L * r)
          if ((allowed eq null) || allowed.contains(targetOrd)) &&
            epoch == docEpochs(docOrd) &&
            (!requireEditable || OccFlags.has(flags, OccFlags.Editable))
          then
            sink.accept(
              docOrd,
              targetOrd,
              epoch,
              postSeg.get(LeInt, colStart + 4L * r),
              postSeg.get(LeInt, colEnd + 4L * r),
              flags
            )
          k += 1
      b += 1

  // --- doc postings ---

  private def docPostCols: (Long, Long, Long, Long) =
    val recCount = docPost.get(LeLong, 0)
    val colSym = 8L
    val colStart = colSym + 4L * recCount
    val colEnd = colStart + 4L * recCount
    val colFlags = colEnd + 4L * recCount
    (colSym, colStart, colEnd, colFlags)

  /** Full scan of one doc's postings, in (packed_start, packed_end) order.
    * doc-postings records carry no per-record target/epoch columns; the doc
    * dictionary values are passed to the sink.
    */
  def scanDoc(docOrd: Int, sink: OccurrenceSink, requireEditable: Boolean = false): Unit =
    val off = docEntryOff(docOrd)
    val first = docIdx.get(LeLong, off + 32)
    val count = docIdx.get(LeInt, off + 40)
    val targetOrd = docIdx.get(LeInt, off + 20)
    val epoch = docEpochs(docOrd)
    val (_, colStart, colEnd, colFlags) = docPostCols
    var k = 0L
    while k < count do
      val r = first + k
      val flags = docPost.get(LeInt, colFlags + 4L * r)
      if !requireEditable || OccFlags.has(flags, OccFlags.Editable) then
        sink.accept(
          docOrd,
          targetOrd,
          epoch,
          docPost.get(LeInt, colStart + 4L * r),
          docPost.get(LeInt, colEnd + 4L * r),
          flags
        )
      k += 1

  /** Exact symbol-at-position over the doc interval block index. The smallest
    * covering occurrence wins; among equally small covering occurrences the
    * earliest-starting one wins. Span containment is start-inclusive and
    * end-inclusive, matching [[Span.contains]].
    */
  def symbolAt(docOrd: Int, line: Int, character: Int): Option[OccurrenceHit] =
    symbolAtCounting(docOrd, line, character)._1

  /** Same as [[symbolAt]] but also returns how many interval blocks had their
    * occurrences scanned — diagnostic hook for asserting block-index
    * effectiveness in tests and the doctor.
    */
  def symbolAtCounting(docOrd: Int, line: Int, character: Int): (Option[OccurrenceHit], Int) =
    val off = docEntryOff(docOrd)
    val intervalFirst = docIdx.get(LeInt, off + 28)
    val intervalCount = docIdx.get(LeInt, off + 44)
    if intervalFirst < 0 || intervalCount == 0 then return (None, 0)

    val queryPacked = Span.pack(line, character)
    val (colSym, colStart, colEnd, colFlags) = docPostCols

    var blocksScanned = 0
    var bestStart = 0
    var bestEnd = 0
    var bestSize = Int.MaxValue
    var bestSym = -1
    var bestFlags = 0
    var found = false

    var b = 0
    var done = false
    while b < intervalCount && !done do
      val ie = intervalBase + IntervalEntrySize.toLong * (intervalFirst + b)
      val firstLine = docIdx.get(LeInt, ie)
      val lastLine = docIdx.get(LeInt, ie + 4)
      if firstLine > line then done = true // blocks sorted by first start line
      else if lastLine >= line then
        blocksScanned += 1
        val recFirst = docIdx.get(LeLong, ie + 8)
        val recCount = docIdx.get(LeInt, ie + 16)
        var k = 0
        var innerDone = false
        while k < recCount && !innerDone do
          val r = recFirst + k
          val ps = docPost.get(LeInt, colStart + 4L * r)
          if ps > queryPacked then innerDone = true // sorted by packed_start
          else
            val pe = docPost.get(LeInt, colEnd + 4L * r)
            if pe >= queryPacked then
              val size = pe - ps
              if !found || size < bestSize then
                found = true
                bestSize = size
                bestStart = ps
                bestEnd = pe
                bestSym = docPost.get(LeInt, colSym + 4L * r)
                bestFlags = docPost.get(LeInt, colFlags + 4L * r)
          k += 1
      b += 1

    if !found then (None, blocksScanned)
    else
      val role =
        if OccFlags.has(bestFlags, OccFlags.Definition) then Role.Definition else Role.Reference
      val span = Span(
        Span.unpackLine(bestStart),
        Span.unpackChar(bestStart),
        Span.unpackLine(bestEnd),
        Span.unpackChar(bestEnd)
      )
      (Some(OccurrenceHit(SymbolOrd(bestSym), DocOrd(docOrd), span, role, bestFlags)), blocksScanned)

  def close(): Unit = arena.close()

object SegmentReader:
  import SegmentFormat.*

  private val LeInt: ValueLayout.OfInt =
    ValueLayout.JAVA_INT_UNALIGNED.withOrder(ByteOrder.LITTLE_ENDIAN)
  private val LeLong: ValueLayout.OfLong =
    ValueLayout.JAVA_LONG_UNALIGNED.withOrder(ByteOrder.LITTLE_ENDIAN)
  private val LeShort: ValueLayout.OfShort =
    ValueLayout.JAVA_SHORT_UNALIGNED.withOrder(ByteOrder.LITTLE_ENDIAN)

  /** Maps and validates a segment directory. Throws
    * [[SegmentCorruptedException]] on any validation failure; the arena is
    * closed before throwing so no mapping leaks.
    */
  def open(segmentDir: Path): SegmentReader =
    val arena = Arena.ofShared()
    try
      def mapFile(name: String): MemorySegment =
        val path = segmentDir.resolve(name)
        if !Files.isRegularFile(path) then
          throw SegmentCorruptedException(s"missing segment file: $path")
        val ch = FileChannel.open(path, StandardOpenOption.READ)
        try ch.map(FileChannel.MapMode.READ_ONLY, 0, ch.size(), arena)
        finally ch.close()

      val headerSeg = mapFile(HeaderFile)
      if headerSeg.byteSize() != HeaderSize then
        throw SegmentCorruptedException(
          s"header.bin has size ${headerSeg.byteSize()}, expected $HeaderSize"
        )
      val magic = headerSeg.get(LeInt, 0)
      if magic != Magic then
        throw SegmentCorruptedException(f"bad magic 0x$magic%08x, expected 0x$Magic%08x")
      val version = java.lang.Short.toUnsignedInt(headerSeg.get(LeShort, 4))
      if version != Version then
        throw SegmentCorruptedException(s"unsupported segment version $version, expected $Version")
      val headerCrc = headerSeg.get(LeLong, 56)
      val computedHeaderCrc = crcOf(headerSeg.asSlice(0, 56))
      if headerCrc != computedHeaderCrc then
        throw SegmentCorruptedException(
          f"header checksum mismatch: stored 0x$headerCrc%08x, computed 0x$computedHeaderCrc%08x"
        )

      val segs: Map[String, MemorySegment] =
        ChecksummedFiles.map(name => name -> (if name == HeaderFile then headerSeg else mapFile(name))).toMap

      // checksums.bin: must list exactly the checksummed files, in order.
      val checksums = mapFile(ChecksumsFile)
      val entryCount = checksums.get(LeLong, 0)
      if entryCount != ChecksummedFiles.length.toLong then
        throw SegmentCorruptedException(
          s"checksums.bin lists $entryCount files, expected ${ChecksummedFiles.length}"
        )
      var off = 8L
      for expected <- ChecksummedFiles do
        val nameLen = checksums.get(LeInt, off)
        if nameLen <= 0 || nameLen > 4096 then
          throw SegmentCorruptedException(s"checksums.bin: bad name length $nameLen")
        val nameBytes = new Array[Byte](nameLen)
        MemorySegment.copy(checksums, off + 4, MemorySegment.ofArray(nameBytes), 0, nameLen.toLong)
        val name = String(nameBytes, UTF_8)
        if name != expected then
          throw SegmentCorruptedException(s"checksums.bin: entry '$name' where '$expected' expected")
        val stored = checksums.get(LeLong, off + 4 + nameLen)
        val computed = crcOf(segs(name))
        if stored != computed then
          throw SegmentCorruptedException(
            f"checksum mismatch for $name: stored 0x$stored%08x, computed 0x$computed%08x"
          )
        off += 4 + nameLen + 8

      val reader = new SegmentReader(
        segmentDir,
        arena,
        headerSeg,
        refGroupIdx = segs(RefGroupIndexFile),
        defGroupIdx = segs(DefinitionGroupIndexFile),
        renGroupIdx = segs(RenameGroupIndexFile),
        docIdx = segs(DocIndexFile),
        symIdx = segs(SymbolIndexFile),
        refPost = segs(RefPostingsFile),
        defPost = segs(DefinitionPostingsFile),
        renPost = segs(RenamePostingsFile),
        docPost = segs(DocPostingsFile),
        blockIdx = segs(BlockIndexFile)
      )
      validateStructure(reader, segs)
      reader
    catch
      case e: Throwable =>
        arena.close()
        e match
          case c: SegmentCorruptedException => throw c
          case io: java.io.IOException => throw io
          case other =>
            throw SegmentCorruptedException(s"segment $segmentDir failed to open: $other", other)

  /** Cheap structural cross-checks between header counts and file sizes so a
    * consistent-CRC-but-nonsense segment is still rejected early.
    */
  private def validateStructure(r: SegmentReader, segs: Map[String, MemorySegment]): Unit =
    def fail(msg: String): Nothing = throw SegmentCorruptedException(msg)

    def checkGroupIndex(name: String, groupCount: Int, withProfiles: Boolean): Unit =
      val seg = segs(name)
      val declared = seg.get(LeLong, 0)
      if declared != groupCount.toLong then
        fail(s"$name declares $declared groups, header says $groupCount")
      val expectedSize =
        8L + GroupIndexEntrySize.toLong * groupCount +
          (if withProfiles then RenameProfileEntrySize.toLong * groupCount else 0L)
      if seg.byteSize() != expectedSize then
        fail(s"$name has size ${seg.byteSize()}, expected $expectedSize")

    checkGroupIndex(RefGroupIndexFile, r.refGroupCount, withProfiles = false)
    checkGroupIndex(DefinitionGroupIndexFile, r.refGroupCount, withProfiles = false)
    checkGroupIndex(RenameGroupIndexFile, r.renameGroupCount, withProfiles = true)

    def checkPostings(name: String, columns: Int): Long =
      val seg = segs(name)
      val recCount = seg.get(LeLong, 0)
      val expected = 8L + 4L * columns * recCount
      if recCount < 0 || seg.byteSize() != expected then
        fail(s"$name has size ${seg.byteSize()}, expected $expected for $recCount records")
      recCount

    val refRecs = checkPostings(RefPostingsFile, 6)
    val defRecs = checkPostings(DefinitionPostingsFile, 6)
    val renRecs = checkPostings(RenamePostingsFile, 6)
    val docRecs = checkPostings(DocPostingsFile, 4)
    if refRecs + defRecs + renRecs + docRecs != r.occurrenceCount then
      fail(
        s"header occurrence_count ${r.occurrenceCount} != ${refRecs + defRecs + renRecs + docRecs} records on disk"
      )

    val docSeg = segs(DocIndexFile)
    val declaredDocs = docSeg.get(LeLong, 0)
    if declaredDocs != r.docCount.toLong then
      fail(s"doc-index.bin declares $declaredDocs docs, header says ${r.docCount}")
    val intervals = docSeg.get(LeLong, 8)
    val docBlob = docSeg.get(LeLong, 16)
    val expectedDocIdx =
      24L + DocEntrySize.toLong * r.docCount + IntervalEntrySize.toLong * intervals + docBlob
    if docSeg.byteSize() != expectedDocIdx then
      fail(s"doc-index.bin has size ${docSeg.byteSize()}, expected $expectedDocIdx")

    val symSeg = segs(SymbolIndexFile)
    val symBlob = symSeg.get(LeLong, 16)
    val expectedSymIdx =
      24L + SymbolEntrySize.toLong * r.symbolCount + 8L * r.targetCount + symBlob
    if symSeg.byteSize() != expectedSymIdx then
      fail(s"symbol-index.bin has size ${symSeg.byteSize()}, expected $expectedSymIdx")

    val blockSeg = segs(BlockIndexFile)
    val blocks = blockSeg.get(LeLong, 0)
    val words = blockSeg.get(LeInt, 8)
    val declaredBlockSize = blockSeg.get(LeInt, 12)
    if declaredBlockSize != BlockSize then
      fail(s"block-index.bin block size $declaredBlockSize, expected $BlockSize")
    if words != targetWordCount(r.targetCount) then
      fail(s"block-index.bin has $words bitset words, expected ${targetWordCount(r.targetCount)}")
    val expectedBlockIdx = 16L + (BlockEntryFixedSize.toLong + 8L * words) * blocks
    if blockSeg.byteSize() != expectedBlockIdx then
      fail(s"block-index.bin has size ${blockSeg.byteSize()}, expected $expectedBlockIdx")

  private def crcOf(seg: MemorySegment): Long =
    // CRC32C.update(ByteBuffer) rejects buffers over closeable shared arenas,
    // so stream through a heap chunk instead.
    val crc = new CRC32C
    val size = seg.byteSize()
    val buf = new Array[Byte](math.max(1L, math.min(1L << 20, size)).toInt)
    var off = 0L
    while off < size do
      val n = math.min(buf.length.toLong, size - off).toInt
      MemorySegment.copy(seg, off, MemorySegment.ofArray(buf), 0, n.toLong)
      crc.update(buf, 0, n)
      off += n
    crc.getValue

  private[postings] def readString(seg: MemorySegment, off: Long, len: Int): String =
    val bytes = new Array[Byte](len)
    MemorySegment.copy(seg, off, MemorySegment.ofArray(bytes), 0, len.toLong)
    String(bytes, UTF_8)
