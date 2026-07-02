package ls.core

import ls.index.{LsError, LsException, Span, SymKind}
import ls.rename.{HighlightKind, WorkspaceEditPlan}
import org.eclipse.lsp4j.{
  DocumentHighlightKind,
  Location,
  Position,
  Range,
  SymbolKind,
  TextEdit,
  WorkspaceEdit
}

/** Pure conversions between the index model and lsp4j 1.0.0 types.
  *
  * [[Span]] and LSP [[Range]] share semantics by construction (zero-based
  * lines, UTF-16-ish characters, end-exclusive), so the mapping is direct.
  */
object LspConvert:

  def range(span: Span): Range =
    new Range(
      new Position(span.startLine, span.startChar),
      new Position(span.endLine, span.endChar)
    )

  def span(range: Range): Span =
    Span(
      range.getStart.getLine,
      range.getStart.getCharacter,
      range.getEnd.getLine,
      range.getEnd.getCharacter
    )

  def location(fileUri: String, span: Span): Location =
    new Location(fileUri, range(span))

  /** [[WorkspaceEditPlan]] (SemanticDB uris) -> LSP [[WorkspaceEdit]]
    * (`changes: Map[fileUri, TextEdit list]`). An edited uri that cannot be
    * resolved to a file fails the whole conversion — a rename must never
    * silently drop edits.
    */
  def workspaceEdit(plan: WorkspaceEditPlan, toFileUri: String => Option[String]): WorkspaceEdit =
    val changes = new java.util.LinkedHashMap[String, java.util.List[TextEdit]]
    for (sdbUri, edits) <- plan.edits.toVector.sortBy(_._1) do
      val fileUri = toFileUri(sdbUri).getOrElse(throw LsException(LsError.NotIndexed(sdbUri)))
      val list = new java.util.ArrayList[TextEdit](edits.length)
      edits.foreach(e => list.add(new TextEdit(range(e.span), e.newText)))
      changes.put(fileUri, list)
    new WorkspaceEdit(changes)

  def symbolKind(kind: SymKind): SymbolKind = kind match
    case SymKind.Class => SymbolKind.Class
    case SymKind.Trait | SymKind.Interface => SymbolKind.Interface
    case SymKind.Object | SymKind.PackageObject => SymbolKind.Object
    case SymKind.Method | SymKind.Macro => SymbolKind.Method
    case SymKind.Constructor => SymbolKind.Constructor
    case SymKind.Type => SymbolKind.Class
    case SymKind.TypeParameter => SymbolKind.TypeParameter
    case SymKind.Field => SymbolKind.Field
    case SymKind.Package => SymbolKind.Package
    case SymKind.LocalValue | SymKind.LocalVariable => SymbolKind.Variable
    case SymKind.Parameter | SymKind.SelfParameter => SymbolKind.Variable
    case SymKind.UnknownKind => SymbolKind.Null

  def highlightKind(kind: HighlightKind): DocumentHighlightKind = kind match
    case HighlightKind.Read => DocumentHighlightKind.Read
    case HighlightKind.Write => DocumentHighlightKind.Write
