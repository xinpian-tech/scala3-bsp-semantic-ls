package ls.core

import java.util.concurrent.{CompletableFuture, ExecutionException, TimeUnit}

import scala.concurrent.duration.{Duration, DurationInt}
import scala.jdk.CollectionConverters.*

import org.eclipse.lsp4j.*
import org.eclipse.lsp4j.jsonrpc.ResponseErrorException

import ls.index.Span

/** Real-BSP Core batch: the deterministic `it/sample-workspace` acceptance the
  * mainline audit requires beyond the happy-path [[RealBspIntegrationTest]].
  * Shares the single booted mill-bsp server through [[RealBspFixture]].
  *
  * Gated by `LS_REAL_BSP_IT=1` (run `scripts/it-real-bsp.sh` inside
  * `nix develop`); skipped otherwise.
  *
  * Coverage:
  *   - E1: SemanticDB is MANDATORY. Module `c` (built without `-Xsemanticdb`)
  *     emits no SemanticDB, so the doctor flags it as an ERROR and EVERY request
  *     on a `c` source — PC (completion) and index (documentHighlight) alike —
  *     is a hard error, never a served result or an empty result.
  *   - E4: rename is rejected with the correct typed error for a source without
  *     SemanticDB (E4a), an external/library symbol (E4b), and a position with
  *     no symbol occurrence (E4c).
  *   - E5: the PC-backed position features (hover, signatureHelp, definition)
  *     and the index-backed documentHighlight answer on the indexed modules.
  */
