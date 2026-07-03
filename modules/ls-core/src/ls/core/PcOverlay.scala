package ls.core

import scala.util.control.NonFatal

import ls.index.{Loc, Role, Span}
import ls.pc.{DefinitionOrigin, DefinitionResult}
import ls.rename.{DirtyBufferOverlay, OverlayHit}
import org.eclipse.lsp4j.Range

/** The narrow slice of the PC surface the overlay needs; tests stub this trait
  * instead of a concrete backend.
  */
trait PcSymbolQueries:
  def isOpen(fileUri: String): Boolean
  def prepareRename(fileUri: String, line: Int, character: Int): Option[Range]
  def definition(fileUri: String, line: Int, character: Int): DefinitionResult

final class FacadePcQueries(pc: PcBackend) extends PcSymbolQueries:
  def isOpen(fileUri: String): Boolean = pc.bufferText(fileUri).isDefined
  def prepareRename(fileUri: String, line: Int, character: Int): Option[Range] =
    pc.prepareRename(fileUri, line, character)
  def definition(fileUri: String, line: Int, character: Int): DefinitionResult =
    pc.definition(fileUri, line, character)

/** PC-backed [[DirtyBufferOverlay]] (PCPath of plan 10 / plan 12.1).
  *
  * All uris arriving here are SemanticDB uris; `toFileUri` (installed after
  * bootstrap) maps them onto the `file://` URIs the [[DocumentStore]] and PC
  * facade speak. Until [[install]] runs nothing is ever dirty.
  *
  * `symbolAt` is the honestly-implementable v1: mtags 1.6.7 exposes no
  * symbol-occurrence API beyond definition/prepareRename, so the SemanticDB
  * symbol string comes from PC `definition` (`DefinitionResult.symbol`) and
  * the occurrence span from PC `prepareRename` — falling back to the
  * identifier token under the cursor in the buffer text when prepareRename
  * declines (the dotty PC only offers rename ranges for file-local symbols;
  * the span is presentation-only, the symbol stays PC semantic truth). When
  * the symbol itself is unavailable the overlay answers None and the query
  * degrades to [[ls.index.LsError.StaleIndex]] — the documented plan-12.1
  * degrade, never a guess against an index that has not seen the buffer.
  * `pcOnly` is true exactly when the definition resolves only into
  * synthetic/plugin origins (plan 14.5).
  *
  * `occurrencesOf` contributes nothing in v1 (the PC has no occurrence scan
  * to back it); references over dirty buffers therefore stay index-truth
  * only, which is a permitted degrade, not an approximation.
  */
final class PcOverlay(docs: DocumentStore) extends DirtyBufferOverlay:

  private final case class Env(pc: PcSymbolQueries, toFileUri: String => Option[String])

  @volatile private var env: Option[Env] = None

  def install(pc: PcSymbolQueries, toFileUri: String => Option[String]): Unit =
    env = Some(Env(pc, toFileUri))

  def installed: Boolean = env.isDefined

  override def isDirty(sdbUri: String): Boolean =
    env.exists(e => fileUriOf(e, sdbUri).exists(docs.isDirty))

  override def symbolAt(sdbUri: String, line: Int, character: Int): Option[OverlayHit] =
    for
      e <- env
      fileUri <- fileUriOf(e, sdbUri)
      if e.pc.isOpen(fileUri)
      hit <- pcHit(e, fileUri, line, character)
    yield hit

  override def occurrencesOf(semanticSymbol: String): Option[Vector[Loc]] = None

  private def fileUriOf(e: Env, sdbUri: String): Option[String] =
    try e.toFileUri(sdbUri)
    catch case NonFatal(_) => None

  private def pcHit(e: Env, fileUri: String, line: Int, character: Int): Option[OverlayHit] =
    try
      val span = e.pc
        .prepareRename(fileUri, line, character)
        .map(LspConvert.span)
        .orElse(tokenSpanAt(fileUri, line, character))
      span.flatMap { span =>
        val defs = e.pc.definition(fileUri, line, character)
        val symbol = Option(defs.symbol).getOrElse("")
        if symbol.isEmpty then None
        else
          val isDefinition = defs.locations.exists { dl =>
            dl.origin == DefinitionOrigin.Workspace &&
            dl.location.getUri == fileUri &&
            LspConvert.span(dl.location.getRange) == span
          }
          val pcOnly =
            defs.locations.nonEmpty &&
              defs.locations.forall(_.origin != DefinitionOrigin.Workspace)
          Some(
            OverlayHit(
              semanticSymbol = symbol,
              span = span,
              role = if isDefinition then Role.Definition else Role.Reference,
              pcOnly = pcOnly
            )
          )
      }
    catch case NonFatal(_) => None

  /** The identifier token covering the cursor in the OPEN BUFFER text (the
    * overlay only ever runs on dirty buffers, so the buffer is the only
    * honest text source). None when the cursor is not on an identifier.
    */
  private def tokenSpanAt(fileUri: String, line: Int, character: Int): Option[Span] =
    def idPart(c: Char): Boolean = c == '$' || Character.isUnicodeIdentifierPart(c)
    for
      text <- docs.text(fileUri)
      lineText <- text.linesIterator.drop(line).nextOption()
      span <- {
        val n = lineText.length
        var start = math.min(math.max(character, 0), n)
        var end = start
        while start > 0 && idPart(lineText.charAt(start - 1)) do start -= 1
        while end < n && idPart(lineText.charAt(end)) do end += 1
        if end > start then Some(Span(line, start, line, end)) else None
      }
    yield span
