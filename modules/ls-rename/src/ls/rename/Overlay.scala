package ls.rename

import ls.index.{Loc, Role, Span}

/** Symbol-at-cursor answer from a dirty-buffer overlay. `semanticSymbol` is
  * the raw SemanticDB symbol string; `pcOnly` marks symbols that only exist
  * in PC-plugin synthetic sources / overlays and are therefore excluded from
  * workspace references truth and rejected for rename (plan 14.5).
  */
final case class OverlayHit(
    semanticSymbol: String,
    span: Span,
    role: Role,
    pcOnly: Boolean = false
)

/** SPI for the presentation-compiler dirty-buffer overlay (plan 12.1 / PCPath
  * in plan 10). The real PC-backed implementation lives in the LSP core
  * module; this module only consumes the hooks:
  *
  *   - a *dirty* uri (open buffer differs from disk) makes the overlay the
  *     only trusted source for symbol-at-cursor in that file;
  *   - `occurrencesOf` contributes extra dirty-buffer occurrences to
  *     references results (never to rename, which is FreshRequired).
  *
  * Overlay data is never written to SQLite or postings.
  */
trait DirtyBufferOverlay:
  /** True when the open editor buffer for `uri` differs from disk. */
  def isDirty(uri: String): Boolean

  /** Symbol at cursor inside a dirty buffer; None when the overlay cannot
    * answer (the query then degrades instead of using the stale index).
    */
  def symbolAt(uri: String, line: Int, character: Int): Option[OverlayHit]

  /** Occurrences of `semanticSymbol` contributed by dirty buffers, or None
    * when the overlay has nothing to add.
    */
  def occurrencesOf(semanticSymbol: String): Option[Vector[Loc]]

  /** True when [[occurrencesOf]] can contribute occurrences at all. When false
    * (the default, and the production PC overlay), references skip the per-group
    * overlay fan-out entirely. An overlay that returns real occurrences must
    * override this to `true` so the group-keyed query is exercised.
    */
  def contributesOccurrences: Boolean = false

/** Overlay used until the PC worker is wired in: nothing is ever dirty. */
object NoopOverlay extends DirtyBufferOverlay:
  override def isDirty(uri: String): Boolean = false
  override def symbolAt(uri: String, line: Int, character: Int): Option[OverlayHit] = None
  override def occurrencesOf(semanticSymbol: String): Option[Vector[Loc]] = None
