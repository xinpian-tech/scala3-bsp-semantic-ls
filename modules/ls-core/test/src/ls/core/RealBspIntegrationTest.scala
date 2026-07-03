package ls.core

import java.io.BufferedReader
import java.io.InputStreamReader
import java.io.{PipedInputStream, PipedOutputStream}
import java.nio.charset.StandardCharsets
import java.nio.file.{FileVisitResult, Files, Path, SimpleFileVisitor, StandardCopyOption}
import java.nio.file.attribute.BasicFileAttributes
import java.util.concurrent.TimeUnit

import scala.concurrent.duration.{Duration, DurationInt}
import scala.jdk.CollectionConverters.*

import org.eclipse.lsp4j.*
import org.eclipse.lsp4j.launch.LSPLauncher
import org.eclipse.lsp4j.services.LanguageServer

import ls.bsp.{BspDiscovery, BspSession, BspSessionConfig}
import ls.index.Span

/** Plan 20 Phase 2 acceptance against a REAL BSP server: Mill itself.
  *
  * Gated by `LS_REAL_BSP_IT=1` (run `scripts/it-real-bsp.sh` inside
  * `nix develop`); skipped otherwise so the ordinary test run stays hermetic.
  *
  * The suite copies `it/sample-workspace` (two Scala 3.8.4 modules, `b`
  * depends on `a`, `-Xsemanticdb -sourceroot <ws>`) to a temp directory, runs
  * the real `mill mill.bsp.BSP/install` + `mill __.compile` there, then
  * boots [[ScalaLs]] exactly like [[LsEndToEndTest]] — except bootstrap goes
  * through [[BspDiscovery]] and [[BspSession.launch]], i.e. the production
  * path over the generated `.bsp/mill-bsp.json`, spawning the real
  * `mill --bsp` server process.
  *
  * Real mill-bsp behaviors this test encodes (found empirically, they differ
  * from the in-process fake server):
  *
  *   - Mill 1.1.2 BSP mode evaluates into a SEPARATE output directory
  *     `.bsp/out` (not `out/`), so the CLI pre-compile does NOT produce the
  *     SemanticDB the BSP-reported targetroots point at. The index only fills
  *     after a compile REQUESTED OVER BSP — the suite drives
  *     `scala3SemanticLs.compile` + `scala3SemanticLs.reindex` first, exactly
  *     what a real editor session goes through.
  *   - Mill exposes its own build definition as a Scala 3 target
  *     (`.../mill-build`, Scala 3.8.1) without `-Xsemanticdb`: one
  *     IndexUnavailable target is the CORRECT steady state, not zero.
  *   - Mill also advertises a `mill-synthetic-root-target` with no `scala`
  *     languageId; the project model must filter it (and does).
  */
