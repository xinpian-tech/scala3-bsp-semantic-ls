package ls.postings

import ls.index.{RenameProfile, Span}

/** One document row of the segment doc dictionary.
  *
  * `targetOrd` indexes into [[SegmentData.targets]]. `epoch` is the current
  * ingest epoch of the document; occurrence records carry their own
  * `docEpoch` and readers drop records whose epoch does not match this value
  * (plan 9.3).
  */
final case class SegmentDoc(
    uri: String,
    docId: Long,
    epoch: Int,
    targetOrd: Int,
    generated: Boolean = false,
    readonly: Boolean = false
)

/** One symbol row of the segment symbol dictionary, in caller order. The
  * writer sorts rows by the UTF-8 bytes of `semanticSymbol` to enable binary
  * search, and remaps [[DocOcc.symbolOrd]] accordingly; callers never need to
  * pre-sort.
  *
  * Group ordinals and `defTargetOrd` use -1 for "none/unknown".
  */
final case class SegmentSymbol(
    semanticSymbol: String,
    symbolId: Long,
    refGroupOrd: Int = -1,
    renameGroupOrd: Int = -1,
    defTargetOrd: Int = -1
)

/** One occurrence in a group postings list (ref / definition / rename).
  *
  * `docEpoch` is stored verbatim; a reader only surfaces the occurrence when
  * it equals the epoch of `docOrd` in the doc dictionary. `flags` uses
  * [[ls.index.OccFlags]] bits.
  */
final case class GroupOcc(
    docOrd: Int,
    docEpoch: Int,
    targetOrd: Int,
    span: Span,
    flags: Int
)

/** One occurrence in a doc postings list. `symbolOrd` indexes into
  * [[SegmentData.symbols]] in caller order (the writer remaps it to the
  * sorted on-disk ordinal). `flags` carries the role bit
  * ([[ls.index.OccFlags.Definition]]) plus any other exact facts.
  */
final case class DocOcc(
    symbolOrd: Int,
    span: Span,
    flags: Int
)

/** Plain in-memory build model consumed by [[SegmentWriter]]. Wave-2 ingest
  * constructs it from SQLite state; it has no dependency beyond
  * ls-index-model.
  *
  * Indexing conventions:
  *   - `docs(i)` is doc_ord i; `targets(i)` is the persistent target id of
  *     target_ord i; `symbols(i)` is caller symbol ordinal i (re-sorted on
  *     disk).
  *   - `refOccurrences(g)` and `defOccurrences(g)` share the ref_group_ord
  *     space and must have the same length.
  *   - `renameOccurrences(g)` and `renameProfiles(g)` share the
  *     rename_group_ord space and must have the same length.
  *   - `docOccurrences(d)` lists all occurrences of doc_ord d.
  *
  * Occurrence lists need not be pre-sorted; the writer sorts group lists by
  * (doc_ord, packed_start, packed_end) and doc lists by (packed_start,
  * packed_end).
  */
final case class SegmentData(
    docs: Vector[SegmentDoc],
    targets: Vector[Long],
    symbols: Vector[SegmentSymbol],
    refOccurrences: Vector[Vector[GroupOcc]],
    defOccurrences: Vector[Vector[GroupOcc]],
    renameOccurrences: Vector[Vector[GroupOcc]],
    renameProfiles: Vector[RenameProfile],
    docOccurrences: Vector[Vector[DocOcc]]
):
  def refGroupCount: Int = refOccurrences.length
  def renameGroupCount: Int = renameOccurrences.length
  def occurrenceCount: Long =
    var n = 0L
    refOccurrences.foreach(n += _.length)
    defOccurrences.foreach(n += _.length)
    renameOccurrences.foreach(n += _.length)
    docOccurrences.foreach(n += _.length)
    n