class RealBspCoreTest extends munit.FunSuite:

  import RealBspFixture.{
    docsService,
    enabled,
    executeCommand,
    greetingUri,
    position,
    textDoc,
    widgetUri,
    withOpen,
    ws
  }

  override def munitTimeout: Duration = 900.seconds

  private def guard(): Unit =
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")

  /** Message of the ResponseError a failing request completes exceptionally
    * with (hard errors surface as JSON-RPC errors over the wire).
    */
  private def requestError(future: CompletableFuture[?]): String =
    val ex = intercept[ExecutionException](future.get(600, TimeUnit.SECONDS))
    ex.getCause match
      case ree: ResponseErrorException => ree.getResponseError.getMessage
      case other => other.getMessage

  private def renameError(uri: String, pos: Position, newName: String): String =
    requestError(docsService.rename(new RenameParams(textDoc(uri), pos, newName)))

  private def lineOf(uri: String, needle: String): (Int, String) =
    val lines = ws.sourceText(uri).linesIterator.toVector
    val idx = lines.indexWhere(_.contains(needle))
    assert(idx >= 0, s"'$needle' not found in $uri")
    (idx, lines(idx))

  // ------------------------------------------------------------ E-foundation

  test("E0 foundation: the shared real mill-bsp server booted and the index filled"):
    guard()
    val summary = RealBspFixture.readyIndex
    assert(summary.contains("4 docs"), summary)
    val report = executeCommand(ScalaLs.Commands.Doctor)
    assert(report.contains("state: ready"), report)
    assert(report.contains("server: mill-bsp"), report)

  // -------------------------------------------------------------------- E1

  test("E1 doctor: module c (no -Xsemanticdb) is flagged as a SemanticDB error"):
    guard()
    val _ = RealBspFixture.initResult
    val report = executeCommand(ScalaLs.Commands.Doctor)
    val rootUri = Uris.toUri(ws.root).stripSuffix("/")
    val coverage =
      "SemanticDB coverage: ([^\\n]*)".r.findFirstMatchIn(report).map(_.group(1).trim)
    assert(coverage.exists(_.startsWith("ERROR")), s"expected a SemanticDB error in:\n$report")
    assert(
      coverage.exists(_.contains(s"$rootUri/c")),
      s"module c must be flagged without SemanticDB in:\n$report"
    )

  test("E1 SemanticDB is mandatory: completion on module c is a hard error (no PC fallback)"):
    guard()
    val _ = RealBspFixture.readyIndex
    // c has no SemanticDB; the presentation compiler must NOT quietly serve it.
    val params = new CompletionParams(textDoc(widgetUri), position(widgetUri, "area"))
    val message = requestError(docsService.completion(params))
    assert(message.contains("has no SemanticDB output"), s"expected a NoSemanticdb error: $message")
    assert(message.contains("-Xsemanticdb"), message)

  test("E1 SemanticDB is mandatory: documentHighlight on module c is a hard error (not empty)"):
    guard()
    val _ = RealBspFixture.readyIndex
    val params = new DocumentHighlightParams(textDoc(widgetUri), position(widgetUri, "area"))
    val message = requestError(docsService.documentHighlight(params))
    assert(message.contains("has no SemanticDB output"), s"expected a NoSemanticdb error: $message")

  // -------------------------------------------------------------------- E4

  test("E4a rename on a source without SemanticDB (module c) is a hard error"):
    guard()
    val _ = RealBspFixture.readyIndex
    val message = renameError(widgetUri, position(widgetUri, "area"), "surface")
    assert(
      message.contains("has no SemanticDB output"),
      s"expected a NoSemanticdb rejection, got: $message"
    )

  test("E4b rename of an external/library symbol is rejected (outside the workspace)"):
    guard()
    val _ = RealBspFixture.readyIndex
    // `String` in `class Greeting(val name: String)` is defined outside the
    // workspace; rename must reject it after the fresh compile+ingest.
    val message = renameError(greetingUri, position(greetingUri, "String"), "Str")
    assert(message.contains("rename rejected"), s"expected a rename rejection, got: $message")
    assert(
      message.contains("outside the workspace"),
      s"expected the external-symbol reason, got: $message"
    )

  test("E4c rename at a position with no symbol occurrence is rejected"):
    guard()
    val _ = RealBspFixture.readyIndex
    // Cursor inside the string literal "world" — scalac emits no occurrence there.
    val (litLine, text) = lineOf(greetingUri, "\"world\"")
    val litChar = text.indexOf("\"world\"") + 3
    val message = renameError(greetingUri, new Position(litLine, litChar), "planet")
    assert(
      message.contains("no symbol occurrence"),
      s"expected a NoSymbolAtCursor rejection, got: $message"
    )

  // -------------------------------------------------------------------- E5

  test("E5 hover (PC) answers on an indexed module"):
    guard()
    val _ = RealBspFixture.readyIndex
    withOpen(greetingUri, ws.sourceText(greetingUri)) {
      val params = new HoverParams(textDoc(greetingUri), position(greetingUri, "message"))
      val hover = docsService.hover(params).get(120, TimeUnit.SECONDS)
      assert(hover != null, "expected a non-null hover for `message`")
      val rendered =
        Option(hover.getContents).map { c =>
          if c.isRight then c.getRight.getValue
          else c.getLeft.asScala.map(_.toString).mkString
        }.getOrElse("")
      assert(rendered.nonEmpty, s"expected non-empty hover contents, got: $hover")
    }

  test("E5 signatureHelp (PC) answers at a constructor call site"):
    guard()
    val _ = RealBspFixture.readyIndex
    withOpen(greetingUri, ws.sourceText(greetingUri)) {
      val (callLine, text) = lineOf(greetingUri, "new Greeting(\"world\")")
      val callChar = text.indexOf("new Greeting(") + "new Greeting(".length
      val params = new SignatureHelpParams(textDoc(greetingUri), new Position(callLine, callChar))
      val sig = docsService.signatureHelp(params).get(120, TimeUnit.SECONDS)
      assert(sig != null, "expected a non-null signatureHelp")
      assert(!sig.getSignatures.isEmpty, s"expected at least one signature, got: $sig")
    }

  test("E5 definition (PC) resolves a same-file reference to its declaration"):
    guard()
    val _ = RealBspFixture.readyIndex
    withOpen(greetingUri, ws.sourceText(greetingUri)) {
      // The `Greeting` in `new Greeting("world")` (4th whole-word occurrence).
      val params = new DefinitionParams(textDoc(greetingUri), position(greetingUri, "Greeting", 3))
      val result = docsService.definition(params).get(120, TimeUnit.SECONDS)
      assert(result.isLeft, s"expected Location results, got: $result")
      val locations = result.getLeft.asScala.toVector
      assert(locations.nonEmpty, "expected a definition location")
      assert(
        locations.exists(_.getUri == ws.fileUri(greetingUri)),
        s"expected the definition in Greeting.scala, got: ${locations.map(_.getUri)}"
      )
    }

  test("E5 documentHighlight (index) returns the in-file occurrences of a symbol"):
    guard()
    val _ = RealBspFixture.readyIndex
    val params =
      new DocumentHighlightParams(textDoc(greetingUri), position(greetingUri, "name"))
    val highlights = docsService.documentHighlight(params).get(60, TimeUnit.SECONDS).asScala.toVector
    val spans: Set[Span] = highlights.map(h => LspConvert.span(h.getRange)).toSet
    val expected = ws.tokenSpans(greetingUri, "name").toSet
    assertEquals(spans, expected, s"expected the two `name` occurrences, got $highlights")
