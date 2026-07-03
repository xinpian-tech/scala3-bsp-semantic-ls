package ls.core

import java.nio.file.{Files, Path}
import java.util.concurrent.{CompletableFuture, TimeUnit}

import scala.jdk.CollectionConverters.*
import scala.util.control.NonFatal

import org.eclipse.lsp4j.*
import org.eclipse.lsp4j.services.{LanguageClient, TextDocumentService, WorkspaceService}

/** Headless AOT-training workload.
  *
  * Boots the real [[ScalaLs]] against a workspace in-process (no stdio, no
  * pipes) and drives the runtime hot paths once so a JVM launched with
  * `-XX:AOTMode=record` observes the classes the server loads and links.
  *
  * Two modes:
  *
  *   - `requireIndex = true` (a real BSP workspace, `.bsp` present): the STRICT
  *     path. It requires a BSP-backed index, drives a real build compile then a
  *     reindex through the production command path, and asserts SemanticDB-backed
  *     workspace/symbol + references + PC completion all return results. Any empty
  *     result or missing index makes the run FAIL (non-zero) — the training must
  *     not silently pass on a degraded path.
  *   - `requireIndex = false` (no BSP): the LENIENT path. It best-effort boots and
  *     queries whatever the recovered index knows and always exits cleanly, so a
  *     workspace with no `.bsp` trains without hanging.
  */
