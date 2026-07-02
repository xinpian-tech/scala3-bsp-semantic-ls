package ls.postings

import java.nio.ByteBuffer
import java.nio.channels.FileChannel
import java.nio.charset.StandardCharsets.UTF_8
import java.nio.file.{Files, Path, StandardCopyOption, StandardOpenOption}
import java.util.zip.CRC32C

import ls.index.{OccFlags, Span}

/** Writes one complete, immutable segment directory (format v1, see
  * docs/index-format.md).
  *
  * Durability protocol: all files are first written into `<root>/tmp-<id>`,
  * each file is fsynced, the tmp directory is fsynced, then the directory is
  * atomically renamed to `<root>/segments/segment-NNNNNN` and the segments
  * directory is fsynced. A crash can only leave behind a tmp directory,
  * never a partially visible segment.
  */
object SegmentWriter:
  import SegmentFormat.*

  /** Little-endian growable byte builder. */
  private final class LeBuf(initial: Int = 4096):
    private var arr = new Array[Byte](math.max(16, initial))
    private var len = 0

    private def ensure(n: Int): Unit =
      if len + n > arr.length then
        var cap = arr.length * 2
        while cap < len + n do cap *= 2
        arr = java.util.Arrays.copyOf(arr, cap)

    def i16(v: Int): Unit =
      ensure(2)
      arr(len) = (v & 0xff).toByte
      arr(len + 1) = ((v >>> 8) & 0xff).toByte
      len += 2

    def i32(v: Int): Unit =
      ensure(4)
      arr(len) = (v & 0xff).toByte
      arr(len + 1) = ((v >>> 8) & 0xff).toByte
      arr(len + 2) = ((v >>> 16) & 0xff).toByte
      arr(len + 3) = ((v >>> 24) & 0xff).toByte
      len += 4

    def i64(v: Long): Unit =
      i32(v.toInt)
      i32((v >>> 32).toInt)

    def bytes(b: Array[Byte]): Unit =
      ensure(b.length)
      System.arraycopy(b, 0, arr, len, b.length)
      len += b.length

    def size: Int = len
    def result(): Array[Byte] = java.util.Arrays.copyOf(arr, len)

  /** Collects block-index entries shared by the three group postings files. */
  private final class BlockCollector(targetWords: Int):
    val buf = new LeBuf(64 * 1024)
    var count = 0

    def add(
        firstRecord: Long,
        recordCount: Int,
        editableCount: Int,
        refRoleCount: Int,
        defRoleCount: Int,
        docMin: Int,
        docMax: Int,
        epochMin: Int,
        epochMax: Int,
        words: Array[Long]
    ): Int =
      val ordinal = count
      buf.i64(firstRecord)
      buf.i32(recordCount)
      buf.i32(editableCount)
      buf.i32(refRoleCount)
      buf.i32(defRoleCount)
      buf.i32(docMin)
      buf.i32(docMax)
      buf.i32(epochMin)
      buf.i32(epochMax)
      var w = 0
      while w < targetWords do
        buf.i64(if w < words.length then words(w) else 0L)
        w += 1
      count += 1
      ordinal

  private final case class GroupEntry(offset: Long, count: Int, blockIndexOffset: Int)

  /** Result of laying out one family of group postings. */
  private final case class GroupLayout(postings: Array[Byte], entries: Array[GroupEntry])

  /** Writes the segment and returns the final segment directory path. */
  def write(
      root: Path,
      segmentId: Long,
      data: SegmentData,
      createdAtMs: Long = System.currentTimeMillis()
  ): Path =
    validate(data)

    val targetCount = data.targets.length
    val words = targetWordCount(targetCount)

    // --- symbol dictionary: sort by UTF-8 bytes, remap caller ordinals ---
    val n = data.symbols.length
    val symUtf8: Array[Array[Byte]] =
      data.symbols.iterator.map(_.semanticSymbol.getBytes(UTF_8)).toArray
    val sortedIdx: Array[Int] =
      Array.range(0, n).sortWith((a, b) => java.util.Arrays.compareUnsigned(symUtf8(a), symUtf8(b)) < 0)
    var i = 1
    while i < n do
      if java.util.Arrays.compareUnsigned(symUtf8(sortedIdx(i - 1)), symUtf8(sortedIdx(i))) == 0 then
        throw IllegalArgumentException(
          s"duplicate semantic symbol: ${data.symbols(sortedIdx(i)).semanticSymbol}"
        )
      i += 1
    val callerToSorted = new Array[Int](n)
    var newOrd = 0
    while newOrd < n do
      callerToSorted(sortedIdx(newOrd)) = newOrd
      newOrd += 1

    val symbolIndex = buildSymbolIndex(data, sortedIdx, symUtf8)

    // --- group postings + shared block index (ref, then def, then rename) ---
    val blocks = new BlockCollector(words)
    val refLayout = layoutGroups(data.refOccurrences, data, blocks, words)
    val defLayout = layoutGroups(data.defOccurrences, data, blocks, words)
    val renLayout = layoutGroups(data.renameOccurrences, data, blocks, words)

    val blockIndex =
      val b = new LeBuf(16 + blocks.buf.size)
      b.i64(blocks.count.toLong)
      b.i32(words)
      b.i32(BlockSize)
      b.bytes(blocks.buf.result())
      b.result()

    // --- doc postings + doc dictionary with interval index ---
    val (docPostings, docIndex, docRecordCount) = buildDocFiles(data, callerToSorted)

    val refGroupIndex = buildGroupIndex(refLayout.entries, None)
    val defGroupIndex = buildGroupIndex(defLayout.entries, None)
    val renGroupIndex = buildGroupIndex(renLayout.entries, Some(data.renameProfiles))

    val groupRecordCount =
      data.refOccurrences.iterator.map(_.length.toLong).sum +
        data.defOccurrences.iterator.map(_.length.toLong).sum +
        data.renameOccurrences.iterator.map(_.length.toLong).sum
    val occurrenceCount = groupRecordCount + docRecordCount

    val header = buildHeader(
      segmentId,
      createdAtMs,
      refGroupCount = data.refGroupCount,
      renameGroupCount = data.renameGroupCount,
      docCount = data.docs.length,
      occurrenceCount = occurrenceCount
    )

    val files: List[(String, Array[Byte])] = List(
      HeaderFile -> header,
      RefGroupIndexFile -> refGroupIndex,
      DefinitionGroupIndexFile -> defGroupIndex,
      RenameGroupIndexFile -> renGroupIndex,
      DocIndexFile -> docIndex,
      SymbolIndexFile -> symbolIndex,
      RefPostingsFile -> refLayout.postings,
      DefinitionPostingsFile -> defLayout.postings,
      RenamePostingsFile -> renLayout.postings,
      DocPostingsFile -> docPostings,
      BlockIndexFile -> blockIndex
    )

    val checksums =
      val b = new LeBuf(512)
      b.i64(files.length.toLong)
      val byName = files.toMap
      for name <- ChecksummedFiles do
        val nameBytes = name.getBytes(UTF_8)
        b.i32(nameBytes.length)
        b.bytes(nameBytes)
        b.i64(crc32c(byName(name)))
      b.result()

    // --- durable, atomic publication ---
    val tmp = root.resolve(s"tmp-$segmentId")
    if Files.exists(tmp) then deleteRecursively(tmp)
    Files.createDirectories(tmp)
    for (name, bytes) <- files :+ (ChecksumsFile -> checksums) do
      writeFileSync(tmp.resolve(name), bytes)
    fsyncDir(tmp)

    val segmentsDir = root.resolve("segments")
    Files.createDirectories(segmentsDir)
    val dest = segmentsDir.resolve(segmentDirName(segmentId))
    if Files.exists(dest) then
      throw IllegalStateException(s"segment directory already exists: $dest")
    Files.move(tmp, dest, StandardCopyOption.ATOMIC_MOVE)
    fsyncDir(segmentsDir)
    dest

  // --- file builders ---

  private def buildHeader(
      segmentId: Long,
      createdAtMs: Long,
      refGroupCount: Int,
      renameGroupCount: Int,
      docCount: Int,
      occurrenceCount: Long
  ): Array[Byte] =
    val b = new LeBuf(HeaderSize)
    b.i32(Magic)
    b.i16(Version)
    b.i16(0) // flags
    b.i64(segmentId)
    b.i64(createdAtMs)
    b.i64(refGroupCount.toLong)
    b.i64(renameGroupCount.toLong)
    b.i64(docCount.toLong)
    b.i64(occurrenceCount)
    val prefix = b.result()
    b.i64(crc32c(prefix))
    b.result()

  private def buildGroupIndex(
      entries: Array[GroupEntry],
      profiles: Option[Vector[ls.index.RenameProfile]]
  ): Array[Byte] =
    val b = new LeBuf(8 + entries.length * (GroupIndexEntrySize + RenameProfileEntrySize))
    b.i64(entries.length.toLong)
    for e <- entries do
      b.i64(e.offset)
      b.i32(e.count)
      b.i32(e.blockIndexOffset)
    profiles.foreach { ps =>
      for p <- ps do
        var flags = 0
        if p.isLocal then flags |= ProfIsLocal
        if p.isExternal then flags |= ProfIsExternal
        if p.hasGeneratedOccurrences then flags |= ProfHasGenerated
        if p.hasReadonlyOccurrences then flags |= ProfHasReadonly
        if p.hasOverrideFamily then flags |= ProfHasOverrideFamily
        if p.hasCompanion then flags |= ProfHasCompanion
        b.i32(flags)
        b.i32(p.editableOccurrenceCount)
        b.i64(p.unsafeReasonMask)
    }
    b.result()

  /** Lays out one family of group postings columnar file and appends its skip
    * blocks to the shared collector.
    */
  private def layoutGroups(
      groups: Vector[Vector[GroupOcc]],
      data: SegmentData,
      blocks: BlockCollector,
      targetWords: Int
  ): GroupLayout =
    val total = groups.iterator.map(_.length).sum
    val docCol = new Array[Int](total)
    val epochCol = new Array[Int](total)
    val targetCol = new Array[Int](total)
    val startCol = new Array[Int](total)
    val endCol = new Array[Int](total)
    val flagsCol = new Array[Int](total)
    val entries = new Array[GroupEntry](groups.length)

    var rec = 0
    var g = 0
    while g < groups.length do
      val sorted = groups(g).sortBy(o =>
        (o.docOrd, Span.pack(o.span.startLine, o.span.startChar), Span.pack(o.span.endLine, o.span.endChar))
      )
      val first = rec
      var blockFirst = -1
      var off = 0
      while off < sorted.length do
        val len = math.min(BlockSize, sorted.length - off)
        var editable = 0
        var refRole = 0
        var defRole = 0
        var docMin = Int.MaxValue
        var docMax = Int.MinValue
        var epMin = Int.MaxValue
        var epMax = Int.MinValue
        val words = new Array[Long](targetWords)
        var k = 0
        while k < len do
          val o = sorted(off + k)
          if OccFlags.has(o.flags, OccFlags.Editable) then editable += 1
          if OccFlags.has(o.flags, OccFlags.Definition) then defRole += 1 else refRole += 1
          docMin = math.min(docMin, o.docOrd)
          docMax = math.max(docMax, o.docOrd)
          epMin = math.min(epMin, o.docEpoch)
          epMax = math.max(epMax, o.docEpoch)
          words(o.targetOrd >>> 6) |= 1L << (o.targetOrd & 63)
          docCol(rec + k) = o.docOrd
          epochCol(rec + k) = o.docEpoch
          targetCol(rec + k) = o.targetOrd
          startCol(rec + k) = Span.pack(o.span.startLine, o.span.startChar)
          endCol(rec + k) = Span.pack(o.span.endLine, o.span.endChar)
          flagsCol(rec + k) = o.flags
          k += 1
        val ordinal = blocks.add(
          firstRecord = (rec).toLong,
          recordCount = len,
          editableCount = editable,
          refRoleCount = refRole,
          defRoleCount = defRole,
          docMin = docMin,
          docMax = docMax,
          epochMin = epMin,
          epochMax = epMax,
          words = words
        )
        if blockFirst < 0 then blockFirst = ordinal
        rec += len
        off += len
      entries(g) = GroupEntry(first.toLong, sorted.length, if sorted.isEmpty then -1 else blockFirst)
      g += 1

    val b = new LeBuf(8 + total * 24)
    b.i64(total.toLong)
    for col <- List(docCol, epochCol, targetCol, startCol, endCol, flagsCol) do
      var r = 0
      while r < total do
        b.i32(col(r))
        r += 1
    GroupLayout(b.result(), entries)

  /** Builds doc-postings.bin and doc-index.bin (dictionary + interval block
    * index + uri blob). Returns (docPostings, docIndex, recordCount).
    */
  private def buildDocFiles(
      data: SegmentData,
      callerToSorted: Array[Int]
  ): (Array[Byte], Array[Byte], Long) =
    val total = data.docOccurrences.iterator.map(_.length).sum
    val symCol = new Array[Int](total)
    val startCol = new Array[Int](total)
    val endCol = new Array[Int](total)
    val flagsCol = new Array[Int](total)

    final case class Interval(firstLine: Int, lastLine: Int, offset: Long, count: Int)
    val intervals = Vector.newBuilder[Interval]
    var intervalCount = 0
    // per doc: (postingsOffset, postingsCount, intervalFirst, intervalCount)
    val docPostingsMeta = new Array[(Long, Int, Int, Int)](data.docs.length)

    var rec = 0
    var d = 0
    while d < data.docs.length do
      val sorted = data.docOccurrences(d).sortBy(o =>
        (Span.pack(o.span.startLine, o.span.startChar), Span.pack(o.span.endLine, o.span.endChar))
      )
      val first = rec
      val intervalFirst = if sorted.isEmpty then -1 else intervalCount
      var nIntervals = 0
      var off = 0
      while off < sorted.length do
        val len = math.min(BlockSize, sorted.length - off)
        var lastLine = Int.MinValue
        var k = 0
        while k < len do
          val o = sorted(off + k)
          symCol(rec + k) = callerToSorted(o.symbolOrd)
          startCol(rec + k) = Span.pack(o.span.startLine, o.span.startChar)
          endCol(rec + k) = Span.pack(o.span.endLine, o.span.endChar)
          flagsCol(rec + k) = o.flags
          lastLine = math.max(lastLine, o.span.endLine)
          k += 1
        intervals += Interval(sorted(off).span.startLine, lastLine, rec.toLong, len)
        intervalCount += 1
        nIntervals += 1
        rec += len
        off += len
      docPostingsMeta(d) = (first.toLong, sorted.length, intervalFirst, nIntervals)
      d += 1

    val docPostings =
      val b = new LeBuf(8 + total * 16)
      b.i64(total.toLong)
      for col <- List(symCol, startCol, endCol, flagsCol) do
        var r = 0
        while r < total do
          b.i32(col(r))
          r += 1
      b.result()

    // doc-index.bin
    val blob = new LeBuf(4096)
    val uriOffsets = new Array[(Int, Int)](data.docs.length)
    d = 0
    while d < data.docs.length do
      val bytes = data.docs(d).uri.getBytes(UTF_8)
      uriOffsets(d) = (blob.size, bytes.length)
      blob.bytes(bytes)
      d += 1
    val blobBytes = blob.result()
    val allIntervals = intervals.result()

    val idx = new LeBuf(24 + data.docs.length * DocEntrySize + allIntervals.length * IntervalEntrySize + blobBytes.length)
    idx.i64(data.docs.length.toLong)
    idx.i64(allIntervals.length.toLong)
    idx.i64(blobBytes.length.toLong)
    d = 0
    while d < data.docs.length do
      val doc = data.docs(d)
      val (uriOff, uriLen) = uriOffsets(d)
      val (pOff, pCount, iFirst, iCount) = docPostingsMeta(d)
      var flags = 0
      if doc.generated then flags |= DocFlagGenerated
      if doc.readonly then flags |= DocFlagReadonly
      idx.i32(uriOff)
      idx.i32(uriLen)
      idx.i64(doc.docId)
      idx.i32(doc.epoch)
      idx.i32(doc.targetOrd)
      idx.i32(flags)
      idx.i32(iFirst)
      idx.i64(pOff)
      idx.i32(pCount)
      idx.i32(iCount)
      d += 1
    for iv <- allIntervals do
      idx.i32(iv.firstLine)
      idx.i32(iv.lastLine)
      idx.i64(iv.offset)
      idx.i32(iv.count)
      idx.i32(0)
    idx.bytes(blobBytes)
    (docPostings, idx.result(), total.toLong)

  private def buildSymbolIndex(
      data: SegmentData,
      sortedIdx: Array[Int],
      symUtf8: Array[Array[Byte]]
  ): Array[Byte] =
    val n = data.symbols.length
    val blob = new LeBuf(4096)
    val entries = new LeBuf(n * SymbolEntrySize)
    var newOrd = 0
    while newOrd < n do
      val caller = sortedIdx(newOrd)
      val s = data.symbols(caller)
      val bytes = symUtf8(caller)
      entries.i32(blob.size)
      entries.i32(bytes.length)
      entries.i64(s.symbolId)
      entries.i32(s.refGroupOrd)
      entries.i32(s.renameGroupOrd)
      entries.i32(s.defTargetOrd)
      entries.i32(0)
      blob.bytes(bytes)
      newOrd += 1
    val blobBytes = blob.result()
    val b = new LeBuf(24 + n * SymbolEntrySize + data.targets.length * 8 + blobBytes.length)
    b.i64(n.toLong)
    b.i64(data.targets.length.toLong)
    b.i64(blobBytes.length.toLong)
    b.bytes(entries.result())
    for t <- data.targets do b.i64(t)
    b.bytes(blobBytes)
    b.result()

  // --- validation ---

  private def validate(data: SegmentData): Unit =
    val docCount = data.docs.length
    val targetCount = data.targets.length
    val symbolCount = data.symbols.length
    require(
      data.defOccurrences.length == data.refOccurrences.length,
      s"defOccurrences (${data.defOccurrences.length}) and refOccurrences (${data.refOccurrences.length}) must share the ref_group_ord space"
    )
    require(
      data.renameProfiles.length == data.renameOccurrences.length,
      s"renameProfiles (${data.renameProfiles.length}) and renameOccurrences (${data.renameOccurrences.length}) must share the rename_group_ord space"
    )
    require(
      data.docOccurrences.length == docCount,
      s"docOccurrences (${data.docOccurrences.length}) must have one list per doc ($docCount)"
    )
    require(data.docs.map(_.uri).distinct.length == docCount, "doc uris must be distinct")
    for (doc, d) <- data.docs.zipWithIndex do
      require(doc.uri.nonEmpty, s"doc $d has empty uri")
      require(
        doc.targetOrd >= 0 && doc.targetOrd < targetCount,
        s"doc $d targetOrd ${doc.targetOrd} out of range [0,$targetCount)"
      )
    for (s, i) <- data.symbols.zipWithIndex do
      require(
        s.refGroupOrd >= -1 && s.refGroupOrd < data.refGroupCount,
        s"symbol $i refGroupOrd ${s.refGroupOrd} out of range"
      )
      require(
        s.renameGroupOrd >= -1 && s.renameGroupOrd < data.renameGroupCount,
        s"symbol $i renameGroupOrd ${s.renameGroupOrd} out of range"
      )
      require(
        s.defTargetOrd >= -1 && s.defTargetOrd < targetCount,
        s"symbol $i defTargetOrd ${s.defTargetOrd} out of range"
      )
    def checkGroupOcc(kind: String, g: Int, o: GroupOcc): Unit =
      require(
        o.docOrd >= 0 && o.docOrd < docCount,
        s"$kind group $g: docOrd ${o.docOrd} out of range [0,$docCount)"
      )
      require(
        o.targetOrd >= 0 && o.targetOrd < targetCount,
        s"$kind group $g: targetOrd ${o.targetOrd} out of range [0,$targetCount)"
      )
      checkSpan(s"$kind group $g", o.span)
    for (g, occs) <- data.refOccurrences.zipWithIndex.map(_.swap); o <- occs do
      checkGroupOcc("ref", g, o)
    for (g, occs) <- data.defOccurrences.zipWithIndex.map(_.swap); o <- occs do
      checkGroupOcc("definition", g, o)
    for (g, occs) <- data.renameOccurrences.zipWithIndex.map(_.swap); o <- occs do
      checkGroupOcc("rename", g, o)
    for (occs, d) <- data.docOccurrences.zipWithIndex; o <- occs do
      require(
        o.symbolOrd >= 0 && o.symbolOrd < symbolCount,
        s"doc $d: symbolOrd ${o.symbolOrd} out of range [0,$symbolCount)"
      )
      checkSpan(s"doc $d", o.span)

  private def checkSpan(where: String, span: Span): Unit =
    require(
      span.startLine >= 0 && span.startChar >= 0 && span.endLine >= 0 && span.endChar >= 0,
      s"$where: negative span coordinate $span"
    )
    require(
      Span.pack(span.startLine, span.startChar) <= Span.pack(span.endLine, span.endChar),
      s"$where: span end before start $span"
    )

  // --- io helpers ---

  private[postings] def crc32c(bytes: Array[Byte]): Long =
    val crc = new CRC32C
    crc.update(bytes)
    crc.getValue

  private def writeFileSync(path: Path, bytes: Array[Byte]): Unit =
    val ch = FileChannel.open(path, StandardOpenOption.CREATE_NEW, StandardOpenOption.WRITE)
    try
      val buf = ByteBuffer.wrap(bytes)
      while buf.hasRemaining do ch.write(buf)
      ch.force(true)
    finally ch.close()

  private def fsyncDir(dir: Path): Unit =
    val ch = FileChannel.open(dir, StandardOpenOption.READ)
    try ch.force(true)
    finally ch.close()

  private[postings] def deleteRecursively(path: Path): Unit =
    if Files.exists(path) then
      import java.util.Comparator
      val walk = Files.walk(path)
      try
        walk
          .sorted(Comparator.reverseOrder())
          .forEach(p => Files.deleteIfExists(p))
      finally walk.close()
