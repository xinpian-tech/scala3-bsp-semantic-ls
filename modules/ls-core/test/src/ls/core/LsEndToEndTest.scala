package ls.core

import java.io.{PipedInputStream, PipedOutputStream}
import java.util.concurrent.{ExecutionException, Executors, TimeUnit}

import scala.concurrent.duration.{Duration, DurationInt}
import scala.jdk.CollectionConverters.*

import ch.epfl.scala.bsp4j.BuildClient
import org.eclipse.lsp4j.*
import org.eclipse.lsp4j.jsonrpc.{Launcher, ResponseErrorException}
import org.eclipse.lsp4j.launch.LSPLauncher
import org.eclipse.lsp4j.services.LanguageServer

import ls.bsp.{BspSession, BspSessionConfig, FakeBuildServer}
import ls.index.Span

/** Flagship end-to-end test: a scalac-compiled fixture served through the
  * REAL wiring — [[FakeBuildServer]] over the real bsp4j jsonrpc Launcher on
  * one pipe pair, [[ScalaLs]] served to a real lsp4j client Launcher on
  * another — with lsp4j 1.0.0 (forced by mtags-interfaces) on the classpath
  * of BOTH. This is the proof that bsp4j 2.2.0-M2 (built against
  * lsp4j-jsonrpc 0.20.1) and lsp4j 1.0.0 coexist at runtime.
  */
