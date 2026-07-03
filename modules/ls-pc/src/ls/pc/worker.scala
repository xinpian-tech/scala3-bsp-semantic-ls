package ls.pc

import java.nio.file.Paths
import java.util.concurrent.{CompletableFuture, Executors}

import scala.jdk.CollectionConverters.*

import org.eclipse.lsp4j.jsonrpc.services.JsonRequest
import org.eclipse.lsp4j.{CompletionItem, CompletionList, Hover, Range, SignatureHelp}

/** JSON-friendly parameter/result carriers for the PC worker protocol
  * (plan 5.2). Plain mutable fields so lsp4j's gson-based json handler can
  * (de)serialize them without Scala-specific adapters; nested lsp4j model
  * types use lsp4j's own adapters.
  */
final class PcWorkerTargetParams:
  var bspId: String = ""
  var scalaVersion: String = "3.8.4"
  var classpath: java.util.List[String] = new java.util.ArrayList()
  var scalacOptions: java.util.List[String] = new java.util.ArrayList()
  var sourceDirs: java.util.List[String] = new java.util.ArrayList()

object PcWorkerTargetParams:
  def of(config: PcTargetConfig): PcWorkerTargetParams =
    val p = new PcWorkerTargetParams
    p.bspId = config.bspId
    p.scalaVersion = config.scalaVersion
    p.classpath = config.classpath.map(_.toString).asJava
    p.scalacOptions = config.scalacOptions.asJava
    p.sourceDirs = config.sourceDirs.map(_.toString).asJava
    p

  def toConfig(p: PcWorkerTargetParams): PcTargetConfig =
    PcTargetConfig(
      bspId = p.bspId,
      classpath = p.classpath.asScala.toVector.map(Paths.get(_)),
      scalacOptions = p.scalacOptions.asScala.toVector,
      sourceDirs = p.sourceDirs.asScala.toVector.map(Paths.get(_)),
      scalaVersion = p.scalaVersion
    )

final class PcWorkerDidOpenParams:
  var targetId: String = ""
  var uri: String = ""
  var text: String = ""

final class PcWorkerChangeParams:
  var uri: String = ""
  var text: String = ""

final class PcWorkerUriParams:
  var uri: String = ""

final class PcWorkerPositionParams:
  var uri: String = ""
  var line: Int = 0
  var character: Int = 0

/** Resolve a completion item: `item` is the item to enrich, `symbol` its PC
  * symbol (from the item's data), `targetId` the owning build target.
  */
final class PcWorkerResolveParams:
  var targetId: String = ""
  var symbol: String = ""
  var item: CompletionItem = new CompletionItem()

/** JSON-friendly definition result: `origins.get(i)` is the
  * [[DefinitionOrigin]] name for `locations.get(i)`.
  */
final class PcWorkerDefinitionResult:
  var symbol: String = ""
  var locations: java.util.List[org.eclipse.lsp4j.Location] = new java.util.ArrayList()
  var origins: java.util.List[String] = new java.util.ArrayList()

object PcWorkerDefinitionResult:
  def of(result: DefinitionResult): PcWorkerDefinitionResult =
    val r = new PcWorkerDefinitionResult
    r.symbol = result.symbol
    r.locations = result.locations.map(_.location).asJava
    r.origins = result.locations.map(_.origin.toString).asJava
    r

  def toDefinitionResult(r: PcWorkerDefinitionResult): DefinitionResult =
    val locs = r.locations.asScala.toVector
    val origins = r.origins.asScala.toVector
    val marked = locs.zipWithIndex.map { case (loc, i) =>
      val origin = origins.lift(i).map(DefinitionOrigin.valueOf).getOrElse(DefinitionOrigin.Workspace)
      DefinitionLocation(loc, origin)
    }
    DefinitionResult(r.symbol, marked)

/** JSON-friendly mirror of [[CompilerPluginStatus]]. */
final class PcWorkerCompilerPlugin:
  var jars: java.util.List[String] = new java.util.ArrayList()
  var options: java.util.List[String] = new java.util.ArrayList()
  var loaded: Boolean = false
  var detail: String = ""

