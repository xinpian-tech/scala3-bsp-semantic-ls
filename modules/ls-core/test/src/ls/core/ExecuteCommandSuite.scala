package ls.core

import java.nio.file.{Files, Path}
import java.util.concurrent.{ExecutionException, TimeUnit}

import ls.bsp.BspCompileOutcome
import ls.pc.{PcFacade, PcPluginInitContext, PcPluginManager, PcSettings}
import ls.postings.SnapshotManager
import ls.rename.{
  CompileService,
  DocumentHighlightService,
  QueryOrchestrator,
  ReferencesEngine,
  RenameEngine
}
import ls.rename.ingest.{IngestPipeline, WorkspaceTargets}
import ls.sqlite.MetaStore
import org.eclipse.lsp4j.ExecuteCommandParams
import org.eclipse.lsp4j.jsonrpc.ResponseErrorException

/** executeCommand dispatch against a stubbed Ready state (no BSP, no PC
  * instances, empty index).
  */
class ExecuteCommandSuite extends munit.FunSuite:

  private final class RecordingCompiler extends CompileService:
    var compiled: Vector[Seq[String]] = Vector.empty
    var outcome: BspCompileOutcome = BspCompileOutcome.Ok(None)
    def compile(targets: Seq[String]): BspCompileOutcome =
      compiled :+= targets
      outcome

  private var toClose: List[() => Unit] = Nil

  override def afterAll(): Unit = toClose.foreach(f => f())

  private def makeServer(compiler: CompileService): ScalaLs =
    val dir = Files.createTempDirectory("ls-core-exec")
    val meta = MetaStore.open(dir.resolve("meta.sqlite"))
    val snapshots = SnapshotManager(dir.resolve("postings"))
    val pipeline = IngestPipeline(meta, snapshots)
    val orchestrator = QueryOrchestrator(meta, snapshots, pipeline)
    val pluginManager = new PcPluginManager(
      PcPluginInitContext(None, Files.createTempDirectory("ls-core-exec-gen"))
    )
    val pc = new PcFacade(pluginManager, PcSettings.ephemeral())
    val services = CoreServices(
      workspaceRoot = dir,
      storageRoot = dir,
      meta = meta,
      snapshots = snapshots,
      pipeline = pipeline,
      orchestrator = orchestrator,
      references = ReferencesEngine(orchestrator),
      highlights = DocumentHighlightService(orchestrator),
      rename = RenameEngine(orchestrator, compiler),
      compiler = compiler,
      session = None,
      serverInfo = None,
      model = None,
      workspaceTargets = WorkspaceTargets(
        Vector(
          ls.rename.ingest.TargetSpec(
            bspId = "stub://target/a",
            semanticdbRoot = dir.resolve("out"),
            sourceroot = dir
          )
        )
      ),
      pc = pc,
      pcConfigs = Map.empty,
      uriToTarget = Map.empty,
      uris = WorkspaceUris(Vector(dir), orchestrator),
      notes = Vector("stub bootstrap note")
    )
    val server = new ScalaLs(ScalaLs.Config(exitProcessOnExit = false))
    server.injectStateForTests(WorkspaceState.Ready(services))
    toClose ::= (() => services.close())
    server

  private def run(server: ScalaLs, command: String): String =
    server.getWorkspaceService
      .executeCommand(new ExecuteCommandParams(command, java.util.List.of()))
      .get(30, TimeUnit.SECONDS)
      .asInstanceOf[String]

  test("doctor returns the plan-19 report with all section headings"):
    val server = makeServer(new RecordingCompiler)
    val report = run(server, ScalaLs.Commands.Doctor)
    for heading <- List("Runtime:", "Nix:", "BSP:", "SemanticDB:", "SQLite:", "Postings:", "PC:", "PC Plugins:") do
      assert(report.contains(heading), s"missing '$heading' in:\n$report")
    assert(report.contains("stub bootstrap note"), report)

  test("doctor works before bootstrap (NotReady state)"):
    val server = new ScalaLs(ScalaLs.Config(exitProcessOnExit = false))
    val report = run(server, ScalaLs.Commands.Doctor)
    assert(report.contains("not ready"), report)
    assert(report.contains("Runtime:"), report)
    assert(report.contains("unavailable"), report)

  test("compile dispatches to the compile service with the indexable targets"):
    val compiler = new RecordingCompiler
    val server = makeServer(compiler)
    val result = run(server, ScalaLs.Commands.Compile)
    assert(result.startsWith("compile ok"), result)
    assertEquals(compiler.compiled, Vector(Seq("stub://target/a")))

  test("compile reports a failed outcome"):
    val compiler = new RecordingCompiler
    compiler.outcome = BspCompileOutcome.Failed(ch.epfl.scala.bsp4j.StatusCode.ERROR, None)
    val server = makeServer(compiler)
    val result = run(server, ScalaLs.Commands.Compile)
    assert(result.startsWith("compile failed"), result)

  test("reindex runs a full ingest and reports the summary"):
    val server = makeServer(new RecordingCompiler)
    val result = run(server, ScalaLs.Commands.Reindex)
    assert(result.startsWith("ingest: segment"), result)
    assert(result.contains("0 docs"), result)

  test("pcPluginStatus renders the plugin report"):
    val server = makeServer(new RecordingCompiler)
    val result = run(server, ScalaLs.Commands.PcPluginStatus)
    assert(result.contains("compiler plugins: none"), result)
    assert(result.contains("service plugins: none"), result)
    assert(result.contains("disabled plugins: none"), result)

  test("unknown command fails with a ResponseErrorException"):
    val server = makeServer(new RecordingCompiler)
    val ex = intercept[ExecutionException] {
      server.getWorkspaceService
        .executeCommand(new ExecuteCommandParams("scala3SemanticLs.nope", java.util.List.of()))
        .get(30, TimeUnit.SECONDS)
    }
    assert(ex.getCause.isInstanceOf[ResponseErrorException], ex.getCause.toString)
