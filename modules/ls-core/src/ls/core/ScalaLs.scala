package ls.core

import java.nio.file.Path
import java.util.concurrent.{CompletableFuture, CountDownLatch, Executors, TimeUnit}
import java.util.concurrent.atomic.AtomicBoolean

import scala.jdk.CollectionConverters.*
import scala.util.control.NonFatal

import com.google.gson.JsonElement

import ls.bsp.{BspCompileOutcome, BspException}
import ls.index.{DocId, LsError, LsException, Span, TargetId}
import ls.pc.PcTimeoutException
import ls.sqlite.{MetaStore, WorkspaceSymbolHit}
import org.eclipse.lsp4j.*
import org.eclipse.lsp4j.jsonrpc.ResponseErrorException
import org.eclipse.lsp4j.jsonrpc.messages.{Either as JEither, Either3, ResponseError, ResponseErrorCode}
import org.eclipse.lsp4j.services.{
  LanguageClient,
  LanguageClientAware,
  LanguageServer,
  TextDocumentService,
  WorkspaceService
}

/** The LSP server (lsp4j 1.0.0), wiring every lower layer together.
  *
  * Structure note: the text-document and workspace services are separate
  * inner objects rather than extra interfaces on this class. lsp4j 1.0.0
  * declares a Java method named `diagnostic` on BOTH service interfaces
  * (`textDocument/diagnostic` and `workspace/diagnostic`), and lsp4j's
  * annotation scan rejects one object implementing both ("Duplicate RPC
  * method"); the `@JsonDelegate` getters are the supported layout.
  *
  * Threading model:
  *   - PC requests (completion/hover/signatureHelp/definition/typeDefinition
  *     and resolve) run on a cached pool — the PC facade is thread-safe;
  *   - everything touching the metadata store / postings (references,
  *     rename, workspace symbol, documentHighlight, executeCommand, the
  *     debounced compile+ingest job) runs on ONE single-threaded executor,
  *     honoring the [[ls.sqlite.Db]] "never two threads at once" contract;
  *   - document sync notifications are handled inline (map updates only).
  *
  * Bootstrap happens asynchronously on `initialized` (plan: BSP discovery,
  * session launch, project model, PC registration, initial ingest); before
  * it finishes PC requests answer empty and index requests answer typed
  * "not ready" errors — never a crash, never a guess.
  */
