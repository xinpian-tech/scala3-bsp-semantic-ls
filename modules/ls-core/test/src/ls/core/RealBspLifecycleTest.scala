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
  * each scenario boots its OWN [[RealBspServer]] over a private workspace copy
  * (via `RealBspFixture.freshWorkspace(...)`), so it never disturbs the read-only
  * workspace shared by `RealBspCoreTest`/`RealBspIntegrationTest`.
  *
  * Gated by `LS_REAL_BSP_IT=1`; skipped otherwise.
  *
  * Coverage: E2 (a real compile error is forwarded as an Error diagnostic; the
  * fix clears it), E3 (`didSave`→compile→reingest reflects new token positions
  * with no explicit reindex), E6 (a source shared across two targets unifies
  * references and passes the shared-source rename consistency check), and E8
  * (repeated saves keep exactly one segment dir + a consistent snapshot file, and
  * a warm restart serves references from the recovered index with no BSP compile).
  */
class RealBspLifecycleTest extends munit.FunSuite:

  import RealBspFixture.{consumerUri, enabled, otherUri}

  override def munitTimeout: Duration = 900.seconds

  private val sharedUri = "shared/src/pkgshared/Shared.scala"

  private val sharedSource =
    """package pkgshared
      |
      |object Shared:
      |  def marker: String = "shared-marker"
      |""".stripMargin

  /** A build where `a` and `d` BOTH compile `shared/src`, so `Shared.scala` is a
    * source shared across two targets.
    */
  private val sharedBuildMill =
    """//| mill-version: 1.1.2
      |//| mill-jvm-version: system
      |package build
      |
      |import mill.*
      |import mill.scalalib.*
      |
      |trait SampleModule extends ScalaModule {
      |  def scalaVersion = "3.8.4"
      |  def scalacOptions = Seq(
      |    "-Xsemanticdb",
      |    "-sourceroot",
      |    mill.api.BuildCtx.workspaceRoot.toString
      |  )
      |}
      |
      |object a extends SampleModule {
      |  def sources = Task.Sources(
      |    mill.api.BuildCtx.workspaceRoot / "a" / "src",
      |    mill.api.BuildCtx.workspaceRoot / "shared" / "src"
      |  )
      |}
      |
      |object d extends SampleModule {
      |  def sources = Task.Sources(mill.api.BuildCtx.workspaceRoot / "shared" / "src")
      |}
      |
      |object b extends SampleModule {
      |  def moduleDeps = Seq(a)
      |}
      |
      |object c extends ScalaModule {
      |  def scalaVersion = "3.8.4"
      |}
      |""".stripMargin

  // Own servers, lazy so the ordinary (ungated) run never boots mill-bsp.
  private lazy val fx: RealBspServer = new RealBspServer(RealBspFixture.freshWorkspace())
  private lazy val e6: RealBspServer =
    new RealBspServer(
      RealBspFixture.freshWorkspace(
        extraSources = Map(sharedUri -> sharedSource),
        buildMill = Some(sharedBuildMill)
      ),
      expectedDocs = -1
    )
  private lazy val e8: RealBspServer = new RealBspServer(RealBspFixture.freshWorkspace())

  private def guard(): Unit =
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")

  private def writeOn(srv: RealBspServer, uri: String, text: String): Unit =
    Files.write(srv.ws.root.resolve(uri), text.getBytes(StandardCharsets.UTF_8))

  private def saveOn(srv: RealBspServer, uri: String): Unit =
    srv.docsService.didSave(new DidSaveTextDocumentParams(srv.textDoc(uri)))

  /** Polls the recording client of `srv` for a `publishDiagnostics` for `uri`
    * arriving AFTER `sinceIndex` that satisfies `pred`.
    */
  private def awaitPublishSince(
      srv: RealBspServer,
      sinceIndex: Int,
      uri: String,
      timeoutMs: Long = 120000
  )(pred: Vector[Diagnostic] => Boolean): PublishDiagnosticsParams =
    val fileUri = srv.ws.fileUri(uri)
    val deadline = System.currentTimeMillis() + timeoutMs
    while System.currentTimeMillis() < deadline do
      srv.client.diagnostics.asScala.toVector
        .drop(sinceIndex)
        .find(p => p.getUri == fileUri && pred(p.getDiagnostics.asScala.toVector)) match
        case Some(p) => return p
        case None => Thread.sleep(200)
    fail(
      s"no publishDiagnostics for $uri (after index $sinceIndex) matched within ${timeoutMs}ms; " +
        s"saw: ${srv.client.diagnostics.asScala.toVector.map(p => p.getUri -> p.getDiagnostics.size)}"
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

  private def countSegmentDirs(srv: RealBspServer): Int =
    val dir = srv.segmentsDir
    if !Files.isDirectory(dir) then 0
    else
      val stream = Files.list(dir)
      try stream.iterator.asScala.count(_.getFileName.toString.startsWith("segment-"))
      finally stream.close()

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
      writeOn(fx, consumerUri, broken)
      saveOn(fx, consumerUri)
      val errPublish =
        awaitPublishSince(fx, before, consumerUri)(_.exists(_.getSeverity == DiagnosticSeverity.Error))
      assert(
        errPublish.getDiagnostics.asScala.exists(_.getSeverity == DiagnosticSeverity.Error),
        s"expected an Error diagnostic, got: ${errPublish.getDiagnostics}"
      )

      // E2b: fix and save -> the file publishes an error-free diagnostic list.
      val before2 = fx.client.diagnostics.size
      writeOn(fx, consumerUri, original)
      saveOn(fx, consumerUri)
      awaitPublishSince(fx, before2, consumerUri)(_.forall(_.getSeverity != DiagnosticSeverity.Error))
    finally writeOn(fx, consumerUri, original)

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
      writeOn(fx, consumerUri, moved)
      saveOn(fx, consumerUri)
      // The debounced pipeline compiles then re-ingests — NO explicit reindex.
      awaitCond(120000, "save-driven re-ingest published")(fx.server.completedIngests > ingestsBefore)

      val params = new ReferenceParams(
        fx.textDoc(consumerUri),
        cursorIn(moved, "message"),
        new ReferenceContext(true)
      )
      val locations = fx.docsService.references(params).get(120, TimeUnit.SECONDS).asScala.toVector
      val here =
        locations.filter(_.getUri == fx.ws.fileUri(consumerUri)).map(l => LspConvert.span(l.getRange))
      assert(here.contains(newSpan), s"expected a reference at the moved span $newSpan, got $here")
      assert(!here.contains(oldSpan), s"stale reference at the old span $oldSpan survived in $here")
    finally
      writeOn(fx, consumerUri, original)
      saveOn(fx, consumerUri)

  // -------------------------------------------------------------------- E6

  test("E6 a source shared across two targets unifies references and passes rename consistency"):
    guard()
    val _ = e6.readyIndex
    // `Shared.scala` is compiled by BOTH `a` and `d`, so the index holds two
    // documents for its uri. references on `marker` must unify to one location,
    // not one per target.
    val refParams =
      new ReferenceParams(e6.textDoc(sharedUri), e6.position(sharedUri, "marker"), new ReferenceContext(true))
    val locs = e6.docsService.references(refParams).get(120, TimeUnit.SECONDS).asScala.toVector
    val markerSpan = e6.ws.tokenSpan(sharedUri, "marker")
    val here = locs.filter(l => l.getUri == e6.ws.fileUri(sharedUri) && LspConvert.span(l.getRange) == markerSpan)
    assertEquals(here.length, 1, s"shared-source occurrence must unify to one location, got $locs")

    // rename runs the shared-source consistency check across both target views;
    // it succeeds (they agree) and edits the shared file.
    val edit = e6.docsService
      .rename(new RenameParams(e6.textDoc(sharedUri), e6.position(sharedUri, "marker"), "flag"))
      .get(600, TimeUnit.SECONDS)
    val changes = edit.getChanges
    assert(changes.containsKey(e6.ws.fileUri(sharedUri)), s"rename should edit the shared source: $changes")
    val edits = changes.get(e6.ws.fileUri(sharedUri)).asScala.toVector
    assertEquals(edits.map(_.getNewText).toSet, Set("flag"), s"$edits")
    assert(edits.exists(e => LspConvert.span(e.getRange) == markerSpan), s"$edits")

  // -------------------------------------------------------------------- E8

  test("E8 repeated saves keep one segment dir; a warm restart serves references from recovery"):
    guard()
    val _ = e8.readyIndex
    val original = e8.ws.sourceText(otherUri)
    // >= 3 didSave -> compile -> reingest cycles (toggle a trailing comment).
    for i <- 1 to 3 do
      val txt = e8.ws.sourceText(otherUri)
      val edited = if txt.endsWith("// e8 mark\n") then original else txt + "// e8 mark\n"
      val before = e8.server.completedIngests
      writeOn(e8, otherUri, edited)
      saveOn(e8, otherUri)
      awaitCond(120000, s"e8 re-ingest cycle $i")(e8.server.completedIngests > before)

    // segment hygiene: exactly one segment dir + a consistent snapshot file.
    assertEquals(countSegmentDirs(e8), 1, s"expected one segment dir in ${e8.segmentsDir}")
    assert(
      e8.executeCommand(ScalaLs.Commands.Doctor).contains("snapshot file: consistent"),
      "doctor should report a consistent snapshot file"
    )

    // warm restart: shut the server down and boot a NEW one WITHOUT a BSP
    // connection on the same workspace/storage; it must serve references from the
    // recovered snapshot, before (indeed without) any BSP compile.
    writeOn(e8, otherUri, original)
    e8.shutdown()
    val recovered = new RealBspServer(e8.ws, expectedDocs = -1, withBsp = false)
    try
      val _ = recovered.initResult
      val refParams = new ReferenceParams(
        recovered.textDoc(consumerUri),
        recovered.position(consumerUri, "message"),
        new ReferenceContext(true)
      )
      val refs = recovered.docsService.references(refParams).get(120, TimeUnit.SECONDS).asScala.toVector
      assert(refs.nonEmpty, "warm restart must serve references from the recovered snapshot")
      assert(
        refs.exists(_.getUri == recovered.ws.fileUri(consumerUri)),
        s"expected a recovered reference in Consumer.scala, got $refs"
      )
    finally recovered.shutdown()