object AotTrain:

  private val InitTimeoutMillis = 60000L
  private val BootstrapTimeoutMillis = 600000L
  private val CommandTimeoutMillis = 600000L
  private val RequestTimeoutMillis = 180000L

  def run(
      workspaceRoot: Path,
      requireIndex: Boolean,
      log: String => Unit = msg => System.err.println(s"[aot-train] $msg")
  ): Int =
    val root = workspaceRoot.toAbsolutePath.normalize
    log(s"training workload over $root (requireIndex=$requireIndex)")

    val server = new ScalaLs(ScalaLs.Config(exitProcessOnExit = false))
    server.connect(new SilentLanguageClient)
    try
      val initParams = new InitializeParams()
      initParams.setRootUri(Uris.toUri(root))
      server.initialize(initParams).get(InitTimeoutMillis, TimeUnit.MILLISECONDS)
      server.initialized(new InitializedParams())
      if !server.awaitBootstrap(BootstrapTimeoutMillis) then log("bootstrap did not finish in time")

      val bspBacked =
        server.currentState.ready.exists(s => s.session.isDefined && s.indexableBspIds.nonEmpty)
      val code =
        if requireIndex then
          if !bspBacked then
            log("FAIL: --require-index set but no BSP-backed index is available")
            1
          else strict(server, root, log)
        else
          lenient(server, root, log)
          0

      try server.shutdown().get(RequestTimeoutMillis, TimeUnit.MILLISECONDS)
      catch case NonFatal(t) => log(s"shutdown skipped: $t")
      server.exit()
      log(if code == 0 then "training workload complete" else "training workload FAILED")
      code
    catch
      case NonFatal(t) =>
        log(s"training workload aborted: $t")
        try server.exit()
        catch case NonFatal(_) => ()
        // Lenient mode never fails; strict mode surfaces the abort.
        if requireIndex then 1 else 0

  // -- strict real-BSP workload: compile + reindex, then assert non-empty ------

  private def strict(server: ScalaLs, root: Path, log: String => Unit): Int =
    val errors = scala.collection.mutable.ArrayBuffer.empty[String]
    def require(cond: Boolean, msg: => String): Unit = if !cond then errors += msg

    val docs = server.getTextDocumentService
    val ws = server.getWorkspaceService

    // real build compile over the BSP session, then reindex the produced SemanticDB
    val compileRes = command(ws, ScalaLs.Commands.Compile)
    require(compileRes.startsWith("compile ok"), s"compile did not succeed: $compileRes")
    val reindexRes = command(ws, ScalaLs.Commands.Reindex)
    val indexedDocs = docsCount(reindexRes)
    require(indexedDocs > 0, s"reindex indexed no docs: $reindexRes")
    log(s"compile=[$compileRes] reindex=[$reindexRes]")

    val sources = sortedScalaSources(root)

    // workspace/symbol over the freshly-filled index
    findTypeProbe(sources) match
      case None => require(false, "no top-level type declaration found to probe")
      case Some(probe) =>
        val hits = symbolNames(ws.symbol(new WorkspaceSymbolParams(probe.name)).get(RequestTimeoutMillis, TimeUnit.MILLISECONDS))
        require(hits.contains(probe.name), s"workspace/symbol('${probe.name}') returned ${hits.take(10)}")
        // references on the declared symbol (includeDeclaration -> at least its own def)
        openDoc(docs, probe.uri, probe.text)
        val refs = docs
          .references(new ReferenceParams(new TextDocumentIdentifier(probe.uri), probe.pos, new ReferenceContext(true)))
          .get(RequestTimeoutMillis, TimeUnit.MILLISECONDS)
        require(refs != null && !refs.isEmpty, s"references on '${probe.name}' returned no locations")
        log(s"workspace/symbol('${probe.name}')=${hits.size} references=${Option(refs).map(_.size).getOrElse(0)}")

    // PC completion at a real member-select
    findSelectProbe(sources) match
      case None => require(false, "no member-select found to probe completion")
      case Some(probe) =>
        openDoc(docs, probe.uri, probe.text)
        val result = docs
          .completion(new CompletionParams(new TextDocumentIdentifier(probe.uri), probe.pos))
          .get(RequestTimeoutMillis, TimeUnit.MILLISECONDS)
        val items =
          if result == null then Vector.empty
          else if result.isRight then result.getRight.getItems.asScala.toVector
          else result.getLeft.asScala.toVector
        require(items.nonEmpty, "PC completion returned no items")
        log(s"completion items=${items.size}")

    if errors.isEmpty then 0
    else
      errors.foreach(e => log(s"FAIL: $e"))
      1

  // -- lenient no-BSP workload: best-effort, always clean ----------------------

  private def lenient(server: ScalaLs, root: Path, log: String => Unit): Unit =
    def step(name: String)(body: => Unit): Unit =
      try body
      catch case NonFatal(t) => log(s"step '$name' skipped: $t")

    val docs = server.getTextDocumentService
    val ws = server.getWorkspaceService
    step("workspace/symbol") {
      ws.symbol(new WorkspaceSymbolParams("a")).get(RequestTimeoutMillis, TimeUnit.MILLISECONDS)
    }
    sortedScalaSources(root).headOption.foreach { source =>
      val uri = Uris.toUri(source)
      val text = try Files.readString(source) catch case NonFatal(_) => ""
      step("didOpen")(openDoc(docs, uri, text))
      val pos = findTypeProbe(Vector(source)).map(_.pos).getOrElse(new Position(0, 0))
      step("references") {
        docs
          .references(new ReferenceParams(new TextDocumentIdentifier(uri), pos, new ReferenceContext(true)))
          .get(RequestTimeoutMillis, TimeUnit.MILLISECONDS)
      }
      step("completion") {
        docs
          .completion(new CompletionParams(new TextDocumentIdentifier(uri), pos))
          .get(RequestTimeoutMillis, TimeUnit.MILLISECONDS)
      }
    }

  // -- helpers -----------------------------------------------------------------

  private def command(ws: WorkspaceService, cmd: String): String =
    try
      ws.executeCommand(new ExecuteCommandParams(cmd, java.util.List.of()))
        .get(CommandTimeoutMillis, TimeUnit.MILLISECONDS) match
        case s: String => s
        case other => String.valueOf(other)
    catch case NonFatal(t) => s"command '$cmd' failed: $t"

  private def openDoc(docs: TextDocumentService, uri: String, text: String): Unit =
    docs.didOpen(new DidOpenTextDocumentParams(new TextDocumentItem(uri, "scala", 1, text)))

  private def symbolNames(
      result: org.eclipse.lsp4j.jsonrpc.messages.Either[
        java.util.List[? <: SymbolInformation],
        java.util.List[? <: WorkspaceSymbol]
      ]
  ): Vector[String] =
    if result == null then Vector.empty
    else if result.isRight then result.getRight.asScala.toVector.map(_.getName)
    else result.getLeft.asScala.toVector.map(_.getName)

  /** `N docs` count from a reindex summary, else 0. */
  private def docsCount(reindex: String): Int =
    "(\\d+) docs".r.findFirstMatchIn(reindex).map(_.group(1).toInt).getOrElse(0)

  /** Workspace `.scala` sources in a stable (relative-path-sorted) order,
    * skipping generated/build directories.
    */
  private def sortedScalaSources(root: Path): Vector[Path] =
    val skip = Set("out", ".bsp", ".scala3-bsp-semantic-ls", ".git", "target")
    val stream =
      try Files.walk(root)
      catch case NonFatal(_) => return Vector.empty
    try
      stream
        .filter(p => Files.isRegularFile(p))
        .filter(p => p.getFileName.toString.endsWith(".scala"))
        .filter(p => !root.relativize(p).iterator.asScala.exists(seg => skip.contains(seg.toString)))
        .iterator
        .asScala
        .toVector
        .sortBy(p => root.relativize(p).toString)
    finally stream.close()

  private final case class TypeProbe(uri: String, text: String, name: String, pos: Position)
  private final case class SelectProbe(uri: String, text: String, pos: Position)

  private val TypeDecl = "\\b(?:class|object|trait|enum)\\s+([A-Za-z_][A-Za-z0-9_]*)".r
  private val MemberSelect = "\\b([A-Za-z_][A-Za-z0-9_]*)\\.([A-Za-z_][A-Za-z0-9_]*)".r

  /** First top-level type declaration across the sorted sources: its name plus a
    * cursor one char into the name token (so references resolve the symbol).
    */
  private def findTypeProbe(sources: Vector[Path]): Option[TypeProbe] =
    sources.iterator.flatMap { p =>
      val text = try Files.readString(p) catch case NonFatal(_) => ""
      text.linesIterator.zipWithIndex.flatMap { (line, ln) =>
        TypeDecl.findFirstMatchIn(line).map(m => TypeProbe(Uris.toUri(p), text, m.group(1), new Position(ln, m.start(1) + 1)))
      }.nextOption()
    }.nextOption()

  /** First member-select `x.y` that is not part of a package/import line, with a
    * cursor right after the dot (where a completion request lists members).
    */
  private def findSelectProbe(sources: Vector[Path]): Option[SelectProbe] =
    sources.iterator.flatMap { p =>
      val text = try Files.readString(p) catch case NonFatal(_) => ""
      text.linesIterator.zipWithIndex.flatMap { (line, ln) =>
        val trimmed = line.trim
        if trimmed.startsWith("package") || trimmed.startsWith("import") then None
        else MemberSelect.findFirstMatchIn(line).map(m => SelectProbe(Uris.toUri(p), text, new Position(ln, m.start(2))))
      }.nextOption()
    }.nextOption()

/** No-op LSP client for the headless training run: swallows every server
  * notification/request. ls-core compiles with `-Xmixin-force-forwarders:false`,
  * so only the abstract [[LanguageClient]] methods need bodies.
  */
private final class SilentLanguageClient extends LanguageClient:
  private def done[A]: CompletableFuture[A] = CompletableFuture.completedFuture(null.asInstanceOf[A])
  override def telemetryEvent(obj: Object): Unit = ()
  override def publishDiagnostics(params: PublishDiagnosticsParams): Unit = ()
  override def showMessage(params: MessageParams): Unit = ()
  override def showMessageRequest(params: ShowMessageRequestParams): CompletableFuture[MessageActionItem] = done
  override def logMessage(params: MessageParams): Unit = ()