final class ScalaLs(val config: ScalaLs.Config = ScalaLs.Config())
    extends LanguageServer
    with LanguageClientAware:

  import ScalaLs.*

  private val docs = new DocumentStore
  private val overlay = new PcOverlay(docs)

  @volatile private var state: WorkspaceState = WorkspaceState.NotReady("initialize has not run")
  @volatile private var workspaceRoot: Option[Path] = None
  @volatile private var client: Option[LanguageClient] = None
  @volatile private var lastCompletionTarget: Option[String] = None
  private val readyLatch = new CountDownLatch(1)
  private val shuttingDown = new AtomicBoolean(false)
  private val ingestCounter = new java.util.concurrent.atomic.AtomicLong(0L)
  private val pendingModelReload = new AtomicBoolean(false)

  private def daemon(name: String): java.util.concurrent.ThreadFactory = r =>
    val t = new Thread(r, name)
    t.setDaemon(true)
    t

  private val pcPool = Executors.newCachedThreadPool(daemon("ls-core-pc"))
  private val indexExecutor = Executors.newSingleThreadExecutor(daemon("ls-core-index"))
  private val scheduler = Executors.newSingleThreadScheduledExecutor(daemon("ls-core-scheduler"))

  private val textDocumentService = new TextDocs
  private val workspaceService = new Workspace

  // ---------------------------------------------------------------- lifecycle

  override def initialize(params: InitializeParams): CompletableFuture[InitializeResult] =
    val rootUri = Option(params.getRootUri)
      .orElse(
        Option(params.getWorkspaceFolders)
          .flatMap(_.asScala.headOption)
          .map(_.getUri)
      )
    workspaceRoot = rootUri.flatMap { uri =>
      try Some(Uris.toPath(uri).toAbsolutePath.normalize)
      catch case NonFatal(_) => None
    }
    if state.ready.isEmpty then
      state = WorkspaceState.NotReady("waiting for the initialized notification")
    CompletableFuture.completedFuture(
      new InitializeResult(capabilities(), new ServerInfo(ServerName, ServerVersion))
    )

  override def initialized(params: InitializedParams): Unit =
    val root = workspaceRoot.getOrElse(Path.of(".").toAbsolutePath.normalize)
    // Route BSP build diagnostics to the connected LSP client, and reload the
    // project model when the build server reports target changes.
    val bootstrapConfig = config.bootstrap.copy(
      publishDiagnostics = p => client.foreach(_.publishDiagnostics(p)),
      onBuildTargetsChanged = () => onBuildTargetsChanged()
    )
    val t = new Thread(
      () =>
        val result =
          try Bootstrap.run(root, bootstrapConfig, docs, overlay)
          catch case NonFatal(t) => WorkspaceState.Failed(t.toString)
        state = result
        result.ready.foreach(replayOpenBuffers)
        readyLatch.countDown()
        // Apply any build-targets change that arrived before bootstrap finished.
        if result.ready.isDefined && pendingModelReload.getAndSet(false) then
          submitIndex(() => reloadBuildModel())
        config.bootstrap.log(s"bootstrap finished: ${result.statusLine}"),
      "ls-core-bootstrap"
    )
    t.setDaemon(true)
    t.start()

  /** Test/embedding hook: true once bootstrap finished (in either outcome). */
  def awaitBootstrap(timeoutMillis: Long): Boolean =
    readyLatch.await(timeoutMillis, TimeUnit.MILLISECONDS)

  /** Whether a display name is present in the persisted workspace-symbol index
    * (an EXACT membership query, not the ranked/limited search). Drives the
    * PC-only overlay: a top-level dirty-buffer symbol is PC-only exactly when
    * this is false.
    */
  private def indexedName(cs: CoreServices)(name: String): Boolean =
    name.nonEmpty && cs.orchestrator.workspaceSymbolNameExists(name)

  /** Test hook: inject a pre-built state instead of running bootstrap. */
  private[core] def injectStateForTests(s: WorkspaceState): Unit =
    state = s
    s.ready.foreach(cs => overlay.install(FacadePcQueries(cs.pc), cs.uris.toFileUri, indexedName(cs)))
    readyLatch.countDown()

  private[core] def currentState: WorkspaceState = state

  /** Test/embedding hook: count of completed background (save-driven) ingests.
    * Non-retaining, so callers can await a re-ingest cycle without holding a
    * postings snapshot.
    */
  private[core] def completedIngests: Long = ingestCounter.get

  override def shutdown(): CompletableFuture[Object] =
    if !shuttingDown.compareAndSet(false, true) then
      return CompletableFuture.completedFuture(null: Object)
    val old = state
    state = WorkspaceState.NotReady("server is shut down")
    val done = CompletableFuture.supplyAsync(
      () =>
        old.ready.foreach(_.close())
        null: Object
      ,
      indexExecutor
    )
    done.whenComplete { (_, _) =>
      scheduler.shutdownNow()
      pcPool.shutdown()
      indexExecutor.shutdown()
      ()
    }
    done

  override def exit(): Unit =
    if config.exitProcessOnExit then System.exit(if shuttingDown.get() then 0 else 1)

  override def connect(languageClient: LanguageClient): Unit =
    client = Option(languageClient)

  // Explicit (annotation-free) overrides of lsp4j's annotated default
  // methods. Without them Scala 3 emits mixin forwarders that COPY the
  // @JsonNotification annotations onto this class, and lsp4j's method scan
  // then rejects the launcher with "Duplicate RPC method $/setTrace".
  override def setTrace(params: SetTraceParams): Unit = ()
  override def cancelProgress(params: WorkDoneProgressCancelParams): Unit = ()

  override def getTextDocumentService: TextDocumentService = textDocumentService
  override def getWorkspaceService: WorkspaceService = workspaceService

  private[core] def capabilities(): ServerCapabilities =
    val caps = new ServerCapabilities()
    caps.setTextDocumentSync(TextDocumentSyncKind.Full)
    val completion = new CompletionOptions(true, java.util.List.of("."))
    caps.setCompletionProvider(completion)
    caps.setHoverProvider(true)
    caps.setSignatureHelpProvider(new SignatureHelpOptions(java.util.List.of("(", ",")))
    caps.setDefinitionProvider(true)
    caps.setTypeDefinitionProvider(true)
    caps.setReferencesProvider(true)
    val rename = new RenameOptions()
    rename.setPrepareProvider(true)
    caps.setRenameProvider(rename)
    caps.setDocumentHighlightProvider(true)
    caps.setWorkspaceSymbolProvider(true)
    caps.setExecuteCommandProvider(new ExecuteCommandOptions(Commands.all.asJava))
    // semanticTokens / inlayHint deliberately absent (plan 3.1: later)
    caps

  // ------------------------------------------------- debounced compile+ingest

  private object jobs:
    val lock = new Object
    var pendingTargets: Set[String] = Set.empty
    var pendingCompile: Boolean = false
    var scheduled: Boolean = false

  /** Debounced (~500ms), single-flight compile+reingest. Saves arriving while
    * a run is in flight collapse into one follow-up run (queue collapse); the
    * single-threaded executors guarantee single-flight.
    */
  private[core] def scheduleBuildJob(targets: Vector[String], compileFirst: Boolean): Unit =
    if shuttingDown.get() then return
    val mustSchedule = jobs.lock.synchronized {
      jobs.pendingTargets ++= targets
      jobs.pendingCompile ||= compileFirst
      if jobs.scheduled then false
      else
        jobs.scheduled = true
        true
    }
    if mustSchedule then
      try
        scheduler.schedule(
          (() => indexExecutor.execute(() => runBuildJob())): Runnable,
          config.debounceMillis,
          TimeUnit.MILLISECONDS
        )
      catch case _: java.util.concurrent.RejectedExecutionException => ()

  private def runBuildJob(): Unit =
    val (targets, doCompile) = jobs.lock.synchronized {
      val t = jobs.pendingTargets
      val c = jobs.pendingCompile
      jobs.pendingTargets = Set.empty
      jobs.pendingCompile = false
      jobs.scheduled = false
      (t.toVector.sorted, c)
    }
    state.ready.foreach { s =>
      if doCompile && targets.nonEmpty then
        try
          s.compiler.compile(targets) match
            case BspCompileOutcome.Ok(_) => ()
            case BspCompileOutcome.Failed(code, _) =>
              log(s"background compile of ${targets.mkString(", ")} failed: $code")
        catch case NonFatal(t) => log(s"background compile failed: $t")
      if s.workspaceTargets.targets.nonEmpty then
        try
          val report = s.orchestrator.ingest(s.workspaceTargets)
          log(Bootstrap.ingestSummary(report))
          ingestCounter.incrementAndGet()
        catch case NonFatal(t) => log(s"background ingest failed: $t")
    }

  private def replayOpenBuffers(s: CoreServices): Unit =
    for
      uri <- docs.openUris
      text <- docs.text(uri)
      bspId <- s.uriToTarget.get(uri)
    do
      try s.pc.didOpen(bspId, uri, text)
      catch case NonFatal(t) => log(s"pc didOpen replay failed for $uri: $t")

  private def submitIndex(task: Runnable): Unit =
    try indexExecutor.execute(task)
    catch case _: java.util.concurrent.RejectedExecutionException => ()

  /** Build server reported that build targets changed. When the server is
    * bootstrapped, reload the project model on the index executor; before that,
    * buffer a single pending reload that is drained once bootstrap publishes
    * Ready (so a change is never dropped and never crashes bootstrap).
    */
  private def onBuildTargetsChanged(): Unit =
    if shuttingDown.get() then return
    if state.ready.isDefined then submitIndex(() => reloadBuildModel())
    else pendingModelReload.set(true)

  /** Refetch the BSP project model and republish an updated `Ready` reusing the
    * durable stores, then re-ingest. Runs on the single index executor.
    */
  private def reloadBuildModel(): Unit =
    state.ready.foreach { s =>
      s.session match
        case None => ()
        case Some(session) =>
          try
            val loaded = Bootstrap.loadModel(session, s.pc, initialize = false, log)
            val sourceroots =
              loaded.workspaceTargets.targets.map(_.sourceroot) ++
                s.meta.allTargets().map(t => Path.of(t.sourceroot))
            val uris = WorkspaceUris(sourceroots, s.orchestrator)
            val updated = new CoreServices(
              workspaceRoot = s.workspaceRoot,
              storageRoot = s.storageRoot,
              meta = s.meta,
              snapshots = s.snapshots,
              pipeline = s.pipeline,
              orchestrator = s.orchestrator,
              references = s.references,
              highlights = s.highlights,
              rename = s.rename,
              compiler = s.compiler,
              session = s.session,
              serverInfo = s.serverInfo,
              model = Some(loaded.model),
              workspaceTargets = loaded.workspaceTargets,
              pc = s.pc,
              pcConfigs = loaded.pcConfigs,
              uriToTarget = loaded.uriToTarget,
              uris = uris,
              notes = s.notes
            )
            state = WorkspaceState.Ready(updated)
            overlay.install(FacadePcQueries(updated.pc), updated.uris.toFileUri, indexedName(updated))
            replayOpenBuffers(updated)
            if updated.workspaceTargets.targets.nonEmpty then
              scheduleBuildJob(Vector.empty, compileFirst = false)
            log("reloaded BSP build target model after buildTarget/didChange")
          catch case NonFatal(t) => log(s"build target model reload failed: $t")
    }

  /** Test hook: fire the build-targets-changed handler directly. */
  private[core] def onBuildTargetsChangedForTest(): Unit = onBuildTargetsChanged()

  /** Test hook: whether a pre-bootstrap build-targets change is buffered. */
  private[core] def pendingModelReloadForTest: Boolean = pendingModelReload.get

  // ----------------------------------------------------------- shared helpers

  private def onPc[A](body: => A): CompletableFuture[A] =
    CompletableFuture.supplyAsync(() => mapErrors(body), pcPool)

  private def onIndex[A](body: => A): CompletableFuture[A] =
    CompletableFuture.supplyAsync(() => mapErrors(body), indexExecutor)

  private def mapErrors[A](body: => A): A =
    try body
    catch
      case e: LsException =>
        throw responseError(ResponseErrorCode.RequestFailed, e.error.message)
      case e: PcTimeoutException =>
        throw responseError(ResponseErrorCode.InternalError, e.getMessage)
      case e: BspException =>
        throw responseError(ResponseErrorCode.RequestFailed, e.error.message)

  private def responseError(code: ResponseErrorCode, message: String): ResponseErrorException =
    new ResponseErrorException(new ResponseError(code, message, null))

  private def requireReady(): CoreServices =
    state match
      case WorkspaceState.Ready(s) => s
      case other =>
        throw responseError(
          ResponseErrorCode.RequestFailed,
          s"workspace is ${other.statusLine}"
        )

  /** PC request precondition: bootstrap done AND the buffer is open in the
    * facade (its target was known). Anything else answers the fallback —
    * empty result, never a crash (plan: PC-less requests must stay typed).
    */
  private def withPcBuffer[A](rawUri: String)(fallback: A)(body: (CoreServices, String) => A): A =
    val uri = Uris.normalize(rawUri)
    state match
      case WorkspaceState.Ready(s) if s.pc.bufferText(uri).isDefined =>
        try body(s, uri)
        catch case _: IllegalStateException => fallback
      case _ => fallback

  private def sdbUriOf(s: CoreServices, rawUri: String): String =
    val uri = Uris.normalize(rawUri)
    s.uris.toSdbUri(uri).getOrElse(throw LsException(LsError.NotIndexed(uri)))

  /** SemanticDB is mandatory: every source is assumed to be compiled with
    * `-Xsemanticdb`. A request on a source whose build target produces no
    * SemanticDB is a hard error (no PC fallback, no empty result) — the same
    * gate for PC and index requests alike. When the workspace is not Ready the
    * per-handler fallback/error path is left unchanged.
    */
  private def requireSemanticdb(rawUri: String): Unit =
    state match
      case WorkspaceState.Ready(s) =>
        val uri = Uris.normalize(rawUri)
        val ownedByLiveTarget = s.uriToTarget.get(uri)
        val indexableInModel =
          ownedByLiveTarget.exists(bspId => s.workspaceTargets.targets.exists(_.bspId == bspId))
        // The persisted-index fallback lets a warm restart with NO BSP connection
        // serve already-indexed sources. It must NOT override a LIVE model that
        // owns this uri via a non-indexable target (e.g. `-Xsemanticdb` removed):
        // that stays a hard error, never answered from stale persisted rows. So it
        // applies only when no live target owns the uri, and counts ACTIVE
        // documents only.
        def indexedOnDisk =
          ownedByLiveTarget.isEmpty &&
            s.uris.toSdbUri(uri).exists(sdb => s.meta.documentsByUri(sdb).exists(_.active))
        if !indexableInModel && !indexedOnDisk then throw LsException(LsError.NoSemanticdb(uri))
      case _ => ()

  private def log(message: String): Unit = config.bootstrap.log(message)

  // ------------------------------------------------------------ text documents

  private final class TextDocs extends TextDocumentService:

    override def didOpen(params: DidOpenTextDocumentParams): Unit =
      val uri = Uris.normalize(params.getTextDocument.getUri)
      val text = params.getTextDocument.getText
      docs.open(uri, text)
      for
        s <- state.ready
        bspId <- s.uriToTarget.get(uri)
      do
        try s.pc.didOpen(bspId, uri, text)
        catch case NonFatal(t) => log(s"pc didOpen failed for $uri: $t")

    override def didChange(params: DidChangeTextDocumentParams): Unit =
      val uri = Uris.normalize(params.getTextDocument.getUri)
      val changes = params.getContentChanges
      if changes != null && !changes.isEmpty then
        // full-text sync: the last change carries the complete document
        val text = changes.get(changes.size() - 1).getText
        docs.change(uri, text)
        state.ready.foreach { s =>
          try
            if s.pc.bufferText(uri).isDefined then s.pc.didChange(uri, text)
            else s.uriToTarget.get(uri).foreach(bspId => s.pc.didOpen(bspId, uri, text))
          catch case NonFatal(t) => log(s"pc didChange failed for $uri: $t")
        }

    override def didClose(params: DidCloseTextDocumentParams): Unit =
      val uri = Uris.normalize(params.getTextDocument.getUri)
      docs.close(uri)
      state.ready.foreach { s =>
        try s.pc.didClose(uri)
        catch case NonFatal(t) => log(s"pc didClose failed for $uri: $t")
      }

    override def didSave(params: DidSaveTextDocumentParams): Unit =
      val uri = Uris.normalize(params.getTextDocument.getUri)
      // If the client sent the saved text, refresh the buffer so dirtiness
      // clears even when the editor batched the last change into the save.
      Option(params.getText).foreach(text => if docs.isOpen(uri) then docs.change(uri, text))
      state.ready.foreach { s =>
        val targets = s.uriToTarget.get(uri) match
          case Some(bspId) =>
            val closure = s.workspaceTargets.reverseDependencyClosure(bspId)
            if closure.nonEmpty then closure.toVector.sorted else Vector(bspId)
          case None => Vector.empty
        scheduleBuildJob(targets, compileFirst = targets.nonEmpty)
      }

    // --- PC requests ---

    override def completion(
        params: CompletionParams
    ): CompletableFuture[JEither[java.util.List[CompletionItem], CompletionList]] =
      onPc {
        requireSemanticdb(params.getTextDocument.getUri)
        val pos = params.getPosition
        withPcBuffer(params.getTextDocument.getUri)(
          JEither.forRight[java.util.List[CompletionItem], CompletionList](emptyCompletions())
        ) { (s, uri) =>
          s.uriToTarget.get(uri).foreach(bspId => lastCompletionTarget = Some(bspId))
          JEither.forRight(s.pc.completion(uri, pos.getLine, pos.getCharacter))
        }
      }

    override def resolveCompletionItem(item: CompletionItem): CompletableFuture[CompletionItem] =
      onPc {
        state match
          case WorkspaceState.Ready(s) =>
            val resolved =
              for
                symbol <- dataSymbol(item)
                target <- lastCompletionTarget
                if s.pcConfigs.contains(target)
              yield s.pc.completionItemResolve(target, item, symbol)
            resolved.getOrElse(item)
          case _ => item
      }

    override def hover(params: HoverParams): CompletableFuture[Hover] =
      onPc {
        requireSemanticdb(params.getTextDocument.getUri)
        val pos = params.getPosition
        withPcBuffer(params.getTextDocument.getUri)(null: Hover) { (s, uri) =>
          s.pc.hover(uri, pos.getLine, pos.getCharacter).orNull
        }
      }

    override def signatureHelp(params: SignatureHelpParams): CompletableFuture[SignatureHelp] =
      onPc {
        requireSemanticdb(params.getTextDocument.getUri)
        val pos = params.getPosition
        withPcBuffer(params.getTextDocument.getUri)(null: SignatureHelp) { (s, uri) =>
          s.pc.signatureHelp(uri, pos.getLine, pos.getCharacter)
        }
      }

    override def definition(
        params: DefinitionParams
    ): CompletableFuture[JEither[java.util.List[? <: Location], java.util.List[? <: LocationLink]]] =
      onPc {
        requireSemanticdb(params.getTextDocument.getUri)
        val pos = params.getPosition
        withPcBuffer(params.getTextDocument.getUri)(emptyLocations()) { (s, uri) =>
          JEither.forLeft(s.pc.definition(uri, pos.getLine, pos.getCharacter).lspLocations)
        }
      }

    override def typeDefinition(
        params: TypeDefinitionParams
    ): CompletableFuture[JEither[java.util.List[? <: Location], java.util.List[? <: LocationLink]]] =
      onPc {
        requireSemanticdb(params.getTextDocument.getUri)
        val pos = params.getPosition
        withPcBuffer(params.getTextDocument.getUri)(emptyLocations()) { (s, uri) =>
          JEither.forLeft(s.pc.typeDefinition(uri, pos.getLine, pos.getCharacter).lspLocations)
        }
      }

    // --- index requests ---

    override def references(
        params: ReferenceParams
    ): CompletableFuture[java.util.List[? <: Location]] =
      onIndex {
        requireSemanticdb(params.getTextDocument.getUri)
        val s = requireReady()
        val sdbUri = sdbUriOf(s, params.getTextDocument.getUri)
        val pos = params.getPosition
        val includeDeclaration =
          Option(params.getContext).exists(_.isIncludeDeclaration)
        val result =
          s.references.references(sdbUri, pos.getLine, pos.getCharacter, includeDeclaration)
        if result.needsReindex then scheduleBuildJob(Vector.empty, compileFirst = false)
        val out = new java.util.ArrayList[Location](result.hits.length)
        for hit <- result.hits do
          s.uris.toFileUri(hit.loc.uri).foreach { fileUri =>
            out.add(LspConvert.location(fileUri, hit.loc.span))
          }
        out
      }

    override def documentHighlight(
        params: DocumentHighlightParams
    ): CompletableFuture[java.util.List[? <: DocumentHighlight]] =
      onIndex {
        requireSemanticdb(params.getTextDocument.getUri)
        state match
          case WorkspaceState.Ready(s) =>
            try
              val sdbUri = sdbUriOf(s, params.getTextDocument.getUri)
              val pos = params.getPosition
              val highlights = s.highlights.highlights(sdbUri, pos.getLine, pos.getCharacter)
              val out = new java.util.ArrayList[DocumentHighlight](highlights.length)
              highlights.foreach(h =>
                out.add(
                  new DocumentHighlight(LspConvert.range(h.span), LspConvert.highlightKind(h.kind))
                )
              )
              out
            catch
              // cursor-follow request: an unanswerable position is an empty
              // result, not an editor-visible error
              case _: LsException => java.util.List.of[DocumentHighlight]()
          case _ => java.util.List.of[DocumentHighlight]()
      }

    override def prepareRename(
        params: PrepareRenameParams
    ): CompletableFuture[Either3[Range, PrepareRenameResult, PrepareRenameDefaultBehavior]] =
      onIndex {
        requireSemanticdb(params.getTextDocument.getUri)
        state match
          case WorkspaceState.Ready(s) =>
            try
              val sdbUri = sdbUriOf(s, params.getTextDocument.getUri)
              val pos = params.getPosition
              val span = s.rename.prepareRename(sdbUri, pos.getLine, pos.getCharacter)
              Either3.forFirst(LspConvert.range(span))
            catch case _: LsException => null
          case _ => null
      }

    override def rename(params: RenameParams): CompletableFuture[WorkspaceEdit] =
      onIndex {
        requireSemanticdb(params.getTextDocument.getUri)
        val s = requireReady()
        val sdbUri = sdbUriOf(s, params.getTextDocument.getUri)
        val pos = params.getPosition
        val plan = s.rename.rename(sdbUri, pos.getLine, pos.getCharacter, params.getNewName)
        LspConvert.workspaceEdit(plan, s.uris.toFileUri)
      }

    private def emptyCompletions(): CompletionList =
      new CompletionList(false, java.util.List.of())

    private def emptyLocations()
        : JEither[java.util.List[? <: Location], java.util.List[? <: LocationLink]] =
      JEither.forLeft(java.util.List.of[Location]())

    /** mtags puts the SemanticDB symbol into `CompletionItem.data.symbol`. */
    private def dataSymbol(item: CompletionItem): Option[String] =
      item.getData match
        case je: JsonElement if je.isJsonObject =>
          Option(je.getAsJsonObject.get("symbol"))
            .filter(e => e.isJsonPrimitive && e.getAsJsonPrimitive.isString)
            .map(_.getAsString)
        case _ => None

  // --------------------------------------------------------------- workspace

  private final class Workspace extends WorkspaceService:

    override def didChangeConfiguration(params: DidChangeConfigurationParams): Unit = ()
    override def didChangeWatchedFiles(params: DidChangeWatchedFilesParams): Unit = ()

    override def symbol(
        params: WorkspaceSymbolParams
    ): CompletableFuture[
      JEither[java.util.List[? <: SymbolInformation], java.util.List[? <: WorkspaceSymbol]]
    ] =
      onIndex {
        state match
          case WorkspaceState.Ready(s) =>
            val query = Option(params.getQuery).getOrElse("")
            val hits = s.orchestrator.workspaceSymbol(query)
            val out = new java.util.ArrayList[WorkspaceSymbol](hits.length)
            hits.foreach(h => workspaceSymbolOf(s, h).foreach(out.add))
            // Merge top-level symbols from open unsaved buffers that the
            // persisted index has never seen, flagged PC-only.
            overlay.pcOnlySymbols(query).foreach(p => out.add(pcOnlyWorkspaceSymbol(p)))
            JEither.forRight(out)
          case _ =>
            // BestEffort consistency (plan 10): an unbootstrapped workspace
            // simply knows no symbols yet.
            JEither.forRight(java.util.List.of[WorkspaceSymbol]())
      }

    override def executeCommand(params: ExecuteCommandParams): CompletableFuture[Object] =
      onIndex {
        params.getCommand match
          case Commands.Doctor =>
            DoctorCommand.report(workspaceRoot, state)
          case Commands.Reindex =>
            state match
              case WorkspaceState.Ready(s) if s.workspaceTargets.targets.nonEmpty =>
                Bootstrap.ingestSummary(s.orchestrator.ingest(s.workspaceTargets))
              case WorkspaceState.Ready(_) =>
                "reindex skipped: no target produces SemanticDB"
              case other => s"reindex unavailable: workspace is ${other.statusLine}"
          case Commands.Compile =>
            state match
              case WorkspaceState.Ready(s) if s.indexableBspIds.nonEmpty =>
                s.compiler.compile(s.indexableBspIds) match
                  case BspCompileOutcome.Ok(_) =>
                    s"compile ok (${s.indexableBspIds.length} targets)"
                  case BspCompileOutcome.Failed(code, _) => s"compile failed: $code"
              case WorkspaceState.Ready(_) => "compile skipped: no indexable targets"
              case other => s"compile unavailable: workspace is ${other.statusLine}"
          case Commands.PcPluginStatus =>
            state match
              case WorkspaceState.Ready(s) => PcStatusRender.render(s.pc.pluginStatus)
              case other => s"pc plugin status unavailable: workspace is ${other.statusLine}"
          case other =>
            throw responseError(ResponseErrorCode.InvalidParams, s"unknown command '$other'")
      }

    private def workspaceSymbolOf(
        s: CoreServices,
        hit: WorkspaceSymbolHit
    ): Option[WorkspaceSymbol] =
      for
        (docUri, targetId) <- docRowById(s.meta, hit.docId)
        targetRow <- s.orchestrator.targetRowById(targetId)
      yield
        val absolute = Path.of(targetRow.sourceroot).resolve(docUri)
        val span = s.meta
          .symbolMetadataFor(hit.docId)
          .find(m => m.symbolId == hit.symbolId && m.targetId == hit.targetId)
          .flatMap(_.span)
          .getOrElse(Span(0, 0, 0, 0))
        val location = LspConvert.location(Uris.toUri(absolute), span)
        val container = hit.ownerName.orElse(hit.packageName).orNull
        new WorkspaceSymbol(
          hit.displayName,
          LspConvert.symbolKind(hit.kind),
          JEither.forLeft(location),
          container
        )

    /** Builds a `WorkspaceSymbol` for a PC-only unsaved-buffer symbol: its
      * location points into the dirty buffer and its container marks it PC-only
      * so a client can tell it apart from a persisted-index result.
      */
    private def pcOnlyWorkspaceSymbol(p: PcOnlySymbol): WorkspaceSymbol =
      val kind = p.keyword match
        case "object" => SymbolKind.Object
        case "class" => SymbolKind.Class
        case "trait" => SymbolKind.Interface
        case "enum" => SymbolKind.Enum
        case "def" => SymbolKind.Method
        case "val" | "var" => SymbolKind.Variable
        case "type" => SymbolKind.TypeParameter
        case _ => SymbolKind.Object
      new WorkspaceSymbol(
        p.name,
        kind,
        JEither.forLeft(LspConvert.location(p.fileUri, p.span)),
        ScalaLs.PcOnlyContainer
      )

    /** documents readback by doc id (MetaStore has no such accessor; the row
      * is needed to turn an FTS hit into a Location).
      */
    private def docRowById(meta: MetaStore, docId: DocId): Option[(String, TargetId)] =
      meta.db
        .prepare("SELECT uri, target_id FROM documents WHERE doc_id = ?")
        .bindLong(1, docId.value)
        .queryOne(st => (st.columnText(0), TargetId(st.columnLong(1))))

object ScalaLs:
  val ServerName = "scala3-bsp-semantic-ls"
  val ServerVersion = "0.1.0"

  /** `containerName` marker on a `workspace/symbol` result that exists only in
    * an open unsaved buffer (PC-only, excluded from global references/rename).
    */
  val PcOnlyContainer = "unsaved buffer (PC-only)"

  object Commands:
    val Doctor = "scala3SemanticLs.doctor"
    val Reindex = "scala3SemanticLs.reindex"
    val Compile = "scala3SemanticLs.compile"
    val PcPluginStatus = "scala3SemanticLs.pcPluginStatus"
    val all: List[String] = List(Doctor, Reindex, Compile, PcPluginStatus)

  final case class Config(
      bootstrap: Bootstrap.Config = Bootstrap.Config(),
      debounceMillis: Long = 500L,
      exitProcessOnExit: Boolean = true
  )
