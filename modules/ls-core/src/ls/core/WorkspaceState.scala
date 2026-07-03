package ls.core

import java.net.URI
import java.nio.file.{Files, Path}

import scala.jdk.CollectionConverters.*
import scala.util.control.NonFatal

import ch.epfl.scala.bsp4j.InitializeBuildResult

import ls.bsp.{
  BspClientHandlers,
  BspCompileOutcome,
  BspDiscovery,
  BspProjectModel,
  BspSession,
  ProjectModelLoader
}
import ls.index.{LsError, LsException}
import ls.pc.{
  PcFacade,
  PcPluginConfigLoader,
  PcPluginInitContext,
  PcPluginManager,
  PcSettings,
  PcTargetConfig
}
import ls.postings.{SegmentReader, SnapshotManager}
import ls.rename.{
  CompileService,
  DocumentHighlightService,
  QueryOrchestrator,
  ReferencesEngine,
  RenameEngine
}
import ls.rename.ingest.{IngestPipeline, IngestReport, WorkspaceTargets}
import ls.sqlite.MetaStore

/** Compile hook over the live BSP session ([[ls.rename.CompileService]]). */
final class BspCompileService(session: BspSession) extends CompileService:
  def compile(targets: Seq[String]): BspCompileOutcome =
    if targets.isEmpty then BspCompileOutcome.Ok(None)
    else session.compile(targets.toVector)

/** Compile hook when no BSP connection exists: a typed failure, never a
  * pretend success (rename requires a fresh compile).
  */
object NoBspCompileService extends CompileService:
  def compile(targets: Seq[String]): BspCompileOutcome =
    throw LsException(LsError.CompileFailed("<no BSP connection>"))

/** Everything a bootstrapped server holds: the store stack, the query
  * engines, the (optional) build connection and the PC facade.
  */
final class CoreServices(
    val workspaceRoot: Path,
    val storageRoot: Path,
    val meta: MetaStore,
    val snapshots: SnapshotManager,
    val pipeline: IngestPipeline,
    val orchestrator: QueryOrchestrator,
    val references: ReferencesEngine,
    val highlights: DocumentHighlightService,
    val rename: RenameEngine,
    val compiler: CompileService,
    val session: Option[BspSession],
    val serverInfo: Option[InitializeBuildResult],
    val model: Option[BspProjectModel],
    val workspaceTargets: WorkspaceTargets,
    val pc: PcFacade,
    val pcConfigs: Map[String, PcTargetConfig],
    /** Normalized `file://` URI -> owning bspId, from the project model. */
    val uriToTarget: Map[String, String],
    val uris: WorkspaceUris,
    val notes: Vector[String]
):
  def indexableBspIds: Vector[String] = workspaceTargets.targets.map(_.bspId)

  def close(): Unit =
    def quietly(body: => Unit): Unit =
      try body
      catch case NonFatal(_) => ()
    quietly(pc.shutdown())
    session.foreach(s => quietly(s.shutdown()))
    quietly(snapshots.close())
    quietly(meta.close())

/** Server lifecycle state. `NotReady` before/without bootstrap, `Failed` when
  * bootstrap crashed hard (the stores could not even open), `Ready` with the
  * wired services otherwise — including BSP-less workspaces, which stay
  * queryable over whatever the recovered index knows and answer everything
  * else with typed errors.
  */
enum WorkspaceState:
  case NotReady(detail: String)
  case Failed(detail: String)
  case Ready(services: CoreServices)

  def ready: Option[CoreServices] = this match
    case Ready(s) => Some(s)
    case _ => None

  def statusLine: String = this match
    case NotReady(d) => s"not ready: $d"
    case Failed(d) => s"bootstrap failed: $d"
    case Ready(_) => "ready"

