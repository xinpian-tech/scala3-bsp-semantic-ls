package ls.rename

import scala.collection.mutable.ArrayBuffer
import scala.concurrent.duration.Duration

import ch.epfl.scala.bsp4j.StatusCode

import ls.bsp.BspCompileOutcome
import ls.index.*

final class StubCompiler(outcome: BspCompileOutcome = BspCompileOutcome.Ok(None))
    extends CompileService:
  val calls: ArrayBuffer[Seq[String]] = ArrayBuffer.empty
  override def compile(targets: Seq[String]): BspCompileOutcome =
    calls += targets
    outcome

/** Rename engine coverage over the non-mutating fixture: happy paths with
  * exact edit spans and every rejection that does not require touching files.
  */
class RenameSuite extends munit.FunSuite:

  override def munitTimeout: Duration = Duration(600, "s")

  private lazy val fx = FixtureWorkspace.master
  private lazy val stack = FixtureWorkspace.newStack()
  private lazy val compiler = StubCompiler()
  private lazy val engine = RenameEngine(stack.orchestrator, compiler)

  override def beforeAll(): Unit =
    stack.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))

  override def afterAll(): Unit = stack.close()

  private def rename(uri: String, token: String, nth: Int, newName: String): WorkspaceEditPlan =
    val (line, ch) = fx.cursor(uri, token, nth)
    engine.rename(uri, line, ch, newName)

  private def rejection(uri: String, token: String, nth: Int, newName: String): LsError =
    intercept[LsException](rename(uri, token, nth, newName)).error

  private def spanSet(plan: WorkspaceEditPlan, uri: String): Set[Span] =
    plan.edits.getOrElse(uri, Vector.empty).map(_.span).toSet

  // ------------------------------------------------------------ happy paths

  test("rename case class: every Item token across files and targets, never the .apply token"):
    val plan = rename("a/src/pkga/Item.scala", "Item", 0, "Thing")
    assertEquals(
      plan.edits.keySet,
      Set("a/src/pkga/Item.scala", "b/src/pkgb/UseB.scala"),
      plan.toString
    )
    assertEquals(
      spanSet(plan, "a/src/pkga/Item.scala"),
      fx.tokenSpans("a/src/pkga/Item.scala", "Item").toSet,
      "definition + Item(1) + Item.apply(2)'s receiver + new Item(3)"
    )
    assertEquals(
      spanSet(plan, "b/src/pkgb/UseB.scala"),
      fx.tokenSpans("b/src/pkgb/UseB.scala", "Item").toSet
    )
    val applySpan = fx.tokenSpan("a/src/pkga/Item.scala", "apply", 0)
    assert(!spanSet(plan, "a/src/pkga/Item.scala").contains(applySpan), "explicit .apply token")
    assert(plan.edits.values.flatten.forall(_.newText == "Thing"))
    assertEquals(plan.occurrenceCount, plan.edits.values.map(_.length).sum)

  test("rename compiles exactly the definition target's reverse dependency closure"):
    compiler.calls.clear()
    rename("a/src/pkga/Item.scala", "Item", 0, "Thing")
    assertEquals(compiler.calls.toList, List(Vector("fixture-a", "fixture-b")))

  test("rename method defined in a shared source (targets agree)"):
    val plan = rename("shared/src/shared/Shared.scala", "tag", 0, "label")
    assertEquals(
      spanSet(plan, "shared/src/shared/Shared.scala"),
      fx.tokenSpans("shared/src/shared/Shared.scala", "tag").toSet
    )
    assertEquals(
      spanSet(plan, "b/src/pkgb/UseB.scala"),
      fx.tokenSpans("b/src/pkgb/UseB.scala", "tag").toSet
    )
    assertEquals(plan.edits.keySet, Set("shared/src/shared/Shared.scala", "b/src/pkgb/UseB.scala"))

  test("rename var renames getter, setter site and definition together"):
    val plan = rename("a/src/pkga/Vars.scala", "value", 0, "count")
    assertEquals(
      spanSet(plan, "a/src/pkga/Vars.scala"),
      fx.tokenSpans("a/src/pkga/Vars.scala", "value").toSet,
      plan.toString
    )
    assertEquals(plan.edits.keySet, Set("a/src/pkga/Vars.scala"))

  test("rename local val touches only its document"):
    val plan = rename("a/src/pkga/Vars.scala", "tmp", 0, "next")
    assertEquals(
      spanSet(plan, "a/src/pkga/Vars.scala"),
      fx.tokenSpans("a/src/pkga/Vars.scala", "tmp").toSet
    )
    assertEquals(plan.edits.keySet, Set("a/src/pkga/Vars.scala"))

  test("rename one method overload leaves the other alone"):
    val plan = rename("a/src/pkga/Over.scala", "fmt", 0, "fmtInt")
    val spans = fx.tokenSpans("a/src/pkga/Over.scala", "fmt")
    assertEquals(spanSet(plan, "a/src/pkga/Over.scala"), Set(spans(0), spans(2)), plan.toString)

  test("keyword new name is backtick-quoted"):
    val plan = rename("a/src/pkga/Vars.scala", "tmp", 0, "type")
    assert(plan.edits("a/src/pkga/Vars.scala").forall(_.newText == "`type`"), plan.toString)

  // ------------------------------------------------------------ rejections

  test("compile failure rejects the rename"):
    val failing = RenameEngine(stack.orchestrator, StubCompiler(BspCompileOutcome.Failed(StatusCode.ERROR, None)))
    val (line, ch) = fx.cursor("a/src/pkga/Item.scala", "Item", 0)
    val err = intercept[LsException](failing.rename("a/src/pkga/Item.scala", line, ch, "Thing"))
    assert(err.error.isInstanceOf[LsError.CompileFailed], err.error.toString)

  test("invalid new identifier is rejected before compiling"):
    compiler.calls.clear()
    val err1 = rejection("a/src/pkga/Item.scala", "Item", 0, "has`tick")
    assert(err1.isInstanceOf[LsError.RenameRejected], err1.toString)
    val err2 = rejection("a/src/pkga/Item.scala", "Item", 0, "")
    assert(err2.isInstanceOf[LsError.RenameRejected], err2.toString)
    assertEquals(compiler.calls.toList, Nil)

  test("no symbol at cursor is rejected before compiling"):
    compiler.calls.clear()
    val err = intercept[LsException](engine.rename("a/src/pkga/Item.scala", 1, 0, "Thing"))
    assert(err.error.isInstanceOf[LsError.NoSymbolAtCursor], err.error.toString)
    assertEquals(compiler.calls.toList, Nil)

  test("override family is rejected"):
    val err = rejection("a/src/pkga/Core.scala", "greet", 0, "salute")
    err match
      case LsError.RenameRejected(reasons) =>
        assert(reasons.exists(_.contains("override")), reasons.toString)
      case other => fail(s"expected RenameRejected, got $other")

  test("rename of an exported symbol is rejected with the exported-symbol reason"):
    // cursor on the `exported` definition (nth=0 whole-word occurrence); it is
    // re-exported via `export OriginalOwner.exported`, so it must reject rather
    // than partially edit and miss the forwarder.
    val err = rejection("a/src/pkga/Exported.scala", "exported", 0, "renamed")
    err match
      case LsError.RenameRejected(reasons) =>
        assert(reasons.exists(_.contains("exported symbol")), reasons.toString)
      case other => fail(s"expected RenameRejected, got $other")

  test("occurrences in generated sources are rejected"):
    val err = rejection("a/src/pkga/Widget.scala", "Widget", 0, "Gizmo")
    err match
      case LsError.RenameRejected(reasons) =>
        assert(reasons.exists(_.contains("generated")), reasons.toString)
      case other => fail(s"expected RenameRejected, got $other")

  test("occurrences in readonly sources are rejected"):
    val err = rejection("a/src/pkga/Gadget.scala", "Gadget", 0, "Gizmo")
    err match
      case LsError.RenameRejected(reasons) =>
        assert(reasons.exists(_.contains("readonly")), reasons.toString)
      case other => fail(s"expected RenameRejected, got $other")

  test("cursor inside a readonly document is rejected before compiling"):
    compiler.calls.clear()
    val err = rejection("a/src/pkga/ReadonlyUse.scala", "Gadget", 0, "Gizmo")
    assert(err.isInstanceOf[LsError.RenameRejected], err.toString)
    assertEquals(compiler.calls.toList, Nil)

  test("dependency sources are rejected"):
    val err = rejection("dep/src/pkgdep/DepThing.scala", "DepThing", 0, "Other")
    err match
      case LsError.RenameRejected(reasons) =>
        assert(reasons.exists(_.contains("dependency")), reasons.toString)
      case other => fail(s"expected RenameRejected, got $other")

  test("PC-only symbols are rejected"):
    val overlay = new DirtyBufferOverlay:
      override def isDirty(uri: String): Boolean = uri == "a/src/pkga/Item.scala"
      override def symbolAt(uri: String, line: Int, character: Int): Option[OverlayHit] =
        Some(OverlayHit("pcplugin/Synthetic#", Span(0, 0, 0, 4), Role.Reference, pcOnly = true))
      override def occurrencesOf(semanticSymbol: String): Option[Vector[Loc]] = None
    val stack2 = FixtureWorkspace.newStack(overlay)
    try
      stack2.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      val engine2 = RenameEngine(stack2.orchestrator, StubCompiler())
      val err = intercept[LsException](engine2.rename("a/src/pkga/Item.scala", 2, 12, "Thing"))
      assert(err.error.isInstanceOf[LsError.PcOnlySymbol], err.error.toString)
    finally stack2.close()

  test("dirty buffers (unsaved changes) are rejected"):
    val overlay = new DirtyBufferOverlay:
      override def isDirty(uri: String): Boolean = uri == "a/src/pkga/Item.scala"
      override def symbolAt(uri: String, line: Int, character: Int): Option[OverlayHit] =
        Some(OverlayHit("pkga/Item#", Span(2, 11, 2, 15), Role.Definition))
      override def occurrencesOf(semanticSymbol: String): Option[Vector[Loc]] = None
    val stack2 = FixtureWorkspace.newStack(overlay)
    try
      stack2.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      val engine2 = RenameEngine(stack2.orchestrator, StubCompiler())
      val err = intercept[LsException](engine2.rename("a/src/pkga/Item.scala", 2, 12, "Thing"))
      err.error match
        case LsError.RenameRejected(reasons) =>
          assert(reasons.exists(_.contains("unsaved")), reasons.toString)
        case other => fail(s"expected RenameRejected, got $other")
    finally stack2.close()

  test("prepareRename returns the occurrence span"):
    val span = fx.tokenSpan("a/src/pkga/Item.scala", "Item", 0)
    val (line, ch) = fx.cursor("a/src/pkga/Item.scala", "Item", 0)
    assertEquals(engine.prepareRename("a/src/pkga/Item.scala", line, ch), span)