/** JSON-friendly mirror of [[ServicePluginStatus]]. */
final class PcWorkerServicePlugin:
  var id: String = ""
  var source: String = ""
  var enabled: Boolean = true
  var selfTestOk: Boolean = true
  var selfTestDetail: String = ""

/** JSON-friendly mirror of [[DisabledPlugin]]. */
final class PcWorkerDisabledPlugin:
  var id: String = ""
  var reason: String = ""

/** JSON-friendly, LOSSLESS mirror of [[PcPluginStatusReport]] for the forked
  * worker: nested plain-field carriers so the doctor and PC-only rejection see
  * the same structured status a forked child reports as an in-process facade
  * would. [[toReport]] is the exact inverse of [[of]].
  */
final class PcWorkerPluginStatus:
  var compilerPlugins: java.util.List[PcWorkerCompilerPlugin] = new java.util.ArrayList()
  var servicePlugins: java.util.List[PcWorkerServicePlugin] = new java.util.ArrayList()
  var disabled: java.util.List[PcWorkerDisabledPlugin] = new java.util.ArrayList()

object PcWorkerPluginStatus:
  def of(report: PcPluginStatusReport): PcWorkerPluginStatus =
    val s = new PcWorkerPluginStatus
    s.compilerPlugins = report.compilerPlugins.map { c =>
      val w = new PcWorkerCompilerPlugin
      w.jars = c.jars.asJava
      w.options = c.options.asJava
      w.loaded = c.loaded
      w.detail = c.detail
      w
    }.asJava
    s.servicePlugins = report.servicePlugins.map { p =>
      val w = new PcWorkerServicePlugin
      w.id = p.id
      w.source = p.source
      w.enabled = p.enabled
      w.selfTestOk = p.selfTestOk
      w.selfTestDetail = p.selfTestDetail
      w
    }.asJava
    s.disabled = report.disabled.map { d =>
      val w = new PcWorkerDisabledPlugin
      w.id = d.id
      w.reason = d.reason
      w
    }.asJava
    s

  def toReport(s: PcWorkerPluginStatus): PcPluginStatusReport =
    PcPluginStatusReport(
      compilerPlugins = s.compilerPlugins.asScala.toVector.map(c =>
        CompilerPluginStatus(c.jars.asScala.toVector, c.options.asScala.toVector, c.loaded, c.detail)
      ),
      servicePlugins = s.servicePlugins.asScala.toVector.map(p =>
        ServicePluginStatus(p.id, p.source, p.enabled, p.selfTestOk, p.selfTestDetail)
      ),
      disabled = s.disabled.asScala.toVector.map(d => DisabledPlugin(d.id, d.reason))
    )

/** The PC worker JVM boundary (plan 5.2): a small JSON-RPC surface over the
  * facade operations — target init, dirty-buffer lifecycle, the six queries,
  * plugin status, shutdown. Served over stdin/stdout by [[PcWorkerMain]],
  * implemented in-process by [[InProcessPcWorker]], and proxied across a
  * child JVM by [[ForkedPcWorker]].
  */
trait PcWorkerApi:
  @JsonRequest("pc/initializeTarget")
  def initializeTarget(params: PcWorkerTargetParams): CompletableFuture[String]

  @JsonRequest("pc/didOpen")
  def didOpen(params: PcWorkerDidOpenParams): CompletableFuture[String]

  @JsonRequest("pc/didChange")
  def didChange(params: PcWorkerChangeParams): CompletableFuture[String]

  @JsonRequest("pc/didClose")
  def didClose(params: PcWorkerUriParams): CompletableFuture[String]

  @JsonRequest("pc/completion")
  def completion(params: PcWorkerPositionParams): CompletableFuture[CompletionList]

  @JsonRequest("pc/completionResolve")
  def completionItemResolve(params: PcWorkerResolveParams): CompletableFuture[CompletionItem]

  /** Null when the PC has no hover at the position. */
  @JsonRequest("pc/hover")
  def hover(params: PcWorkerPositionParams): CompletableFuture[Hover]

  @JsonRequest("pc/signatureHelp")
  def signatureHelp(params: PcWorkerPositionParams): CompletableFuture[SignatureHelp]

  @JsonRequest("pc/definition")
  def definition(params: PcWorkerPositionParams): CompletableFuture[PcWorkerDefinitionResult]

  @JsonRequest("pc/typeDefinition")
  def typeDefinition(params: PcWorkerPositionParams): CompletableFuture[PcWorkerDefinitionResult]

  /** Null when the symbol is not renameable via PC. */
  @JsonRequest("pc/prepareRename")
  def prepareRename(params: PcWorkerPositionParams): CompletableFuture[Range]

  @JsonRequest("pc/pluginStatus")
  def pluginStatus(): CompletableFuture[PcWorkerPluginStatus]

  @JsonRequest("pc/shutdown")
  def shutdown(): CompletableFuture[String]

