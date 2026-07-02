package ls.postings

/** Binary segment format v1 constants. The normative description lives in
  * docs/index-format.md; this object is the single in-code source of truth
  * for magic numbers, file names and record widths.
  *
  * All multi-byte values are little-endian. v1 simplification: one segment is
  * one complete index generation (the snapshot reads exactly one active
  * segment), but every occurrence record still carries doc_epoch and readers
  * still epoch-filter, so multi-segment layering can be added later without a
  * format change.
  */
object SegmentFormat:
  /** Bytes 'L','S','P','G' in file order (uint32 LE value 0x4750534C). */
  inline val Magic = 0x4750534c
  inline val Version = 1

  /** Occurrences per skip block in group postings and per interval block in
    * doc postings.
    */
  inline val BlockSize = 256

  inline val HeaderSize = 64
  inline val GroupIndexEntrySize = 16
  inline val RenameProfileEntrySize = 16
  inline val DocEntrySize = 48
  inline val IntervalEntrySize = 24
  inline val SymbolEntrySize = 32
  /** Fixed prefix of one block-index entry, before the target bitset words. */
  inline val BlockEntryFixedSize = 40

  val HeaderFile = "header.bin"
  val RefGroupIndexFile = "ref-group-index.bin"
  val DefinitionGroupIndexFile = "definition-group-index.bin"
  val RenameGroupIndexFile = "rename-group-index.bin"
  val DocIndexFile = "doc-index.bin"
  val SymbolIndexFile = "symbol-index.bin"
  val RefPostingsFile = "ref-postings.bin"
  val DefinitionPostingsFile = "definition-postings.bin"
  val RenamePostingsFile = "rename-postings.bin"
  val DocPostingsFile = "doc-postings.bin"
  val BlockIndexFile = "block-index.bin"
  val ChecksumsFile = "checksums.bin"

  /** Every file covered by checksums.bin, in the canonical order in which
    * checksum entries are written.
    */
  val ChecksummedFiles: List[String] = List(
    HeaderFile,
    RefGroupIndexFile,
    DefinitionGroupIndexFile,
    RenameGroupIndexFile,
    DocIndexFile,
    SymbolIndexFile,
    RefPostingsFile,
    DefinitionPostingsFile,
    RenamePostingsFile,
    DocPostingsFile,
    BlockIndexFile
  )

  /** Doc dictionary flag bits (doc-index.bin DocEntry.doc_flags). */
  inline val DocFlagGenerated = 1 << 0
  inline val DocFlagReadonly = 1 << 1

  /** Rename profile flag bits (rename-group-index.bin profile_flags). */
  inline val ProfIsLocal = 1 << 0
  inline val ProfIsExternal = 1 << 1
  inline val ProfHasGenerated = 1 << 2
  inline val ProfHasReadonly = 1 << 3
  inline val ProfHasOverrideFamily = 1 << 4
  inline val ProfHasCompanion = 1 << 5

  def segmentDirName(segmentId: Long): String = f"segment-$segmentId%06d"

  /** Number of int64 bitset words per block entry for a given target count.
    * Always at least one word so block entries never degenerate to width 0.
    */
  def targetWordCount(targetCount: Int): Int =
    math.max(1, (targetCount + 63) >>> 6)

/** A segment failed validation at open (bad magic/version, checksum mismatch,
  * structurally inconsistent counts or sizes). Corrupt segments are rejected;
  * they are never partially served.
  */
final class SegmentCorruptedException(message: String, cause: Throwable | Null = null)
    extends RuntimeException(message, cause)