class RealBspIntegrationTest extends munit.FunSuite:

  override def munitTimeout: Duration = 900.seconds

  private val enabled = sys.env.get("LS_REAL_BSP_IT").contains("1")

  private val greetingUri = "a/src/pkga/Greeting.scala"
  private val insideUri = "a/src/pkga/Inside.scala"
  private val consumerUri = "b/src/pkgb/Consumer.scala"
  private val otherUri = "b/src/pkgb/Other.scala"
  private def allUris = Vector(greetingUri, insideUri, consumerUri, otherUri)

  private object env:
    lazy val repoRoot: Path =
      def containsSample(p: Path) = Files.isDirectory(p.resolve("it").resolve("sample-workspace"))
      val fromEnv = (sys.env.get("LS_REPO_ROOT") ++ sys.env.get("MILL_WORKSPACE_ROOT"))
        .map(Path.of(_).toAbsolutePath.normalize)
        .find(containsSample)
      fromEnv.getOrElse {
        var p: Path | Null = Path.of("").toAbsolutePath
        while p != null && !containsSample(p.nn) do p = p.nn.getParent
        assert(
          p != null,
          "it/sample-workspace not found: set LS_REPO_ROOT to the repository root"
        )
        p.nn
      }

    /** Temp copy of the sample workspace with the real mill build run in it:
      * `.bsp/mill-bsp.json` installed and `__.compile` green.
      */
    lazy val ws: E2eFixture.Ws =
      val sample = repoRoot.resolve("it").resolve("sample-workspace")
      val root = Files.createTempDirectory("ls-real-bsp-it-").toRealPath()
      copyTree(sample, root)
      runMill(root, "mill.bsp.BSP/install")
      runMill(root, "__.compile")
      assert(
        Files.isRegularFile(root.resolve(".bsp").resolve("mill-bsp.json")),
        "mill.bsp.BSP/install did not write .bsp/mill-bsp.json"
      )
      E2eFixture.Ws(root)

    lazy val server = new ScalaLs(
      ScalaLs.Config(
        bootstrap = Bootstrap.Config(
          // The production path: discover .bsp/mill-bsp.json and launch its
          // argv as a child process, only with test-friendly timeouts (a real
          // mill BSP compile evaluates the whole build in .bsp/out).
          connectBsp = (root, handlers) =>
            BspDiscovery.pick(root).map(file =>
              BspSession.launch(
                root,
                file.details,
                handlers,
                BspSessionConfig(requestTimeout = 300.seconds)
              )
            ),
          pcRequestTimeoutMillis = 120000L,
          log = msg => System.err.println(s"[real-bsp-it] $msg")
        ),
        debounceMillis = 150L,
        exitProcessOnExit = false
      )
    )

    // LSP pipes: real LSPLauncher on both ends, exactly like LsEndToEndTest.
    lazy val proxy: LanguageServer =
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
      clientLauncher.getRemoteProxy

    lazy val initResult: InitializeResult =
      val params = new InitializeParams()
      params.setRootUri(Uris.toUri(ws.root))
      val result = proxy.initialize(params).get(60, TimeUnit.SECONDS)
      proxy.initialized(new InitializedParams())
      assert(server.awaitBootstrap(600000L), "bootstrap did not finish in time")
      result

    /** Mill BSP evaluates in `.bsp/out`, so the SemanticDB the model points
      * at does not exist until a compile is requested OVER the BSP session.
      * Drives the real editor-session flow once: compile + reindex.
      */
    lazy val readyIndex: String =
      val _ = initResult
      val compileResult = executeCommand(ScalaLs.Commands.Compile)
      assert(compileResult.startsWith("compile ok"), s"real BSP compile failed: $compileResult")
      val reindexResult = executeCommand(ScalaLs.Commands.Reindex)
      assert(reindexResult.contains("4 docs"), s"expected all 4 sample docs ingested: $reindexResult")
      reindexResult

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
      .get(600, TimeUnit.SECONDS)
      .asInstanceOf[String]

  override def afterAll(): Unit =
    if enabled then
      try env.proxy.shutdown().get(60, TimeUnit.SECONDS)
      catch case _: Exception => ()

  // ------------------------------------------------------------------ tests

  test("doctor reports the real mill BSP server and its Scala 3 targets"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")
    val _ = env.initResult
    val report = executeCommand(ScalaLs.Commands.Doctor)
    assert(report.contains("state: ready"), report)
    // (a) the real server identity from build/initialize
    assert(report.contains("server: mill-bsp"), report)
    // (a) >= 2 Scala 3 targets (a, b; mill also exposes its own mill-build
    // meta-target as Scala 3)
    val scala3Count =
      "Scala 3 targets: (\\d+)".r.findFirstMatchIn(report).map(_.group(1).toInt)
    assert(scala3Count.exists(_ >= 2), s"expected >=2 Scala 3 targets in:\n$report")
    val rootUri = Uris.toUri(ws.root).stripSuffix("/")
    for name <- Vector("a", "b") do
      assert(report.contains(s"$rootUri/$name"), s"target $name missing in:\n$report")
    // (a) adapted from "0 IndexUnavailable": mill-bsp REALLY advertises the
    // build definition itself (mill-build, Scala 3.8.1, no -Xsemanticdb) as
    // a build target, so exactly that one target is IndexUnavailable. The
    // sample's own modules a and b must all be indexable.
    val unavailable =
      "IndexUnavailable targets: ([^\\n]*)".r.findFirstMatchIn(report).map(_.group(1).trim)
    assertEquals(
      unavailable,
      Some(s"1 ($rootUri/mill-build)"),
      s"expected only the mill-build meta-target to be IndexUnavailable in:\n$report"
    )

  test("compile over the real BSP session fills the index (separate .bsp/out)"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")
    val summary = env.readyIndex
    assert(summary.startsWith("ingest: segment"), summary)

  test("workspace/symbol finds the class defined in module a"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")
    val _ = env.readyIndex
    val result = wsService.symbol(new WorkspaceSymbolParams("Greeting")).get(60, TimeUnit.SECONDS)
    assert(result.isRight, "expected WorkspaceSymbol list")
    val symbols = result.getRight.asScala.toVector
    val greeting = symbols.filter(_.getName == "Greeting")
    assert(greeting.nonEmpty, symbols.map(_.getName).toString)
    assert(
      greeting.exists(s => s.getLocation.getLeft.getUri == ws.fileUri(greetingUri)),
      greeting.map(_.getLocation).toString
    )

  test("references on a usage in b returns the exact cross-module, cross-file set"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")
    val _ = env.readyIndex
    val params = new ReferenceParams(
      textDoc(consumerUri),
      position(consumerUri, "message"),
      new ReferenceContext(true)
    )
    val locations = docsService.references(params).get(120, TimeUnit.SECONDS).asScala.toVector
    val expected = allUris.map(uri => locationOf(uri, ws.tokenSpan(uri, "message", 0)))
    assertEquals(
      locations.toSet,
      expected.toSet,
      s"expected exactly the 4 message occurrences, got $locations"
    )
    assertEquals(locations.length, 4, locations.toString)

  test("rename compiles through the real BSP server and edits both modules"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")
    val _ = env.readyIndex
    val params = new RenameParams(
      textDoc(consumerUri),
      position(consumerUri, "message"),
      "note"
    )
    val edit = docsService.rename(params).get(600, TimeUnit.SECONDS)
    val changes = edit.getChanges
    assertEquals(changes.keySet.asScala.toSet, allUris.map(ws.fileUri).toSet)
    for uri <- allUris do
      val edits = changes.get(ws.fileUri(uri)).asScala.toVector
      assertEquals(edits.length, 1, s"$uri: $edits")
      assertEquals(edits.head.getNewText, "note", s"$uri: $edits")
      assertEquals(
        LspConvert.span(edits.head.getRange),
        ws.tokenSpan(uri, "message", 0),
        s"$uri: $edits"
      )

  test("completion works on a dirty buffer against the real classpath"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")
    val _ = env.readyIndex
    val dirtyText = ws.sourceText(consumerUri) + "  val q = greeting.mess\n"
    docsService.didOpen(
      new DidOpenTextDocumentParams(
        new TextDocumentItem(ws.fileUri(consumerUri), "scala", 1, dirtyText)
      )
    )
    val line = dirtyText.linesIterator.length - 1
    val character = "  val q = greeting.mess".length
    val params = new CompletionParams(textDoc(consumerUri), new Position(line, character))
    val result = docsService.completion(params).get(180, TimeUnit.SECONDS)
    val items =
      if result.isRight then result.getRight.getItems.asScala.toVector
      else result.getLeft.asScala.toVector
    assert(items.exists(_.getLabel.startsWith("message")), items.map(_.getLabel).toString)

  // ------------------------------------------------------------- plumbing

  /** Recursive copy skipping build droppings, in case the sample workspace
    * in the repository was ever built in place.
    */
  private def copyTree(from: Path, to: Path): Unit =
    val skipped = Set("out", ".bsp", ".scala3-bsp-semantic-ls")
    Files.walkFileTree(
      from,
      new SimpleFileVisitor[Path]:
        override def preVisitDirectory(dir: Path, attrs: BasicFileAttributes): FileVisitResult =
          if dir != from && skipped.contains(dir.getFileName.toString) then
            FileVisitResult.SKIP_SUBTREE
          else
            Files.createDirectories(to.resolve(from.relativize(dir).toString))
            FileVisitResult.CONTINUE
        override def visitFile(file: Path, attrs: BasicFileAttributes): FileVisitResult =
          Files.copy(
            file,
            to.resolve(from.relativize(file).toString),
            StandardCopyOption.REPLACE_EXISTING
          )
          FileVisitResult.CONTINUE
    )
    ()

  /** Runs `mill --no-daemon <args>` from PATH in `cwd`, streaming output to
    * stderr on failure. `--no-daemon` because the mill daemon is flaky in
    * throwaway temp directories; the BSP server later launched from
    * `.bsp/mill-bsp.json` runs `mill --bsp`, which is daemon-less by design.
    */
  private def runMill(cwd: Path, args: String*): Unit =
    val cmd = Vector("mill", "--no-daemon") ++ args
    val pb = new ProcessBuilder(cmd.asJava)
    pb.directory(cwd.toFile)
    pb.redirectErrorStream(true)
    val process = pb.start()
    val output = new StringBuilder
    val reader = new BufferedReader(
      new InputStreamReader(process.getInputStream, StandardCharsets.UTF_8)
    )
    val pump = new Thread(
      () =>
        var line = reader.readLine()
        while line != null do
          output.append(line).append('\n')
          line = reader.readLine()
      ,
      "real-bsp-it-mill-output"
    )
    pump.setDaemon(true)
    pump.start()
    val finished = process.waitFor(10, TimeUnit.MINUTES)
    if !finished then process.destroyForcibly()
    pump.join(5000)
    assert(
      finished && process.exitValue() == 0,
      s"'${cmd.mkString(" ")}' in $cwd ${if finished then s"exited ${process.exitValue()}" else "timed out"}:\n$output"
    )