/** Client-side interface of the worker protocol. The worker never calls back
  * into its parent, so this is intentionally empty; it only satisfies the
  * jsonrpc launcher's remote-interface requirement on the server side.
  */
trait PcWorkerClient

/** Default in-process worker: [[PcWorkerApi]] over a [[PcFacade]]. Requests
  * are serialized on a dedicated daemon thread so a slow PC request cannot
  * block the caller's protocol threads.
  */
final class InProcessPcWorker(
    val facade: PcFacade,
    onShutdown: () => Unit = () => ()
) extends PcWorkerApi:

  private val executor = Executors.newSingleThreadExecutor { r =>
    val t = new Thread(r, "ls-pc-inprocess-worker")
    t.setDaemon(true)
    t
  }

  private def async[A](body: => A): CompletableFuture[A] =
    CompletableFuture.supplyAsync(() => body, executor)

  override def initializeTarget(params: PcWorkerTargetParams): CompletableFuture[String] =
    async {
      facade.registerTarget(PcWorkerTargetParams.toConfig(params))
      "ok"
    }

  override def didOpen(params: PcWorkerDidOpenParams): CompletableFuture[String] =
    async {
      facade.didOpen(params.targetId, params.uri, params.text)
      "ok"
    }

  override def didChange(params: PcWorkerChangeParams): CompletableFuture[String] =
    async {
      facade.didChange(params.uri, params.text)
      "ok"
    }

  override def didClose(params: PcWorkerUriParams): CompletableFuture[String] =
    async {
      facade.didClose(params.uri)
      "ok"
    }

  override def completion(params: PcWorkerPositionParams): CompletableFuture[CompletionList] =
    async(facade.completion(params.uri, params.line, params.character))

  override def completionItemResolve(params: PcWorkerResolveParams): CompletableFuture[CompletionItem] =
    async(facade.completionItemResolve(params.targetId, params.item, params.symbol))

  override def hover(params: PcWorkerPositionParams): CompletableFuture[Hover] =
    async(facade.hover(params.uri, params.line, params.character).orNull)

  override def signatureHelp(params: PcWorkerPositionParams): CompletableFuture[SignatureHelp] =
    async(facade.signatureHelp(params.uri, params.line, params.character))

  override def definition(params: PcWorkerPositionParams): CompletableFuture[PcWorkerDefinitionResult] =
    async(PcWorkerDefinitionResult.of(facade.definition(params.uri, params.line, params.character)))

  override def typeDefinition(params: PcWorkerPositionParams): CompletableFuture[PcWorkerDefinitionResult] =
    async(PcWorkerDefinitionResult.of(facade.typeDefinition(params.uri, params.line, params.character)))

  override def prepareRename(params: PcWorkerPositionParams): CompletableFuture[Range] =
    async(facade.prepareRename(params.uri, params.line, params.character).orNull)

  override def pluginStatus(): CompletableFuture[PcWorkerPluginStatus] =
    async(PcWorkerPluginStatus.of(facade.pluginStatus))

  override def shutdown(): CompletableFuture[String] =
    async {
      facade.shutdown()
      onShutdown()
      "ok"
    }
