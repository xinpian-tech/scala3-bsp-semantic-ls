package ls.core

import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.util.concurrent.TimeUnit

import scala.concurrent.duration.{Duration, DurationInt}
import scala.jdk.CollectionConverters.*

import org.eclipse.lsp4j.*

import ls.index.Span

/** Real-BSP Lifecycle batch: the mutating, editor-session-over-time rows of the
  * real-`mill --bsp` end-to-end suite. Because these tests EDIT sources on disk,
  * the suite boots its OWN [[RealBspServer]] over a private workspace copy (via
  * `RealBspFixture.freshWorkspace()`), so it never disturbs the read-only
  * workspace that `RealBspCoreTest`/`RealBspIntegrationTest` share.
  *
  * Gated by `LS_REAL_BSP_IT=1`; skipped otherwise.
  *
  * Coverage this round: E2 (a real compile error is forwarded as an Error
  * diagnostic, and the fix clears it) and E3 (`didSave`→compile→reingest reflects
  * the edited file's new token positions with no explicit reindex). E6
  * (shared-source) and E8 (segment hygiene + warm restart) are the remaining
  * Lifecycle rows.
  */
class RealBspLifecycleTest extends munit.FunSuite:

  import RealBspFixture.{consumerUri, enabled}

  override def munitTimeout: Duration = 900.seconds

  /** Own mutable workspace + server; lazy so the ordinary (ungated) run never
    * boots mill-bsp.
    */
  private lazy val fx: RealBspServer = new RealBspServer(RealBspFixture.freshWorkspace())

  private def guard(): Unit =
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")

  private def writeSource(uri: String, text: String): Unit =
    Files.write(fx.ws.root.resolve(uri), text.getBytes(StandardCharsets.UTF_8))

  private def save(uri: String): Unit =
    fx.docsService.didSave(new DidSaveTextDocumentParams(fx.textDoc(uri)))

  /** Polls the recording client for a `publishDiagnostics` for `uri` that arrives
    * AFTER `sinceIndex` and satisfies `pred`.
    */
  private def awaitPublishSince(
      sinceIndex: Int,
      uri: String,
      timeoutMs: Long = 120000
  )(pred: Vector[Diagnostic] => Boolean): PublishDiagnosticsParams =
    val fileUri = fx.ws.fileUri(uri)
    val deadline = System.currentTimeMillis() + timeoutMs
    while System.currentTimeMillis() < deadline do
      val fresh = fx.client.diagnostics.asScala.toVector.drop(sinceIndex)
      fresh.find(p => p.getUri == fileUri && pred(p.getDiagnostics.asScala.toVector)) match
        case Some(p) => return p
        case None => Thread.sleep(200)
    fail(
      s"no publishDiagnostics for $uri (after index $sinceIndex) matched within ${timeoutMs}ms; " +
        s"saw: ${fx.client.diagnostics.asScala.toVector.map(p => p.getUri -> p.getDiagnostics.size)}"
    )

  private def awaitCond(timeoutMs: Long, what: String)(cond: => Boolean): Unit =
    val deadline = System.currentTimeMillis() + timeoutMs
    while System.currentTimeMillis() < deadline do
      if cond then return
      Thread.sleep(200)
    fail(s"condition not met within ${timeoutMs}ms: $what")

  /** 0-based span of the nth whole-word `token` occurrence in `text`. */
  private def spanIn(text: String, token: String, nth: Int = 0): Span =
    val spans =
      for
        (line, ln) <- text.linesIterator.toVector.zipWithIndex
        i <- Iterator.iterate(line.indexOf(token))(p => line.indexOf(token, p + 1)).takeWhile(_ >= 0)
        if (i == 0 || !Character.isJavaIdentifierPart(line.charAt(i - 1))) &&
          (i + token.length >= line.length || !Character.isJavaIdentifierPart(line.charAt(i + token.length)))
      yield Span(ln, i, ln, i + token.length)
    assert(nth < spans.length, s"token '$token' occurrence $nth not found")
    spans(nth)

  private def cursorIn(text: String, token: String, nth: Int = 0): Position =
    val s = spanIn(text, token, nth)
    new Position(s.startLine, s.startChar + 1)

  // -------------------------------------------------------------------- E2

  test("E2 a real compile error is forwarded as an Error diagnostic; the fix clears it"):
    guard()
    val _ = fx.readyIndex
    val original = fx.ws.sourceText(consumerUri)
    try
      // introduce a type error: `message` is a String, so `val text: Int = ...` fails.
      val broken = original.replace("val text: String =", "val text: Int =")
      assert(broken != original, "fixture text changed; update the E2 edit")

      val before = fx.client.diagnostics.size
      writeSource(consumerUri, broken)
      save(consumerUri)
      val errPublish =
        awaitPublishSince(before, consumerUri)(_.exists(_.getSeverity == DiagnosticSeverity.Error))
      assert(
        errPublish.getDiagnostics.asScala.exists(_.getSeverity == DiagnosticSeverity.Error),
        s"expected an Error diagnostic, got: ${errPublish.getDiagnostics}"
      )

      // E2b: fix and save -> the file publishes an empty (error-free) diagnostic list.
      val before2 = fx.client.diagnostics.size
      writeSource(consumerUri, original)
      save(consumerUri)
      awaitPublishSince(before2, consumerUri)(
        _.forall(_.getSeverity != DiagnosticSeverity.Error)
      )
    finally writeSource(consumerUri, original)

  // -------------------------------------------------------------------- E3

  test("E3 didSave -> compile -> reingest reflects new token positions with no explicit reindex"):
    guard()
    val _ = fx.readyIndex
    val original = fx.ws.sourceText(consumerUri)
    try
      // Insert a comment line ABOVE the `greeting.message` usage, shifting it one
      // line down. The comment must NOT contain the token `message`, or spanIn
      // would match the comment instead of the usage.
      val moved =
        original.replace(
          "  val text: String = greeting.message",
          "  // pad line to shift the usage down\n  val text: String = greeting.message"
        )
      assert(moved != original, "fixture text changed; update the E3 edit")
      val oldSpan = spanIn(original, "message")
      val newSpan = spanIn(moved, "message")
      assert(newSpan.startLine == oldSpan.startLine + 1, "the edit must shift the token's line")

      val ingestsBefore = fx.server.completedIngests
      writeSource(consumerUri, moved)
      save(consumerUri)
      // The debounced pipeline compiles then re-ingests — NO explicit reindex.
      awaitCond(120000, "save-driven re-ingest published")(fx.server.completedIngests > ingestsBefore)

      // references at the (now shifted) usage include the NEW span and NOT the
      // old one, proving the index reflects the edit without a manual reindex.
      val params = new ReferenceParams(
        fx.textDoc(consumerUri),
        cursorIn(moved, "message"),
        new ReferenceContext(true)
      )
      val locations = fx.docsService.references(params).get(120, TimeUnit.SECONDS).asScala.toVector
      val here = locations.filter(_.getUri == fx.ws.fileUri(consumerUri)).map(l => LspConvert.span(l.getRange))
      assert(here.contains(newSpan), s"expected a reference at the moved span $newSpan, got $here")
      assert(!here.contains(oldSpan), s"stale reference at the old span $oldSpan survived in $here")
    finally
      writeSource(consumerUri, original)
      save(consumerUri)
