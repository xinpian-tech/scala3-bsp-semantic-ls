package ls.core

import java.io.{BufferedReader, InputStreamReader, PipedInputStream, PipedOutputStream}
import java.nio.charset.StandardCharsets
import java.nio.file.attribute.BasicFileAttributes
import java.nio.file.{FileVisitResult, Files, Path, SimpleFileVisitor, StandardCopyOption}
import java.util.concurrent.TimeUnit

import scala.concurrent.duration.{Duration, DurationInt}
import scala.jdk.CollectionConverters.*

import org.eclipse.lsp4j.*
import org.eclipse.lsp4j.launch.LSPLauncher
import org.eclipse.lsp4j.services.LanguageServer

import ls.bsp.{BspDiscovery, BspSession, BspSessionConfig}

/** Real-Mill-BSP acceptance against a REAL third-party repository rather than a
  * toy fixture: the vendored pure-Scala subset of `zaozi` under `it/zaozi`
  * (`rvdecoderdb` + `decoder`; see `it/zaozi/NOTICE.md`). Gated by
  * `LS_REAL_BSP_IT=1` (run `scripts/it-real-bsp.sh`); skipped otherwise.
  *
  * The suite copies `it/zaozi`, runs the real `mill mill.bsp.BSP/install` +
  * `mill __.compile` there, then boots [[ScalaLs]] over the generated
  * `.bsp/mill-bsp.json` (the production `mill --bsp` path) and drives a real
  * editor session (compile + reindex) over ~24 genuine SemanticDB documents
  * (1000+ symbols).
  *
  * It asserts the SemanticDB-backed GLOBAL features — doctor, workspace/symbol,
  * and cross-file references — on real symbols. Those are version-independent
  * (they read SemanticDB), so they work even though zaozi targets Scala 3.7.4
  * while this server bundles the 3.8.4 presentation compiler; PC completion is
  * version-skewed on this workspace and is intentionally not asserted here.
  */
