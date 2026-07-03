package ls.core

import java.io.BufferedReader
import java.io.InputStreamReader
import java.io.{PipedInputStream, PipedOutputStream}
import java.nio.charset.StandardCharsets
import java.nio.file.{FileVisitResult, Files, Path, SimpleFileVisitor, StandardCopyOption}
import java.nio.file.attribute.BasicFileAttributes
import java.util.concurrent.TimeUnit

import scala.concurrent.duration.DurationInt
import scala.jdk.CollectionConverters.*

import org.eclipse.lsp4j.*
import org.eclipse.lsp4j.launch.LSPLauncher
import org.eclipse.lsp4j.services.LanguageServer

import ls.bsp.{BspDiscovery, BspSession, BspSessionConfig}
import ls.index.Span

/** Boot of the language server against a REAL BSP server (Mill itself), used by
  * every gated real-BSP suite. The heavy work — copying `it/sample-workspace`,
  * `mill.bsp.BSP/install` + `mill __.compile`, launching `mill --bsp`, and
  * booting [[ScalaLs]] over LSP pipes — is encapsulated in [[RealBspServer]], so
  * a suite can either share the read-only [[RealBspFixture.shared]] instance
  * (RealBspCoreTest, RealBspIntegrationTest, whose assertions treat the workspace
  * as immutable) or spin up its OWN instance when it mutates sources
  * (RealBspLifecycleTest). This matches the plan's "one workspace fixture per
  * suite" while still booting mill-bsp lazily.
  *
  * Gated by `LS_REAL_BSP_IT=1` (run `scripts/it-real-bsp.sh` inside
  * `nix develop`); the suites `assume(enabled)` so the ordinary run skips them.
  *
  * The sample workspace is three Scala 3.8.4 modules: `a`, `b` (depends on `a`,
  * both `-Xsemanticdb -sourceroot <ws>`), and `c` (built WITHOUT SemanticDB).
  * SemanticDB is mandatory, so `c` — like Mill's own `mill-build` target — is a
  * hard SemanticDB-coverage error, and every request on a `c` source fails.
  */
object RealBspFixture:

  val enabled: Boolean = sys.env.get("LS_REAL_BSP_IT").contains("1")

  val greetingUri = "a/src/pkga/Greeting.scala"
  val insideUri = "a/src/pkga/Inside.scala"
  val consumerUri = "b/src/pkgb/Consumer.scala"
  val otherUri = "b/src/pkgb/Other.scala"
  val widgetUri = "c/src/pkgc/Widget.scala"

  /** The four SemanticDB-indexed sample docs (module `c` is excluded: it emits
    * no SemanticDB).
    */
  val indexedUris: Vector[String] = Vector(greetingUri, insideUri, consumerUri, otherUri)

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

  /** A fresh temp copy of the sample workspace with the real mill build run in
    * it: `.bsp/mill-bsp.json` installed and `__.compile` green. Each call yields
    * an independent workspace so a mutating suite never disturbs another.
    *
    * `extraSources` (uri -> text) and `buildMill` (a full replacement for
    * `build.mill`, or None to keep the sample's) let a suite extend the sample
    * before the real build runs — e.g. adding a source shared by two targets.
    */
  def freshWorkspace(
      extraSources: Map[String, String] = Map.empty,
      buildMill: Option[String] = None
  ): E2eFixture.Ws =
    val sample = repoRoot.resolve("it").resolve("sample-workspace")
    val root = Files.createTempDirectory("ls-real-bsp-it-").toRealPath()
    copyTree(sample, root)
    buildMill.foreach(text => Files.writeString(root.resolve("build.mill"), text))
    for (uri, text) <- extraSources do
      val p = root.resolve(uri)
      Files.createDirectories(p.getParent)
      Files.writeString(p, text)
    runMill(root, "mill.bsp.BSP/install")
    runMill(root, "__.compile")
    assert(
      Files.isRegularFile(root.resolve(".bsp").resolve("mill-bsp.json")),
      "mill.bsp.BSP/install did not write .bsp/mill-bsp.json"
    )
    E2eFixture.Ws(root)

  /** The shared read-only server: boots mill-bsp once for the suites whose
    * assertions treat the sample workspace as immutable.
    */
  lazy val shared: RealBspServer = new RealBspServer(freshWorkspace())

  // Forwarders onto the shared instance so read-only suites keep referring to
  // `RealBspFixture.{ws, proxy, executeCommand, ...}` unchanged.
  def ws: E2eFixture.Ws = shared.ws
  def proxy: LanguageServer = shared.proxy
  def docsService: org.eclipse.lsp4j.services.TextDocumentService = shared.docsService
  def wsService: org.eclipse.lsp4j.services.WorkspaceService = shared.wsService
  def executeCommand(command: String): String = shared.executeCommand(command)
  def textDoc(uri: String): TextDocumentIdentifier = shared.textDoc(uri)
  def position(uri: String, token: String, nth: Int = 0): Position = shared.position(uri, token, nth)
  def locationOf(uri: String, span: Span): Location = shared.locationOf(uri, span)
  def withOpen[A](uri: String, text: String)(body: => A): A = shared.withOpen(uri, text)(body)
  def initResult: InitializeResult = shared.initResult
  def readyIndex: String = shared.readyIndex

  // ------------------------------------------------------------- plumbing

  /** Recursive copy skipping build droppings, in case the sample workspace in
    * the repository was ever built in place.
    */
  def copyTree(from: Path, to: Path): Unit =
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
  def runMill(cwd: Path, args: String*): Unit =
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