class LsEndToEndTest extends munit.FunSuite:

  override def munitTimeout: Duration = 600.seconds

  private object env:
    val ws: E2eFixture.Ws = E2eFixture.master

    val fake = new FakeBuildServer(
      ws.root,
      ws.aSourceDir,
      ws.bSourceFile,
      ws.cSourceFile,
      ws.semanticdbOverride,
      advertiseInverseSources = true
    )
    val bspServer = new ClasspathAugmentingServer(
      fake,
      {
        case "a" => E2eFixture.libraryClasspath
        case "b" => E2eFixture.libraryClasspath :+ ws.classDirOf("a")
        case _ => E2eFixture.libraryClasspath
      }
    )

    val bspExecutor = Executors.newCachedThreadPool { (r: Runnable) =>
      val t = new Thread(r, "e2e-fake-bsp")
      t.setDaemon(true)
      t
    }

    // BSP pipes: fake server <-> BspSession, real bsp4j Launcher machinery.
    val bspToClient = new PipedInputStream(1 << 20)
    val bspServerOut = new PipedOutputStream(bspToClient)
    val bspToServer = new PipedInputStream(1 << 20)
    val bspClientOut = new PipedOutputStream(bspToServer)
    val bspLauncher = new Launcher.Builder[BuildClient]()
      .setLocalService(bspServer)
      .setRemoteInterface(classOf[BuildClient])
      .setInput(bspToServer)
      .setOutput(bspServerOut)
      .setExecutorService(bspExecutor)
      .create()
    fake.client = bspLauncher.getRemoteProxy
    bspLauncher.startListening()

    val server = new ScalaLs(
      ScalaLs.Config(
        bootstrap = Bootstrap.Config(
          connectBsp = (root, handlers) =>
            Some(
              BspSession.connect(
                root,
                bspToClient,
                bspClientOut,
                handlers,
                BspSessionConfig(requestTimeout = 60.seconds)
              )
            ),
          pcRequestTimeoutMillis = 120000L,
          log = msg => System.err.println(s"[e2e-ls] $msg")
        ),
        debounceMillis = 150L,
        exitProcessOnExit = false
      )
    )

    // LSP pipes: real LSPLauncher on both ends.
    val lspToServer = new PipedInputStream(1 << 20)
    val lspClientOut = new PipedOutputStream(lspToServer)
    val lspToClient = new PipedInputStream(1 << 20)
    val lspServerOut = new PipedOutputStream(lspToClient)

    val serverLauncher = LSPLauncher.createServerLauncher(server, lspToServer, lspServerOut)
    server.connect(serverLauncher.getRemoteProxy)
    serverLauncher.startListening()

    val client = new RecordingLanguageClient
    val clientLauncher = LSPLauncher.createClientLauncher(client, lspToClient, lspClientOut)
    clientLauncher.startListening()
    val proxy: LanguageServer = clientLauncher.getRemoteProxy

    lazy val initResult: InitializeResult =
      val params = new InitializeParams()
      params.setRootUri(Uris.toUri(ws.root))
      val result = proxy.initialize(params).get(60, TimeUnit.SECONDS)
      proxy.initialized(new InitializedParams())
      assert(server.awaitBootstrap(180000L), "bootstrap did not finish in time")
      result

  private def ws = env.ws
  private def docsService = env.proxy.getTextDocumentService
  private def wsService = env.proxy.getWorkspaceService

  private def textDoc(uri: String) = new TextDocumentIdentifier(ws.fileUri(uri))

  private def position(uri: String, token: String, nth: Int = 0): Position =
    val (line, character) = ws.cursor(uri, token, nth)
    new Position(line, character)

  private def locationOf(uri: String, span: Span): Location =
    LspConvert.location(ws.fileUri(uri), span)

  private def executeCommand(command: String): String =
    wsService
      .executeCommand(new ExecuteCommandParams(command, java.util.List.of()))
      .get(120, TimeUnit.SECONDS)
      .asInstanceOf[String]

  private def eventually(clue: String, timeoutMs: Long = 20000)(cond: => Boolean): Unit =
    val deadline = System.currentTimeMillis() + timeoutMs
    while !cond && System.currentTimeMillis() < deadline do Thread.sleep(50)
    assert(cond, s"condition not reached within ${timeoutMs}ms: $clue")

  override def afterAll(): Unit =
    try env.proxy.shutdown().get(30, TimeUnit.SECONDS)
    catch case _: Exception => ()

  // ------------------------------------------------------------------ tests

  test("initialize over the wire returns the advertised capabilities"):
    val caps = env.initResult.getCapabilities
    assert(caps.getWorkspaceSymbolProvider.getLeft, "workspaceSymbolProvider")
    assert(caps.getReferencesProvider.getLeft, "referencesProvider")
    assert(caps.getRenameProvider.isRight && caps.getRenameProvider.getRight.getPrepareProvider,
      "renameProvider.prepareProvider")
    assert(caps.getDocumentHighlightProvider.getLeft, "documentHighlightProvider")
    assert(caps.getCompletionProvider != null, "completionProvider")
    assertEquals(
      caps.getExecuteCommandProvider.getCommands.asScala.toSet,
      ScalaLs.Commands.all.toSet
    )

  test("bootstrap talked real BSP: initialize/initialized handshake reached the fake server"):
    val _ = env.initResult
    assert(env.fake.initializeReceived.get, "build/initialize")
    assert(env.fake.initializedNotified.get, "build/initialized")

  private def sameFile(uri: String, path: java.nio.file.Path): Boolean =
    java.nio.file.Path.of(java.net.URI.create(uri)) == path

  private def bspErrorPublish(file: java.nio.file.Path, targetName: String, msg: String)
      : ch.epfl.scala.bsp4j.PublishDiagnosticsParams =
    val d = new ch.epfl.scala.bsp4j.Diagnostic(
      new ch.epfl.scala.bsp4j.Range(
        new ch.epfl.scala.bsp4j.Position(1, 2),
        new ch.epfl.scala.bsp4j.Position(1, 7)
      ),
      msg
    )
    d.setSeverity(ch.epfl.scala.bsp4j.DiagnosticSeverity.ERROR)
    new ch.epfl.scala.bsp4j.PublishDiagnosticsParams(
      new ch.epfl.scala.bsp4j.TextDocumentIdentifier(file.toUri.toString),
      new ch.epfl.scala.bsp4j.BuildTargetIdentifier(s"bsp://workspace/$targetName"),
      java.util.List.of(d),
      true
    )

  private def bspClearPublish(file: java.nio.file.Path, targetName: String)
      : ch.epfl.scala.bsp4j.PublishDiagnosticsParams =
    new ch.epfl.scala.bsp4j.PublishDiagnosticsParams(
      new ch.epfl.scala.bsp4j.TextDocumentIdentifier(file.toUri.toString),
      new ch.epfl.scala.bsp4j.BuildTargetIdentifier(s"bsp://workspace/$targetName"),
      java.util.List.of(),
      true
    )

  private def publishesFor(file: java.nio.file.Path): Vector[PublishDiagnosticsParams] =
    env.client.diagnostics.asScala.filter(p => sameFile(p.getUri, file)).toVector

  test("executeCommand compile forwards an error diagnostic with the exact shape and status"):
    val _ = env.initResult
    val bFile = ws.bSourceFile
    env.client.diagnostics.clear()
    env.bspServer.stageCompile(
      CompileScenario(Vector(bspErrorPublish(bFile, "b", "boom in B")), ch.epfl.scala.bsp4j.StatusCode.ERROR)
    )
    val result = executeCommand(ScalaLs.Commands.Compile)
    assert(result.startsWith("compile failed"), result)
    eventually("exactly one diagnostic publish for B.scala")(publishesFor(bFile).size == 1)
    val publish = publishesFor(bFile).head
    val diags = publish.getDiagnostics.asScala.toVector
    assertEquals(diags.size, 1)
    assertEquals(diags.head.getSeverity, DiagnosticSeverity.Error)
    assertEquals(LspConvert.span(diags.head.getRange), Span(1, 2, 1, 7))
    assertEquals(diags.head.getMessage.getLeft, "boom in B")

  test("a reset-empty diagnostic clears the uri with exactly one empty publish"):
    val _ = env.initResult
    val bFile = ws.bSourceFile
    env.client.diagnostics.clear()
    env.bspServer.stageCompile(
      CompileScenario(Vector(bspErrorPublish(bFile, "b", "to be cleared")), ch.epfl.scala.bsp4j.StatusCode.OK)
    )
    executeCommand(ScalaLs.Commands.Compile)
    eventually("initial non-empty diagnostic present")(
      publishesFor(bFile).exists(!_.getDiagnostics.isEmpty)
    )
    env.client.diagnostics.clear()
    env.bspServer.stageCompile(
      CompileScenario(Vector(bspClearPublish(bFile, "b")), ch.epfl.scala.bsp4j.StatusCode.OK)
    )
    executeCommand(ScalaLs.Commands.Compile)
    eventually("clearing publish arrives")(publishesFor(bFile).nonEmpty)
    val clears = publishesFor(bFile)
    assertEquals(clears.size, 1)
    assert(clears.head.getDiagnostics.isEmpty, "expected empty clearing publish")

  test("doctor over executeCommand reports the BSP server and the IndexUnavailable target"):
    val _ = env.initResult
    val report = executeCommand(ScalaLs.Commands.Doctor)
    assert(report.contains("state: ready"), report)
    assert(report.contains("fake-bsp-server"), report)
    assert(report.contains("bsp://workspace/c"), report)
    assert(report.contains("SQLite:"), report)
    assert(report.contains("Postings:"), report)

  test("workspace/symbol finds the fixture class with a real file location"):
    val _ = env.initResult
    val result = wsService.symbol(new WorkspaceSymbolParams("Core")).get(60, TimeUnit.SECONDS)
    assert(result.isRight, "expected WorkspaceSymbol list")
    val symbols = result.getRight.asScala.toVector
    val core = symbols.filter(_.getName == "Core")
    assert(core.nonEmpty, symbols.map(_.getName).toString)
    assert(
      core.exists(s => s.getLocation.getLeft.getUri == ws.fileUri(E2eFixture.coreUri)),
      core.map(_.getLocation).toString
    )

  test("textDocument/references returns all cross-file locations"):
    val _ = env.initResult
    val params = new ReferenceParams(
      textDoc(E2eFixture.useUri),
      position(E2eFixture.useUri, "ping"),
      new ReferenceContext(true)
    )
    val locations = docsService.references(params).get(60, TimeUnit.SECONDS).asScala.toVector
    val expected = Vector(
      locationOf(E2eFixture.coreUri, ws.tokenSpan(E2eFixture.coreUri, "ping", 0)),
      locationOf(E2eFixture.useUri, ws.tokenSpan(E2eFixture.useUri, "ping", 0)),
      locationOf(E2eFixture.bUri, ws.tokenSpan(E2eFixture.bUri, "ping", 0))
    )
    for e <- expected do assert(locations.contains(e), s"missing $e in $locations")

  test("textDocument/documentHighlight works on a clean file"):
    val _ = env.initResult
    val params = new DocumentHighlightParams(
      textDoc(E2eFixture.coreUri),
      position(E2eFixture.coreUri, "label", 1) // the use inside ping
    )
    val highlights = docsService.documentHighlight(params).get(60, TimeUnit.SECONDS).asScala.toVector
    assert(highlights.length >= 2, highlights.toString)
    val useSpan = ws.tokenSpan(E2eFixture.coreUri, "label", 1)
    assert(
      highlights.exists(h => LspConvert.span(h.getRange) == useSpan),
      s"expected $useSpan in $highlights"
    )

  test("rename with an invalid identifier surfaces the LsError message over the wire"):
    val _ = env.initResult
    val params = new RenameParams(
      textDoc(E2eFixture.coreUri),
      position(E2eFixture.coreUri, "make"),
      "not`valid"
    )
    val ex = intercept[ExecutionException] {
      docsService.rename(params).get(60, TimeUnit.SECONDS)
    }
    val cause = ex.getCause
    assert(cause.isInstanceOf[ResponseErrorException], cause.toString)
    val message = cause.asInstanceOf[ResponseErrorException].getResponseError.getMessage
    assert(message.contains("rename rejected"), message)
    assert(message.contains("not a valid Scala identifier"), message)

  test("prepareRename returns the exact token range"):
    val _ = env.initResult
    val params = new PrepareRenameParams(
      textDoc(E2eFixture.coreUri),
      position(E2eFixture.coreUri, "make")
    )
    val result = docsService.prepareRename(params).get(60, TimeUnit.SECONDS)
    assert(result != null, "prepareRename returned null")
    assertEquals(LspConvert.span(result.getFirst), ws.tokenSpan(E2eFixture.coreUri, "make", 0))

  test("textDocument/rename compiles, re-ingests and returns the cross-file WorkspaceEdit"):
    val _ = env.initResult
    val params = new RenameParams(
      textDoc(E2eFixture.coreUri),
      position(E2eFixture.coreUri, "make"),
      "create"
    )
    val edit = docsService.rename(params).get(120, TimeUnit.SECONDS)
    val changes = edit.getChanges
    assertEquals(
      changes.keySet.asScala.toSet,
      Set(
        ws.fileUri(E2eFixture.coreUri),
        ws.fileUri(E2eFixture.useUri),
        ws.fileUri(E2eFixture.bUri)
      )
    )
    def editsOf(uri: String): Vector[TextEdit] =
      changes.get(ws.fileUri(uri)).asScala.toVector
    for uri <- Vector(E2eFixture.coreUri, E2eFixture.useUri, E2eFixture.bUri) do
      val expected = ws.tokenSpan(uri, "make", 0)
      val edits = editsOf(uri)
      assert(edits.forall(_.getNewText == "create"), edits.toString)
      assert(
        edits.exists(e => LspConvert.span(e.getRange) == expected),
        s"expected an edit at $expected in $uri, got $edits"
      )

  test("didOpen + completion goes through the real presentation compiler"):
    val _ = env.initResult
    val dirtyText = ws.sourceText(E2eFixture.useUri) + "  val q = core.pi\n"
    docsService.didOpen(
      new DidOpenTextDocumentParams(
        new TextDocumentItem(ws.fileUri(E2eFixture.useUri), "scala", 1, dirtyText)
      )
    )
    val line = dirtyText.linesIterator.length - 1
    val character = "  val q = core.pi".length
    val params = new CompletionParams(textDoc(E2eFixture.useUri), new Position(line, character))
    val result = docsService.completion(params).get(180, TimeUnit.SECONDS)
    val items =
      if result.isRight then result.getRight.getItems.asScala.toVector
      else result.getLeft.asScala.toVector
    assert(items.exists(_.getLabel.startsWith("ping")), items.map(_.getLabel).toString)

  test("references on a dirty buffer resolve through the PC overlay"):
    val _ = env.initResult
    // Use.scala is open and dirty (previous test); the cursor symbol must
    // come from the PC overlay, the occurrence set from the index.
    val params = new ReferenceParams(
      textDoc(E2eFixture.useUri),
      position(E2eFixture.useUri, "core", 1), // the use in `core.ping`
      new ReferenceContext(true)
    )
    val locations = docsService.references(params).get(120, TimeUnit.SECONDS).asScala.toVector
    val defLocation = locationOf(E2eFixture.useUri, ws.tokenSpan(E2eFixture.useUri, "core", 0))
    assert(locations.contains(defLocation), s"missing $defLocation in $locations")

  test("didSave schedules the debounced compile + re-ingest job"):
    val _ = env.initResult
    val compilesBefore = env.bspServer.compileRequests.get
    val snapshotBefore = snapshotId(executeCommand(ScalaLs.Commands.Doctor))
    docsService.didSave(new DidSaveTextDocumentParams(textDoc(E2eFixture.useUri)))
    eventually("background compile ran") {
      env.bspServer.compileRequests.get > compilesBefore
    }
    eventually("background ingest published a fresh snapshot") {
      snapshotId(executeCommand(ScalaLs.Commands.Doctor)).exists(id => snapshotBefore.forall(_ < id))
    }

  test("two save-driven re-ingests leave exactly one segment dir and no compaction debt"):
    val _ = env.initResult
    val services = env.server.currentState.ready.get
    val segmentsDir = services.snapshots.segmentsDir

    def segmentDirCount: Int =
      if !java.nio.file.Files.isDirectory(segmentsDir) then 0
      else
        val stream = java.nio.file.Files.list(segmentsDir)
        try stream.iterator().asScala.count(_.getFileName.toString.startsWith("segment-"))
        finally stream.close()

    // Drive two save -> debounced compile -> re-ingest -> publish cycles, waiting
    // for each via non-retaining counters (never holding a snapshot across a publish).
    for _ <- 1 to 2 do
      val compilesBefore = env.bspServer.compileRequests.get
      val ingestsBefore = env.server.completedIngests
      docsService.didSave(new DidSaveTextDocumentParams(textDoc(E2eFixture.useUri)))
      eventually("save-driven compile ran")(env.bspServer.compileRequests.get > compilesBefore)
      eventually("save-driven ingest published")(env.server.completedIngests > ingestsBefore)

    // The publish tail reclaimed each superseded segment (no manual janitor call here).
    assertEquals(segmentDirCount, 1, s"expected one segment dir in $segmentsDir")
    assert(
      executeCommand(ScalaLs.Commands.Doctor).contains("compaction pending: 0"),
      "doctor should report no compaction debt after drained re-ingests"
    )

  test("buildTarget/didChange reloads the model and re-ingests"):
    val _ = env.initResult
    val loadsBefore = env.fake.workspaceBuildTargetsCalls.get
    val ingestsBefore = env.server.completedIngests
    val snapshotBefore = snapshotId(executeCommand(ScalaLs.Commands.Doctor))
    env.bspServer.sendDidChange("a")
    eventually("workspaceBuildTargets refetched")(env.fake.workspaceBuildTargetsCalls.get > loadsBefore)
    eventually("re-ingest ran after reload")(env.server.completedIngests > ingestsBefore)
    eventually("doctor snapshot generation advanced")(
      snapshotId(executeCommand(ScalaLs.Commands.Doctor)).exists(id => snapshotBefore.forall(_ < id))
    )
    // The server stays queryable after the reload.
    val symbols = wsService.symbol(new WorkspaceSymbolParams("Core")).get(60, TimeUnit.SECONDS)
    assert(symbols.isRight && symbols.getRight.asScala.exists(_.getName == "Core"))

  test("shutdown tears down the BSP session"):
    val _ = env.initResult
    env.proxy.shutdown().get(60, TimeUnit.SECONDS)
    eventually("BSP shutdown requested")(env.fake.shutdownRequested.get)

  private def snapshotId(doctorReport: String): Option[Long] =
    val pattern = java.util.regex.Pattern.compile("snapshot id: (\\d+)")
    val matcher = pattern.matcher(doctorReport)
    if matcher.find() then Some(matcher.group(1).toLong) else None
