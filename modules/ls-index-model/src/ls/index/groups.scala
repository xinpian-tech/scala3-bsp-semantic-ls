package ls.index

/** Reasons a rename group is rejected. Stored as a bitmask so the request
  * path is one integer test; expanded to messages only on rejection.
  */
object UnsafeReason:
  inline val External = 1L << 0
  inline val GeneratedOccurrence = 1L << 1
  inline val ReadonlyOccurrence = 1L << 2
  inline val OverrideFamily = 1L << 3
  inline val SyntheticOnly = 1L << 4
  inline val PcOnly = 1L << 5
  inline val SharedSourceDisagreement = 1L << 6
  inline val UnsupportedSymbolFamily = 1L << 7
  inline val DependencySource = 1L << 8

  def explain(mask: Long): List[String] =
    val msgs = List.newBuilder[String]
    if (mask & External) != 0 then
      msgs += "symbol is defined outside the workspace"
    if (mask & GeneratedOccurrence) != 0 then
      msgs += "symbol has occurrences in generated sources"
    if (mask & ReadonlyOccurrence) != 0 then
      msgs += "symbol has occurrences in readonly sources"
    if (mask & OverrideFamily) != 0 then
      msgs += "symbol participates in an override family that cannot be renamed safely"
    if (mask & SyntheticOnly) != 0 then
      msgs += "symbol only has synthetic occurrences"
    if (mask & PcOnly) != 0 then
      msgs += "symbol is provided by a PC-only plugin and is not present in fresh SemanticDB"
    if (mask & SharedSourceDisagreement) != 0 then
      msgs += "targets sharing this source disagree on the rename group"
    if (mask & UnsupportedSymbolFamily) != 0 then
      msgs += "symbol family (e.g. apply/unapply, exported symbol) is not safely renameable"
    if (mask & DependencySource) != 0 then
      msgs += "symbol has occurrences in dependency sources"
    msgs.result()

/** Precomputed at ingest; consulted at rename request time. */
final case class RenameProfile(
    isLocal: Boolean,
    isExternal: Boolean,
    hasGeneratedOccurrences: Boolean,
    hasReadonlyOccurrences: Boolean,
    hasOverrideFamily: Boolean,
    hasCompanion: Boolean,
    editableOccurrenceCount: Int,
    unsafeReasonMask: Long
):
  def isSafe: Boolean = unsafeReasonMask == 0L

object RenameProfile:
  val empty: RenameProfile =
    RenameProfile(false, false, false, false, false, false, 0, 0L)
