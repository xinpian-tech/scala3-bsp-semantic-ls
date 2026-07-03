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

  private final case class Env(
      pc: PcSymbolQueries,
      toFileUri: String => Option[String],
      /** True when a display name is present in the persisted index. */
      isIndexedName: String => Boolean
  )

  @volatile private var env: Option[Env] = None

  def install(
      pc: PcSymbolQueries,
      toFileUri: String => Option[String],
      isIndexedName: String => Boolean
  ): Unit =
    env = Some(Env(pc, toFileUri, isIndexedName))

  def installed: Boolean = env.isDefined

  override def isDirty(sdbUri: String): Boolean =
    env.exists(e => fileUriOf(e, sdbUri).exists(docs.isDirty))

  override def symbolAt(sdbUri: String, line: Int, character: Int): Option[OverlayHit] =
    for
      e <- env
      fileUri <- fileUriOf(e, sdbUri)
      if e.pc.isOpen(fileUri)
      // A top-level declaration in the dirty buffer whose name the persisted
      // index has never seen is PC-only: global references and
      // rename refuse it. Otherwise fall back to the PC symbol resolution.
      hit <- pcOnlyTopLevelHit(e, fileUri, line, character).orElse(pcHit(e, fileUri, line, character))
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

  private def isIndexed(e: Env, name: String): Boolean =
    // Fail safe: if the index membership check throws, treat the symbol as
    // indexed so a query error never spuriously refuses references/rename.
    try e.isIndexedName(name)
    catch case NonFatal(_) => true

  /** A PC-only overlay hit when the cursor sits on the name of a top-level
    * declaration in the dirty buffer that the persisted index has never seen.
    * The synthetic symbol string is never used: `pcOnly` short-circuits the
    * engines before they read it.
    */
  private def pcOnlyTopLevelHit(
      e: Env,
      fileUri: String,
      line: Int,
      character: Int
  ): Option[OverlayHit] =
    docs.text(fileUri).flatMap { text =>
      PcOverlay
        .topLevelDecls(text)
        .find(d => d.span.startLine == line && character >= d.span.startChar && character <= d.span.endChar)
        .filter(d => !isIndexed(e, d.name))
        .map(d =>
          OverlayHit(
            semanticSymbol = s"local/${d.name}#",
            span = d.span,
            role = Role.Definition,
            pcOnly = true
          )
        )
    }

  /** Top-level symbols declared in open, unsaved buffers whose names the
    * persisted index has never seen, matched (case-insensitive substring)
    * against the workspace/symbol query. These are surfaced by
    * `workspace/symbol` flagged PC-only.
    */
  def pcOnlySymbols(query: String): Vector[PcOnlySymbol] =
    env match
      case None => Vector.empty
      case Some(e) =>
        val q = query.toLowerCase
        docs.openUris.flatMap { fileUri =>
          if !docs.isDirty(fileUri) then Vector.empty[PcOnlySymbol]
          else
            docs.text(fileUri).toVector.flatMap { text =>
              PcOverlay
                .topLevelDecls(text)
                .filter(d => q.isEmpty || d.name.toLowerCase.contains(q))
                .filter(d => !isIndexed(e, d.name))
                .map(d => PcOnlySymbol(d.name, d.keyword, fileUri, d.span))
            }
        }

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

object PcOverlay:

  /** A top-level declaration `keyword Name` at column 0 (optionally preceded by
    * modifiers): the span covers the name token only.
    */
  private final case class TopLevelDecl(name: String, keyword: String, span: Span)

  private val Modifiers = "private|protected|final|sealed|abstract|case|open|implicit|lazy|override|inline|transparent"
  private val Keyword = "object|class|trait|enum|def|val|var|type"
  private val TopLevelDeclRe =
    s"^(?:(?:$Modifiers)\\s+)*($Keyword)\\s+([A-Za-z_][A-Za-z0-9_$$]*)".r

  /** Top-level declarations in a buffer: the declaration keyword sits at column
    * 0 (top-level members in Scala 3; nested members are indented). A light
    * scan — no PC round-trip — is enough to surface unsaved symbols.
    */
  private def topLevelDecls(text: String): Vector[TopLevelDecl] =
    text.linesIterator.zipWithIndex.flatMap { (lineText, ln) =>
      TopLevelDeclRe.findFirstMatchIn(lineText).map { m =>
        TopLevelDecl(name = m.group(2), keyword = m.group(1), span = Span(ln, m.start(2), ln, m.end(2)))
      }
    }.toVector

/** A top-level symbol that exists only in an open, unsaved buffer (not in the
  * persisted index): surfaced by `workspace/symbol` flagged PC-only, and
  * excluded from global references/rename.
  */
final case class PcOnlySymbol(name: String, keyword: String, fileUri: String, span: Span)