object Bootstrap:

  /** Wiring knobs. `connectBsp` exists so tests can serve an in-process BSP
    * server over pipes; production discovers the `.bsp` connection files and
    * launches the server process.
    */
  final case class Config(
      connectBsp: (Path, BspClientHandlers) => Option[BspSession] = defaultConnect,
      pcRequestTimeoutMillis: Long = 15000L,
      log: String => Unit = msg => System.err.println(s"[scala3-bsp-semantic-ls] $msg"),
      /** Sink for LSP diagnostics; the server wires it to its LanguageClient so
        * build diagnostics reach the editor. Defaults to dropping notifications.
        */
      publishDiagnostics: org.eclipse.lsp4j.PublishDiagnosticsParams => Unit = _ => (),
      /** Invoked when the build server reports that build targets changed; the
        * server reloads its project model and re-ingests. Defaults to a no-op.
        */
      onBuildTargetsChanged: () => Unit = () => ()
  )

  def defaultConnect(root: Path, handlers: BspClientHandlers): Option[BspSession] =
    BspDiscovery.pick(root).map(file => BspSession.launch(root, file.details, handlers))

  def storageRootOf(workspaceRoot: Path): Path =
    workspaceRoot.resolve(".scala3-bsp-semantic-ls")

  /** Runs the full workspace bootstrap (plan Phase 2/3 wire-up):
    *
    *   1. open the store stack (MetaStore + SnapshotManager + pipeline +
    *      orchestrator over the given overlay);
    *   2. startup recovery: re-mmap the manifest's active segment and publish
    *      it without re-ingesting;
    *   3. BSP: discover/launch (or the injected connector), initialize, load
    *      the project model, register PC targets with the classpath from the
    *      buildTarget/scalacOptions response;
    *   4. initial full ingest when indexable targets exist.
    *
    * Missing `.bsp` or missing SemanticDB are clean skips recorded in the
    * notes (surfaced by the doctor); only a store-stack failure yields
    * [[WorkspaceState.Failed]].
    */
  def run(workspaceRoot: Path, config: Config, docs: DocumentStore, overlay: PcOverlay): WorkspaceState =
    val notes = Vector.newBuilder[String]

    val storage = storageRootOf(workspaceRoot)
    var meta: MetaStore | Null = null
    var snapshots: SnapshotManager | Null = null
    try
      Files.createDirectories(storage)
      meta = MetaStore.open(storage.resolve("meta.sqlite"))
      snapshots = SnapshotManager(storage.resolve("postings"))
    catch
      case NonFatal(t) =>
        if meta != null then
          try meta.nn.close()
          catch case NonFatal(_) => ()
        return WorkspaceState.Failed(s"could not open index stores under $storage: ${describe(t)}")

    val theMeta = meta.nn
    val theSnapshots = snapshots.nn
    var livePc: PcFacade | Null = null
    var liveSession: BspSession | Null = null
    try
      // --- startup recovery: publish the active segment without re-ingesting ---
      val activeSegmentPath =
        theMeta.activeSegment() match
          case Some(seg) =>
            val path = Path.of(seg.path)
            try
              val reader = SegmentReader.open(path)
              theSnapshots.publish(reader)
              notes += s"recovered postings segment ${seg.segmentId} (${seg.path})"
            catch
              case NonFatal(t) =>
                notes += s"active segment ${seg.segmentId} could not be recovered: ${describe(t)}; a re-ingest will rebuild it"
            Some(path)
          case None =>
            notes += "no active postings segment; first ingest will create one"
            None

      // --- startup janitor: reclaim orphan segment dirs and writer debris left
      // by a previous process, never touching the manifest-active segment ---
      val reclaimed = theSnapshots.cleanupOrphans(activeSegmentPath)
      if reclaimed.nonEmpty then
        notes += s"startup janitor reclaimed ${reclaimed.length} orphan segment director${if reclaimed.length == 1 then "y" else "ies"}"

      val pipeline = IngestPipeline(theMeta, theSnapshots)
      val orchestrator = QueryOrchestrator(theMeta, theSnapshots, pipeline, overlay)

      // --- PC facade (plugins first, then the facade over them) ---
      val settings = PcSettings
        .forWorkspace(workspaceRoot)
        .copy(requestTimeoutMillis = config.pcRequestTimeoutMillis)
      Files.createDirectories(settings.generatedSourcesRoot)
      val pluginManager = new PcPluginManager(
        PcPluginInitContext(Some(workspaceRoot), settings.generatedSourcesRoot, m => config.log(s"pc-plugin: $m"))
      )
      val pluginConfigFile = PcPluginConfigLoader.defaultPath(workspaceRoot)
      if Files.isRegularFile(pluginConfigFile) then
        try
          pluginManager.applyConfig(PcPluginConfigLoader.load(pluginConfigFile))
          notes += s"applied PC plugin config $pluginConfigFile"
        catch
          case NonFatal(t) =>
            notes += s"PC plugin config $pluginConfigFile could not be loaded: ${describe(t)}"
      val pc = new PcFacade(pluginManager, settings)
      livePc = pc

      // --- BSP connection + project model ---
      val diagnosticRouter = DiagnosticRouter(config.publishDiagnostics)
      val handlers = BspClientHandlers(
        onDiagnostics = diagnosticRouter.accept,
        onDidChangeBuildTarget = _ => config.onBuildTargetsChanged(),
        onLogMessage = p => config.log(s"bsp: ${p.getMessage}"),
        onServerStderr = line => config.log(s"bsp-stderr: $line")
      )
      val session =
        try
          val s = config.connectBsp(workspaceRoot, handlers)
          s.foreach(liveSession = _)
          s
        catch
          case NonFatal(t) =>
            notes += s"BSP connection failed: ${describe(t)}"
            None

      var serverInfo: Option[InitializeBuildResult] = None
      var model: Option[BspProjectModel] = None
      var workspaceTargets = WorkspaceTargets.empty
      var pcConfigs = Map.empty[String, PcTargetConfig]
      var uriToTarget = Map.empty[String, String]

      session match
        case None =>
          notes += "no BSP connection: global index features stay on the recovered index; PC is disabled"
        case Some(s) =>
          val loaded = loadModel(s, pc, initialize = true, config.log)
          serverInfo = loaded.serverInfo
          model = Some(loaded.model)
          workspaceTargets = loaded.workspaceTargets
          pcConfigs = loaded.pcConfigs
          uriToTarget = loaded.uriToTarget
          loaded.notes.foreach(notes += _)

          // --- initial ingest ---
          if workspaceTargets.targets.isEmpty then
            notes += "initial ingest skipped: no target produces SemanticDB"
          else
            try
              val report = orchestrator.ingest(workspaceTargets)
              notes += ingestSummary(report)
            catch
              case NonFatal(t) =>
                notes += s"initial ingest failed: ${describe(t)}"

      val compiler: CompileService =
        session.map(BspCompileService(_)).getOrElse(NoBspCompileService)
      val references = ReferencesEngine(orchestrator)
      val highlights = DocumentHighlightService(orchestrator)
      val rename = RenameEngine(orchestrator, compiler)

      val sourceroots =
        workspaceTargets.targets.map(_.sourceroot) ++
          theMeta.allTargets().map(t => Path.of(t.sourceroot))
      val uris = WorkspaceUris(sourceroots, orchestrator)

      overlay.install(FacadePcQueries(pc), uris.toFileUri)

      val services = CoreServices(
        workspaceRoot = workspaceRoot,
        storageRoot = storage,
        meta = theMeta,
        snapshots = theSnapshots,
        pipeline = pipeline,
        orchestrator = orchestrator,
        references = references,
        highlights = highlights,
        rename = rename,
        compiler = compiler,
        session = session,
        serverInfo = serverInfo,
        model = model,
        workspaceTargets = workspaceTargets,
        pc = pc,
        pcConfigs = pcConfigs,
        uriToTarget = uriToTarget,
        uris = uris,
        notes = notes.result()
      )
      services.notes.foreach(n => config.log(n))
      WorkspaceState.Ready(services)
    catch
      case NonFatal(t) =>
        def quietly(body: => Unit): Unit =
          try body
          catch case NonFatal(_) => ()
        if livePc != null then quietly(livePc.nn.shutdown())
        if liveSession != null then quietly(liveSession.nn.shutdown())
        quietly(theSnapshots.close())
        quietly(theMeta.close())
        WorkspaceState.Failed(describe(t))

  /** Model-derived state loaded from the BSP session, shared by initial
    * bootstrap and the `buildTarget/didChange` reload.
    */
  private[core] final case class ModelLoad(
      serverInfo: Option[InitializeBuildResult],
      model: BspProjectModel,
      workspaceTargets: WorkspaceTargets,
      pcConfigs: Map[String, PcTargetConfig],
      uriToTarget: Map[String, String],
      notes: Vector[String]
  )

  /** Loads the BSP project model, registers PC targets with their classpaths,
    * and gathers supplementary project info. `initialize` runs `build/initialize`
    * (only at first bootstrap; a reload keeps the existing session). Does not
    * ingest — the caller decides.
    */
  private[core] def loadModel(
      session: BspSession,
      pc: PcFacade,
      initialize: Boolean,
      log: String => Unit
  ): ModelLoad =
    val notes = Vector.newBuilder[String]
    val serverInfo = if initialize then Some(session.initialize()) else None
    val m = ProjectModelLoader.load(session)
    val workspaceTargets = WorkspaceTargets.fromBsp(m)
    val uriToTarget = m.uriToTarget.map((uri, bspId) => Uris.normalize(uri) -> bspId)
    if m.targets.isEmpty then notes += "BSP project model has no Scala 3 targets"

    // PC target registration: classpath comes from the raw buildTarget/scalacOptions
    // response (BspTarget itself does not carry the classpath), plus each target's
    // own class directory.
    val classpathOf: Map[String, Vector[Path]] =
      if m.targets.isEmpty then Map.empty
      else
        try
          session
            .buildTargetScalacOptions(m.targets.map(_.bspId))
            .map { item =>
              val entries = Option(item.getClasspath)
                .map(_.asScala.toVector)
                .getOrElse(Vector.empty)
                .flatMap(u =>
                  try Some(Path.of(URI.create(u)))
                  catch case NonFatal(_) => None
                )
              item.getTarget.getUri -> entries
            }
            .toMap
        catch
          case NonFatal(t) =>
            notes += s"buildTarget/scalacOptions classpath fetch failed: ${describe(t)}"
            Map.empty
    var pcConfigs = Map.empty[String, PcTargetConfig]
    for t <- m.targets do
      val classpath = (classpathOf.getOrElse(t.bspId, Vector.empty) :+ t.classDirectory).distinct
      val cfg = PcTargetConfig(
        bspId = t.bspId,
        classpath = classpath,
        scalacOptions = pcOptions(t.scalacOptions),
        sourceDirs = Vector.empty,
        scalaVersion = t.scalaVersion
      )
      pc.registerTarget(cfg)
      pcConfigs = pcConfigs.updated(t.bspId, cfg)

    m.unavailableTargets.foreach(t => notes += LsError.IndexUnavailable(t.bspId).message)

    // Supplementary project facts, best-effort and capability-gated (never crash).
    val indexableIds = workspaceTargets.targets.map(_.bspId)
    try
      session
        .dependencySources(indexableIds)
        .foreach(items => notes += s"dependency sources: ${items.length} targets")
      session
        .outputPaths(indexableIds)
        .foreach(items => notes += s"output paths: ${items.length} targets")
    catch case NonFatal(t) => notes += s"supplementary BSP info failed: ${describe(t)}"

    ModelLoad(serverInfo, m, workspaceTargets, pcConfigs, uriToTarget, notes.result())

  def ingestSummary(report: IngestReport): String =
    s"ingest: segment ${report.segmentId}, ${report.docsIndexed} docs " +
      s"(${report.docsShared} shared, ${report.docsStale} stale, ${report.docsSkipped} skipped), " +
      s"${report.symbolCount} symbols, ${report.refGroupCount} ref groups, " +
      s"${report.renameGroupCount} rename groups in ${report.durationMs}ms"

  /** Options handed to the PC: SemanticDB generation and sourceroot flags are
    * build-side concerns and are stripped (the PC must never produce
    * SemanticDB, plan 4.3).
    */
  private[core] def pcOptions(scalacOptions: Vector[String]): Vector[String] =
    val twoToken = Set("-semanticdb-target", "-sourceroot")
    val out = Vector.newBuilder[String]
    var i = 0
    while i < scalacOptions.length do
      val opt = scalacOptions(i)
      if opt == "-Xsemanticdb" || opt == "-Ysemanticdb" then ()
      else if twoToken.contains(opt) && i + 1 < scalacOptions.length then i += 1
      else if twoToken.exists(f => opt.startsWith(f + ":")) then ()
      else out += opt
      i += 1
    out.result()

  private def describe(t: Throwable): String =
    val cls = t.getClass.getSimpleName
    Option(t.getMessage).filter(_.nonEmpty).map(m => s"$cls: $m").getOrElse(cls)
