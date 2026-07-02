package ls.core

import scala.jdk.CollectionConverters.*

import ls.index.{LsException, Span, SymKind}
import ls.rename.{TextEditSpan, WorkspaceEditPlan}
import org.eclipse.lsp4j.SymbolKind

class LspConvertSuite extends munit.FunSuite:

  test("span <-> range conversion is exact"):
    val span = Span(3, 4, 3, 9)
    val range = LspConvert.range(span)
    assertEquals(range.getStart.getLine, 3)
    assertEquals(range.getStart.getCharacter, 4)
    assertEquals(range.getEnd.getLine, 3)
    assertEquals(range.getEnd.getCharacter, 9)
    assertEquals(LspConvert.span(range), span)

  test("WorkspaceEditPlan -> WorkspaceEdit: multi-file changes with correct ranges"):
    val plan = WorkspaceEditPlan(
      edits = Map(
        "a/src/Core.scala" -> Vector(
          TextEditSpan(Span(2, 6, 2, 10), "renamed"),
          TextEditSpan(Span(5, 2, 5, 6), "renamed")
        ),
        "b/src/Use.scala" -> Vector(TextEditSpan(Span(0, 12, 0, 16), "renamed"))
      ),
      occurrenceCount = 3
    )
    val toFileUri: String => Option[String] = sdb => Some(s"file:///ws/$sdb")
    val edit = LspConvert.workspaceEdit(plan, toFileUri)

    val changes = edit.getChanges
    assertEquals(changes.keySet.asScala.toSet, Set("file:///ws/a/src/Core.scala", "file:///ws/b/src/Use.scala"))

    val coreEdits = changes.get("file:///ws/a/src/Core.scala").asScala.toVector
    assertEquals(coreEdits.length, 2)
    assertEquals(coreEdits.map(_.getNewText).toSet, Set("renamed"))
    assertEquals(LspConvert.span(coreEdits.head.getRange), Span(2, 6, 2, 10))
    assertEquals(LspConvert.span(coreEdits(1).getRange), Span(5, 2, 5, 6))

    val useEdits = changes.get("file:///ws/b/src/Use.scala").asScala.toVector
    assertEquals(useEdits.length, 1)
    assertEquals(LspConvert.span(useEdits.head.getRange), Span(0, 12, 0, 16))

  test("WorkspaceEditPlan conversion fails loudly when a uri cannot be resolved"):
    val plan = WorkspaceEditPlan(
      edits = Map("ghost/File.scala" -> Vector(TextEditSpan(Span(0, 0, 0, 1), "x"))),
      occurrenceCount = 1
    )
    intercept[LsException](LspConvert.workspaceEdit(plan, _ => None))

  test("SymKind -> lsp4j SymbolKind mapping covers every kind"):
    assertEquals(LspConvert.symbolKind(SymKind.Class), SymbolKind.Class)
    assertEquals(LspConvert.symbolKind(SymKind.Trait), SymbolKind.Interface)
    assertEquals(LspConvert.symbolKind(SymKind.Object), SymbolKind.Object)
    assertEquals(LspConvert.symbolKind(SymKind.Method), SymbolKind.Method)
    assertEquals(LspConvert.symbolKind(SymKind.Field), SymbolKind.Field)
    assertEquals(LspConvert.symbolKind(SymKind.Package), SymbolKind.Package)
    // total: no kind throws
    SymKind.values.foreach(k => LspConvert.symbolKind(k))
