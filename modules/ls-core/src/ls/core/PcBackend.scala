package ls.core

import java.util.concurrent.CompletableFuture

import scala.collection.concurrent.TrieMap
import scala.util.control.NonFatal

import ls.pc.{
  DefinitionResult,
  ForkedPcWorker,
  PcFacade,
  PcPluginStatusReport,
  PcTargetConfig,
  PcWorkerChangeParams,
  PcWorkerDefinitionResult,
  PcWorkerDidOpenParams,
  PcWorkerPluginStatus,
  PcWorkerPositionParams,
  PcWorkerResolveParams,
  PcWorkerTargetParams,
  PcWorkerUriParams
}
import org.eclipse.lsp4j.{CompletionItem, CompletionList, Hover, Range, SignatureHelp}

/** Selects how the presentation compiler runs (plan 5.2). */
enum PcBackendMode:
  case InProcess, Forked

/** The core-facing PC surface. Every PC call from the LSP core routes through
  * this seam, so the presentation compiler can run in-process (default) or in
  * an isolated child JVM (`--forked-pc`) without the core knowing which — a
  * user plugin crash in a forked child can never take down the main LS index
  * (plan 5.2).
  */
trait PcBackend:
  def registerTarget(config: PcTargetConfig): Unit
  def didOpen(targetId: String, uri: String, text: String): Unit
  def didChange(uri: String, text: String): Unit
  def didClose(uri: String): Unit
  def bufferText(uri: String): Option[String]
  def completion(uri: String, line: Int, character: Int): CompletionList
  def completionItemResolve(targetId: String, item: CompletionItem, symbol: String): CompletionItem
  def hover(uri: String, line: Int, character: Int): Option[Hover]
  def signatureHelp(uri: String, line: Int, character: Int): SignatureHelp
  def definition(uri: String, line: Int, character: Int): DefinitionResult
  def typeDefinition(uri: String, line: Int, character: Int): DefinitionResult
  def prepareRename(uri: String, line: Int, character: Int): Option[Range]
  def pluginStatus: PcPluginStatusReport
  def activeTargets: Vector[String]
  def registeredTargets: Vector[String]
  def shutdown(): Unit

  /** `Some(isAlive)` for a forked worker (drives the doctor "forked worker
    * alive" line); `None` for the in-process backend.
    */
  def workerAlive: Option[Boolean]

  /** OS pid of the forked child, if any — a fault-injection test hook: kill
    * this pid and the next request respawns and replays.
    */
  def workerPid: Option[Long]

  /** Kill the current worker. A forked backend respawns and replays its
    * targets/buffers on the next request; the in-process backend is a no-op.
    */
  def restartWorker(): Unit

/** In-process backend: the presentation compiler runs in this JVM over a
  * [[PcFacade]]. The default until the forked mode is promoted to production.
  */
final class InProcessPcBackend(val facade: PcFacade) extends PcBackend:
  def registerTarget(config: PcTargetConfig): Unit = facade.registerTarget(config)
  def didOpen(targetId: String, uri: String, text: String): Unit = facade.didOpen(targetId, uri, text)
  def didChange(uri: String, text: String): Unit = facade.didChange(uri, text)
  def didClose(uri: String): Unit = facade.didClose(uri)
  def bufferText(uri: String): Option[String] = facade.bufferText(uri)
  def completion(uri: String, line: Int, character: Int): CompletionList =
    facade.completion(uri, line, character)
  def completionItemResolve(targetId: String, item: CompletionItem, symbol: String): CompletionItem =
    facade.completionItemResolve(targetId, item, symbol)
  def hover(uri: String, line: Int, character: Int): Option[Hover] = facade.hover(uri, line, character)
  def signatureHelp(uri: String, line: Int, character: Int): SignatureHelp =
    facade.signatureHelp(uri, line, character)
  def definition(uri: String, line: Int, character: Int): DefinitionResult =
    facade.definition(uri, line, character)
  def typeDefinition(uri: String, line: Int, character: Int): DefinitionResult =
    facade.typeDefinition(uri, line, character)
  def prepareRename(uri: String, line: Int, character: Int): Option[Range] =
    facade.prepareRename(uri, line, character)
  def pluginStatus: PcPluginStatusReport = facade.pluginStatus
  def activeTargets: Vector[String] = facade.activeTargets
  def registeredTargets: Vector[String] = facade.registeredTargets
  def shutdown(): Unit = facade.shutdown()
  def workerAlive: Option[Boolean] = None
  def workerPid: Option[Long] = None
  def restartWorker(): Unit = ()