class RealBspZaoziTest extends munit.FunSuite:

  override def munitTimeout: Duration = 900.seconds

  private val enabled = sys.env.get("LS_REAL_BSP_IT").contains("1")

  private val bitSetUri = "decoder/src/BitSet.scala"
  private val plaUri = "decoder/src/PLA.scala"

  private object env:
    lazy val repoRoot: Path =
      def containsZaozi(p: Path) = Files.isDirectory(p.resolve("it").resolve("zaozi"))
      val fromEnv = (sys.env.get("LS_REPO_ROOT") ++ sys.env.get("MILL_WORKSPACE_ROOT"))
        .map(Path.of(_).toAbsolutePath.normalize)
        .find(containsZaozi)
      fromEnv.getOrElse {
        var p: Path | Null = Path.of("").toAbsolutePath
        while p != null && !containsZaozi(p.nn) do p = p.nn.getParent
        assert(p != null, "it/zaozi not found: set LS_REPO_ROOT to the repository root")
        p.nn
      }

    /** Temp copy of the vendored zaozi subset with the real mill build run in it. */
    lazy val ws: E2eFixture.Ws =
      val sample = repoRoot.resolve("it").resolve("zaozi")
      val root = Files.createTempDirectory("ls-real-bsp-zaozi-").toRealPath()
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
          connectBsp = (root, handlers) =>
            BspDiscovery
              .pick(root)
              .map(file => BspSession.launch(root, file.details, handlers, BspSessionConfig(requestTimeout = 300.seconds))),
          pcRequestTimeoutMillis = 120000L,
          log = msg => System.err.println(s"[real-bsp-zaozi] $msg")
        ),
        debounceMillis = 150L,
        exitProcessOnExit = false
      )
    )

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

    /** Real editor-session flow once: compile over BSP + reindex. */
    lazy val readyIndex: String =
      val _ = initResult
      val compileResult = executeCommand(ScalaLs.Commands.Compile)
      assert(compileResult.startsWith("compile ok"), s"real BSP compile failed: $compileResult")
      val reindexResult = executeCommand(ScalaLs.Commands.Reindex)
      val docs = "(\\d+) docs".r.findFirstMatchIn(reindexResult).map(_.group(1).toInt).getOrElse(0)
      assert(docs >= 10, s"expected the real zaozi SemanticDB (>=10 docs) ingested: $reindexResult")
      reindexResult

  private def ws = env.ws
  private def docsService = env.proxy.getTextDocumentService
  private def wsService = env.proxy.getWorkspaceService
  private def textDoc(uri: String) = new TextDocumentIdentifier(ws.fileUri(uri))
  private def position(uri: String, token: String, nth: Int = 0): Position =
    val (line, character) = ws.cursor(uri, token, nth)
    new Position(line, character)

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

  test("doctor reports the real mill BSP server for the zaozi workspace"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP zaozi test")
    val _ = env.initResult
    val report = executeCommand(ScalaLs.Commands.Doctor)
    assert(report.contains("state: ready"), report)
    assert(report.contains("server: mill-bsp"), report)
    val scala3Count = "Scala 3 targets: (\\d+)".r.findFirstMatchIn(report).map(_.group(1).toInt)
    assert(scala3Count.exists(_ >= 2), s"expected >=2 Scala 3 targets in:\n$report")

  test("compile + reindex fills the index from real zaozi SemanticDB"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP zaozi test")
    val summary = env.readyIndex
    assert(summary.startsWith("ingest: segment"), summary)

  test("workspace/symbol finds a real zaozi symbol (BitSet)"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP zaozi test")
    val _ = env.readyIndex
    val result = wsService.symbol(new WorkspaceSymbolParams("BitSet")).get(60, TimeUnit.SECONDS)
    assert(result.isRight, "expected WorkspaceSymbol list")
    val symbols = result.getRight.asScala.toVector
    val bitSet = symbols.filter(_.getName == "BitSet")
    assert(bitSet.nonEmpty, symbols.map(_.getName).take(20).toString)
    assert(
      bitSet.exists(_.getLocation.getLeft.getUri == ws.fileUri(bitSetUri)),
      bitSet.map(_.getLocation).toString
    )

  test("references on a real zaozi symbol span multiple files"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP zaozi test")
    val _ = env.readyIndex
    val params = new ReferenceParams(
      textDoc(plaUri),
      position(plaUri, "BitSet"),
      new ReferenceContext(true)
    )
    val locations = docsService.references(params).get(120, TimeUnit.SECONDS).asScala.toVector
    assert(locations.nonEmpty, "references on BitSet returned nothing")
    val files = locations.map(_.getUri).toSet
    assert(files.size >= 2, s"expected cross-file references, got: $files")
    assert(files.contains(ws.fileUri(bitSetUri)), s"expected a reference in BitSet.scala, got: $files")

  // ------------------------------------------------------------- plumbing

  private def copyTree(from: Path, to: Path): Unit =
    val skipped = Set("out", ".bsp", ".scala3-bsp-semantic-ls")
    Files.walkFileTree(
      from,
      new SimpleFileVisitor[Path]:
        override def preVisitDirectory(dir: Path, attrs: BasicFileAttributes): FileVisitResult =
          if dir != from && skipped.contains(dir.getFileName.toString) then FileVisitResult.SKIP_SUBTREE
          else
            Files.createDirectories(to.resolve(from.relativize(dir).toString))
            FileVisitResult.CONTINUE
        override def visitFile(file: Path, attrs: BasicFileAttributes): FileVisitResult =
          Files.copy(file, to.resolve(from.relativize(file).toString), StandardCopyOption.REPLACE_EXISTING)
          FileVisitResult.CONTINUE
    )
    ()

  private def runMill(cwd: Path, args: String*): Unit =
    val cmd = Vector("mill", "--no-daemon") ++ args
    val pb = new ProcessBuilder(cmd.asJava)
    pb.directory(cwd.toFile)
    pb.redirectErrorStream(true)
    val process = pb.start()
    val output = new StringBuilder
    val reader = new BufferedReader(new InputStreamReader(process.getInputStream, StandardCharsets.UTF_8))
    val pump = new Thread(
      () =>
        var line = reader.readLine()
        while line != null do
          output.append(line).append('\n')
          line = reader.readLine()
      ,
      "real-bsp-zaozi-mill-output"
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