/** One booted language server over the real `mill --bsp` for a given workspace
  * copy: LSP pipes, initialize, and (via [[readyIndex]]) the first compile +
  * reindex that a real editor session goes through. Also exposes the recording
  * client's published diagnostics so lifecycle suites can await them.
  */
final class RealBspServer(
    val ws: E2eFixture.Ws,
    expectedDocs: Int = 4,
    withBsp: Boolean = true,
    pcBackendMode: PcBackendMode = PcBackendMode.InProcess
):

  val server: ScalaLs = new ScalaLs(
    ScalaLs.Config(
      bootstrap = Bootstrap.Config(
        pcBackendMode = pcBackendMode,
        // The production path: discover .bsp/mill-bsp.json and launch its argv
        // as a child process, only with test-friendly timeouts (a real mill BSP
        // compile evaluates the whole build in .bsp/out). `withBsp = false` boots
        // WITHOUT a BSP connection — a warm restart that must serve from the
        // recovered persisted index alone.
        connectBsp =
          if withBsp then
            (root, handlers) =>
              BspDiscovery.pick(root).map(file =>
                BspSession.launch(
                  root,
                  file.details,
                  handlers,
                  BspSessionConfig(requestTimeout = 300.seconds)
                )
              )
          else (_, _) => None,
        pcRequestTimeoutMillis = 120000L,
        log = msg => System.err.println(s"[real-bsp-it] $msg")
      ),
      debounceMillis = 150L,
      exitProcessOnExit = false
    )
  )

  /** The LSP client end; its `diagnostics` queue records every
    * `textDocument/publishDiagnostics` the server pushes.
    */
  val client = new RecordingLanguageClient

  /** LSP pipes: real LSPLauncher on both ends, exactly like LsEndToEndTest. */
  val proxy: LanguageServer =
    val lspToServer = new PipedInputStream(1 << 20)
    val lspClientOut = new PipedOutputStream(lspToServer)
    val lspToClient = new PipedInputStream(1 << 20)
    val lspServerOut = new PipedOutputStream(lspToClient)

    val serverLauncher = LSPLauncher.createServerLauncher(server, lspToServer, lspServerOut)
    server.connect(serverLauncher.getRemoteProxy)
    serverLauncher.startListening()

    val clientLauncher = LSPLauncher.createClientLauncher(client, lspToClient, lspClientOut)
    clientLauncher.startListening()
    val remote = clientLauncher.getRemoteProxy
    Runtime.getRuntime.addShutdownHook(new Thread(new Runnable:
      override def run(): Unit =
        try remote.shutdown().get(60, TimeUnit.SECONDS)
        catch case _: Exception => ()
    ))
    remote

  lazy val initResult: InitializeResult =
    val params = new InitializeParams()
    params.setRootUri(Uris.toUri(ws.root))
    val result = proxy.initialize(params).get(60, TimeUnit.SECONDS)
    proxy.initialized(new InitializedParams())
    assert(server.awaitBootstrap(600000L), "bootstrap did not finish in time")
    result

  /** Mill BSP evaluates in `.bsp/out`, so the SemanticDB the model points at
    * does not exist until a compile is requested OVER the BSP session. Drives
    * the real editor-session flow once: compile + reindex.
    */
  lazy val readyIndex: String =
    val _ = initResult
    val compileResult = executeCommand(ScalaLs.Commands.Compile)
    assert(compileResult.startsWith("compile ok"), s"real BSP compile failed: $compileResult")
    val reindexResult = executeCommand(ScalaLs.Commands.Reindex)
    if expectedDocs >= 0 then
      assert(
        reindexResult.contains(s"$expectedDocs docs"),
        s"expected $expectedDocs sample docs ingested: $reindexResult"
      )
    reindexResult

  // --------------------------------------------------------------- helpers

  /** The on-disk postings segments directory of the ready index. */
  def segmentsDir: java.nio.file.Path =
    server.currentState.ready.get.snapshots.segmentsDir

  /** LSP shutdown + exit, releasing the SQLite storage under `ws` so a warm
    * restart can open the same index. `exitProcessOnExit = false` keeps the JVM.
    */
  def shutdown(): Unit =
    try
      proxy.shutdown().get(60, TimeUnit.SECONDS)
      proxy.exit()
    catch case _: Exception => ()

  def docsService: org.eclipse.lsp4j.services.TextDocumentService = proxy.getTextDocumentService
  def wsService: org.eclipse.lsp4j.services.WorkspaceService = proxy.getWorkspaceService

  def textDoc(uri: String) = new TextDocumentIdentifier(ws.fileUri(uri))

  def position(uri: String, token: String, nth: Int = 0): Position =
    val (line, character) = ws.cursor(uri, token, nth)
    new Position(line, character)

  def locationOf(uri: String, span: Span): Location =
    LspConvert.location(ws.fileUri(uri), span)

  def executeCommand(command: String): String =
    wsService
      .executeCommand(new ExecuteCommandParams(command, java.util.List.of()))
      .get(600, TimeUnit.SECONDS)
      .asInstanceOf[String]

  /** Opens `uri` in the PC facade with `text` (a PC request precondition), runs
    * `body`, then closes it so shared index state stays clean.
    */
  def withOpen[A](uri: String, text: String)(body: => A): A =
    docsService.didOpen(
      new DidOpenTextDocumentParams(new TextDocumentItem(ws.fileUri(uri), "scala", 1, text))
    )
    try body
    finally docsService.didClose(new DidCloseTextDocumentParams(textDoc(uri)))
