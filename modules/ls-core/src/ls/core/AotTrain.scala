package ls.core

import java.nio.file.{Files, Path}
import java.util.concurrent.{CompletableFuture, TimeUnit}

import scala.concurrent.duration.DurationInt
import scala.jdk.CollectionConverters.*
import scala.util.control.NonFatal

import ls.bsp.{BspDiscovery, BspSession, BspSessionConfig}
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
      skipPc: Boolean = false,
      navProbe: Boolean = false,
      navExpectPlugin: Boolean = true,
      log: String => Unit = msg => System.err.println(s"[aot-train] $msg")
  ): Int =
    val root = workspaceRoot.toAbsolutePath.normalize
    log(s"training workload over $root (requireIndex=$requireIndex, skipPc=$skipPc, navProbe=$navProbe, navExpectPlugin=$navExpectPlugin)")

    // A generous BSP request timeout so a large real project (whose first
    // build/compile can take minutes) does not time out mid-compile.
    val server = new ScalaLs(
      ScalaLs.Config(
        bootstrap = Bootstrap.Config(
          connectBsp = (r, handlers) =>
            BspDiscovery
              .pick(r)
              .map(f => BspSession.launch(r, f.details, handlers, BspSessionConfig(requestTimeout = 600.seconds)))
        ),
        exitProcessOnExit = false
      )
    )
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
          else strict(server, root, log, skipPc, navProbe, navExpectPlugin)
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

  private def strict(
      server: ScalaLs,
      root: Path,
      log: String => Unit,
      skipPc: Boolean,
      navProbe: Boolean,
      navExpectPlugin: Boolean
  ): Int =
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

    // PC completion at a real member-select. Skipped for cross-version real
    // repos (e.g. a Scala 3.7.x project under this 3.8.x presentation compiler):
    // the SemanticDB-backed index features above are version-independent, but PC
    // completion needs a matching compiler.
    if skipPc then log("PC completion check skipped (--skip-pc)")
    else findSelectProbe(sources) match
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

    // zaozi-specific: go-to + hover on a Dynamic bundle-field access through the
    // presentation compiler (the plugin loaded from pc-plugins.json steers it).
    if navProbe then zaoziNavProbe(server, docs, root, navExpectPlugin, log, require)

    if errors.isEmpty then 0
    else
      errors.foreach(e => log(s"FAIL: $e"))
      1

  // -- zaozi Dynamic-nav probe: PC go-to + hover on `io.a` & friends -----------

  /** Drive `textDocument/definition` + `textDocument/hover` on the zaozi Dynamic
    * bundle-field accesses in `BundleSpec.scala`, which route through the
    * presentation compiler. With the `zaozi-pcplugin` loaded (from the workspace
    * `pc-plugins.json`) go-to resolves `io.a`/`io.f.g`/`io.k` to the real field
    * declarations and hover describes the field; without it, go-to lands on the
    * framework `selectDynamic`. `expectPlugin` pins which side we assert, and is
    * cross-checked against the live PC plugin status so a mis-pathed jar cannot
    * pass silently. All positions are text-scanned (never hard-coded lines).
    */
  private def zaoziNavProbe(
      server: ScalaLs,
      docs: TextDocumentService,
      root: Path,
      expectPlugin: Boolean,
      log: String => Unit,
      require: (Boolean, => String) => Unit
  ): Unit =
    sortedScalaSources(root).find(_.getFileName.toString == "BundleSpec.scala") match
      case None => require(false, "zaozi-nav: BundleSpec.scala not found under the workspace")
      case Some(path) =>
        val uri = Uris.toUri(path)
        val text = try Files.readString(path) catch case NonFatal(_) => ""
        if text.isEmpty then require(false, s"zaozi-nav: could not read $path")
        else
          openDoc(docs, uri, text)
          val lines = text.split("\n", -1)

          // We configure exactly one compiler plugin (the zaozi jar), so "any
          // compiler plugin loaded" means the plugin is active. (The jar dir is
          // `zaoziPcplugin`, not `zaozi-pcplugin` — do not substring-match a name.)
          val pluginLoaded = server.currentState.ready.exists(_.pc.pluginStatus.compilerPlugins.exists(_.loaded))
          log(s"zaozi-nav: expectPlugin=$expectPlugin pluginLoaded=$pluginLoaded")
          // A mis-pathed jar would leave the plugin unloaded and make the baseline
          // assertions pass on the "plugin" run — pin the state to the expectation.
          require(pluginLoaded == expectPlugin, s"zaozi-nav: pc reports pluginLoaded=$pluginLoaded, expected $expectPlugin")

          def sameFile(u: String): Boolean = Uris.normalize(u) == Uris.normalize(uri)

          // Cursor `charOffset` chars into `access` on its first occurrence; the
          // returned locations plus whether any covers `declLine0` in this file.
          def definitionAt(access: String, charOffset: Int, declLine0: Int): (Boolean, String) =
            val useLine = lines.indexWhere(_.contains(access))
            if useLine < 0 then (false, s"<no `$access` use>")
            else
              val col = lines(useLine).indexOf(access) + charOffset
              val params = new DefinitionParams(new TextDocumentIdentifier(uri), new Position(useLine, col))
              val res =
                try docs.definition(params).get(RequestTimeoutMillis, TimeUnit.MILLISECONDS)
                catch
                  case NonFatal(t) =>
                    log(s"zaozi-nav: definition($access) threw: $t")
                    null
              val locs: Vector[Location] =
                if res == null then Vector.empty
                else if res.isLeft then res.getLeft.asScala.toVector
                else Vector.empty
              val hit = locs.exists(l =>
                sameFile(l.getUri) &&
                  l.getRange.getStart.getLine <= declLine0 && declLine0 <= l.getRange.getEnd.getLine
              )
              (hit, locs.map(l => s"${l.getUri.split('/').lastOption.getOrElse(l.getUri)}:${l.getRange.getStart.getLine}").mkString(","))

          // Field-declaration line (0-based) of `val <field>` at/after `afterClass`.
          def declLineOf(afterClassRegex: String, field: String): Int =
            val from = lines.indexWhere(_.matches(s".*$afterClassRegex.*"))
            var i = if from < 0 then 0 else from + 1
            val pat = s"""\\s*val $field\\b.*"""
            while i < lines.length && !lines(i).matches(pat) do i += 1
            i

          // io.a -> BundleSpecIO.a (direct); io.f.g -> SimpleBundle.g (nested);
          // io.k -> BundleSpecIO.k (optional, getOptionRefViaFieldValName path).
          val declA = declLineOf("class BundleSpecIO", "a")
          val declG = declLineOf("class SimpleBundle\\b", "g")
          val declK = declLineOf("class BundleSpecIO", "k")
          val (hitA, locA) = definitionAt("io.a", 3, declA)
          val (hitG, locG) = definitionAt("io.f.g", 5, declG)
          val (hitK, locK) = definitionAt("io.k", 3, declK)
          log(s"zaozi-nav: io.a->val a@${declA + 1} hit=$hitA locs=[$locA]")
          log(s"zaozi-nav: io.f.g->SimpleBundle.g@${declG + 1} hit=$hitG locs=[$locG]")
          log(s"zaozi-nav: io.k->val k@${declK + 1} hit=$hitK locs=[$locK]")

          // hover on io.a
          val hovText =
            val useLine = lines.indexWhere(_.contains("io.a"))
            if useLine < 0 then ""
            else
              val col = lines(useLine).indexOf("io.a") + 3
              val hp = new HoverParams(new TextDocumentIdentifier(uri), new Position(useLine, col))
              val h =
                try docs.hover(hp).get(RequestTimeoutMillis, TimeUnit.MILLISECONDS)
                catch
                  case NonFatal(t) =>
                    log(s"zaozi-nav: hover(io.a) threw: $t")
                    null
              hoverText(h)
          log(s"zaozi-nav: hover(io.a)=${hovText.replace('\n', ' ').take(160)}")

          if expectPlugin then
            require(hitA, s"zaozi-nav: with the plugin, go-to on io.a should reach `val a` (line ${declA + 1}); got [$locA]")
            require(hitG, s"zaozi-nav: with the plugin, nested go-to on io.f.g should reach `val g` (line ${declG + 1}); got [$locG]")
            require(hitK, s"zaozi-nav: with the plugin, optional go-to on io.k should reach `val k` (line ${declK + 1}); got [$locK]")
            require(
              hovText.nonEmpty && !hovText.contains("selectDynamic"),
              s"zaozi-nav: with the plugin, hover on io.a should describe the field, not selectDynamic; got '${hovText.take(120)}'"
            )
          else
            require(!hitA, s"zaozi-nav baseline: without the plugin, go-to on io.a must NOT reach `val a`; got [$locA]")

  private def hoverText(h: Hover): String =
    if h == null || h.getContents == null then ""
    else
      val c = h.getContents
      if c.isRight then Option(c.getRight).map(_.getValue).getOrElse("")
      else
        c.getLeft.asScala
          .map(e => if e.isLeft then e.getLeft else Option(e.getRight).map(_.getValue).getOrElse(""))
          .mkString("\n")

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
