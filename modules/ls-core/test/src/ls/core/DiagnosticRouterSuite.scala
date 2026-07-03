package ls.core

import scala.collection.mutable
import scala.jdk.CollectionConverters.*

import ch.epfl.scala.bsp4j.{
  BuildTargetIdentifier,
  Diagnostic as BspDiagnostic,
  DiagnosticSeverity as BspSeverity,
  Position as BspPosition,
  PublishDiagnosticsParams as BspPublish,
  Range as BspRange,
  TextDocumentIdentifier as BspTextDoc
}
import org.eclipse.lsp4j.{DiagnosticSeverity as LspSeverity, PublishDiagnosticsParams as LspPublish}
import org.eclipse.lsp4j.jsonrpc.messages.Either as JsonEither

class DiagnosticRouterSuite extends munit.FunSuite:

  private def diag(
      msg: String,
      sev: BspSeverity,
      sl: Int = 1,
      sc: Int = 2,
      el: Int = 1,
      ec: Int = 7
  ): BspDiagnostic =
    val d = new BspDiagnostic(new BspRange(new BspPosition(sl, sc), new BspPosition(el, ec)), msg)
    d.setSeverity(sev)
    d

  private def publish(uri: String, target: String, reset: Boolean, diags: BspDiagnostic*): BspPublish =
    new BspPublish(
      new BspTextDoc(uri),
      new BuildTargetIdentifier(target),
      diags.toList.asJava,
      reset
    )

  private final class Sink:
    val out: mutable.ArrayBuffer[LspPublish] = mutable.ArrayBuffer.empty
    val fn: LspPublish => Unit = p => out += p
    def messagesFor(uri: String): List[String] =
      out.filter(_.getUri == uri).lastOption.toList.flatMap(_.getDiagnostics.asScala.map(_.getMessage.getLeft))

  test("single publish reaches the sink with converted content"):
    val sink = new Sink
    val router = new DiagnosticRouter(sink.fn)
    router.accept(publish("file:///a.scala", "t/a", reset = true, diag("boom", BspSeverity.ERROR)))
    assertEquals(sink.out.length, 1)
    val p = sink.out.head
    assertEquals(p.getUri, "file:///a.scala")
    assertEquals(p.getDiagnostics.size, 1)
    val d = p.getDiagnostics.get(0)
    assertEquals(d.getMessage.getLeft, "boom")
    assertEquals(d.getSeverity, LspSeverity.Error)
    assertEquals(d.getRange.getStart.getLine, 1)
    assertEquals(d.getRange.getStart.getCharacter, 2)
    assertEquals(d.getRange.getEnd.getCharacter, 7)

  test("two targets on one uri merge into one publish"):
    val sink = new Sink
    val router = new DiagnosticRouter(sink.fn)
    router.accept(publish("file:///a.scala", "t/a", reset = true, diag("from-a", BspSeverity.ERROR)))
    router.accept(publish("file:///a.scala", "t/b", reset = true, diag("from-b", BspSeverity.WARNING)))
    assertEquals(sink.messagesFor("file:///a.scala").toSet, Set("from-a", "from-b"))

  test("reset replaces a target's diagnostics"):
    val sink = new Sink
    val router = new DiagnosticRouter(sink.fn)
    router.accept(publish("file:///a.scala", "t/a", reset = true, diag("old", BspSeverity.ERROR)))
    router.accept(publish("file:///a.scala", "t/a", reset = true, diag("new", BspSeverity.ERROR)))
    assertEquals(sink.messagesFor("file:///a.scala"), List("new"))

  test("clearing one target does not clear a sibling target on the same uri"):
    val sink = new Sink
    val router = new DiagnosticRouter(sink.fn)
    router.accept(publish("file:///a.scala", "t/a", reset = true, diag("from-a", BspSeverity.ERROR)))
    router.accept(publish("file:///a.scala", "t/b", reset = true, diag("from-b", BspSeverity.WARNING)))
    router.accept(publish("file:///a.scala", "t/a", reset = true)) // clear A
    assertEquals(sink.messagesFor("file:///a.scala"), List("from-b"))

  test("clearing the only target publishes an empty list"):
    val sink = new Sink
    val router = new DiagnosticRouter(sink.fn)
    router.accept(publish("file:///a.scala", "t/a", reset = true, diag("boom", BspSeverity.ERROR)))
    router.accept(publish("file:///a.scala", "t/a", reset = true)) // clear
    assert(sink.out.last.getDiagnostics.isEmpty, "expected empty clearing publish")

  test("empty publish for an already-clean uri does not publish"):
    val sink = new Sink
    val router = new DiagnosticRouter(sink.fn)
    router.accept(publish("file:///a.scala", "t/a", reset = true)) // nothing ever published
    assertEquals(sink.out.length, 0)

  test("non-reset diagnostics accumulate for a target"):
    val sink = new Sink
    val router = new DiagnosticRouter(sink.fn)
    router.accept(publish("file:///a.scala", "t/a", reset = true, diag("one", BspSeverity.ERROR)))
    router.accept(publish("file:///a.scala", "t/a", reset = false, diag("two", BspSeverity.ERROR)))
    assertEquals(sink.messagesFor("file:///a.scala"), List("one", "two"))

  test("toFileUri hook is applied to the published uri"):
    val sink = new Sink
    val router = new DiagnosticRouter(sink.fn, toFileUri = _ => "file:///mapped.scala")
    router.accept(publish("bsp:///whatever", "t/a", reset = true, diag("x", BspSeverity.ERROR)))
    assertEquals(sink.out.head.getUri, "file:///mapped.scala")

class LspConvertDiagnosticSuite extends munit.FunSuite:

  private def bspDiag(sl: Int, sc: Int, el: Int, ec: Int, msg: String): BspDiagnostic =
    new BspDiagnostic(new BspRange(new BspPosition(sl, sc), new BspPosition(el, ec)), msg)

  test("converts range, message, severity and code"):
    val d = bspDiag(3, 4, 5, 6, "hi")
    d.setSeverity(BspSeverity.WARNING)
    d.setCode(JsonEither.forLeft[String, Integer]("E123"))
    val out = LspConvert.diagnostic(d)
    assertEquals(out.getMessage.getLeft, "hi")
    assertEquals(out.getRange.getStart.getLine, 3)
    assertEquals(out.getRange.getStart.getCharacter, 4)
    assertEquals(out.getRange.getEnd.getLine, 5)
    assertEquals(out.getRange.getEnd.getCharacter, 6)
    assertEquals(out.getSeverity, LspSeverity.Warning)
    assertEquals(out.getCode.getLeft, "E123")

  test("missing severity stays null"):
    val out = LspConvert.diagnostic(bspDiag(0, 0, 0, 1, "m"))
    assertEquals(out.getSeverity, null)

  test("maps every severity"):
    def sev(b: BspSeverity): LspSeverity =
      val d = bspDiag(0, 0, 0, 1, "m"); d.setSeverity(b); LspConvert.diagnostic(d).getSeverity
    assertEquals(sev(BspSeverity.ERROR), LspSeverity.Error)
    assertEquals(sev(BspSeverity.WARNING), LspSeverity.Warning)
    assertEquals(sev(BspSeverity.INFORMATION), LspSeverity.Information)
    assertEquals(sev(BspSeverity.HINT), LspSeverity.Hint)