/** Forked backend: the presentation compiler runs in a child JVM proxied by
  * [[ForkedPcWorker]] over the PC worker protocol. Queries and lifecycle calls
  * cross the process boundary; a light local mirror answers the synchronous
  * reads (`bufferText`/`registeredTargets`/`activeTargets`) without a
  * round-trip. Plugin execution and its containment happen in the child.
  */
final class ForkedPcBackend(val worker: ForkedPcWorker) extends PcBackend:

  private final case class Buffer(targetId: String, text: String)
  private val targets = TrieMap.empty[String, PcTargetConfig]
  private val buffers = TrieMap.empty[String, Buffer]

  // Every forked request already carries an internal orTimeout in
  // ForkedPcWorker, so the future always completes; get() never hangs.
  private def await[A](f: CompletableFuture[A]): A =
    try f.get()
    catch
      case e: java.util.concurrent.ExecutionException if e.getCause != null =>
        throw e.getCause

  private def pos(uri: String, line: Int, character: Int): PcWorkerPositionParams =
    val p = new PcWorkerPositionParams
    p.uri = uri
    p.line = line
    p.character = character
    p

  def registerTarget(config: PcTargetConfig): Unit =
    targets.put(config.bspId, config)
    await(worker.initializeTarget(PcWorkerTargetParams.of(config)))

  def didOpen(targetId: String, uri: String, text: String): Unit =
    require(targets.contains(targetId), s"didOpen for unregistered target '$targetId'")
    buffers.put(uri, Buffer(targetId, text))
    val p = new PcWorkerDidOpenParams
    p.targetId = targetId
    p.uri = uri
    p.text = text
    await(worker.didOpen(p))

  def didChange(uri: String, text: String): Unit =
    buffers.get(uri).foreach(b => buffers.put(uri, b.copy(text = text)))
    val p = new PcWorkerChangeParams
    p.uri = uri
    p.text = text
    await(worker.didChange(p))

  def didClose(uri: String): Unit =
    buffers.remove(uri)
    val p = new PcWorkerUriParams
    p.uri = uri
    await(worker.didClose(p))

  def bufferText(uri: String): Option[String] = buffers.get(uri).map(_.text)

  def completion(uri: String, line: Int, character: Int): CompletionList =
    await(worker.completion(pos(uri, line, character)))

  def completionItemResolve(targetId: String, item: CompletionItem, symbol: String): CompletionItem =
    val p = new PcWorkerResolveParams
    p.targetId = targetId
    p.symbol = symbol
    p.item = item
    await(worker.completionItemResolve(p))

  def hover(uri: String, line: Int, character: Int): Option[Hover] =
    Option(await(worker.hover(pos(uri, line, character))))

  def signatureHelp(uri: String, line: Int, character: Int): SignatureHelp =
    await(worker.signatureHelp(pos(uri, line, character)))

  def definition(uri: String, line: Int, character: Int): DefinitionResult =
    PcWorkerDefinitionResult.toDefinitionResult(await(worker.definition(pos(uri, line, character))))

  def typeDefinition(uri: String, line: Int, character: Int): DefinitionResult =
    PcWorkerDefinitionResult.toDefinitionResult(await(worker.typeDefinition(pos(uri, line, character))))

  def prepareRename(uri: String, line: Int, character: Int): Option[Range] =
    Option(await(worker.prepareRename(pos(uri, line, character))))

  def pluginStatus: PcPluginStatusReport =
    PcWorkerPluginStatus.toReport(await(worker.pluginStatus()))

  def activeTargets: Vector[String] =
    buffers.values.map(_.targetId).toVector.distinct.sorted

  def registeredTargets: Vector[String] = targets.keySet.toVector

  def shutdown(): Unit =
    try worker.close()
    catch case NonFatal(_) => ()

  def workerAlive: Option[Boolean] = Some(worker.isAlive)
  def workerPid: Option[Long] = worker.pid
  def restartWorker(): Unit = worker.restart()
