package ls.pc

import java.nio.file.{Path, Paths}
import java.util.concurrent.{CompletableFuture, CompletionException, Executors, Future, TimeUnit, TimeoutException}

import scala.jdk.CollectionConverters.*
import scala.util.control.NonFatal

import org.eclipse.lsp4j.jsonrpc.Launcher
import org.eclipse.lsp4j.{CompletionItem, CompletionList, Hover, Range, SignatureHelp}

/** [[PcWorkerApi]] proxy over a forked PC worker JVM (plan 5.2: PC worker is
  * a separate JVM; user plugin crashes cannot break the main LS index).
  *
  * - The child (`java -cp <classpath> ls.pc.PcWorkerMain <workerArgs>`) is
  *   spawned lazily on first use and speaks JSON-RPC on stdin/stdout.
  * - Every request carries a timeout; a timed-out (wedged) worker is killed
  *   and respawned on the next request.
  * - Initialized targets and open buffers are remembered and replayed into a
  *   fresh child after a restart, so a wedge is a latency blip, not a state
  *   loss for the caller.
  */
final class ForkedPcWorker(
    javaExecutable: Path = Paths.get(System.getProperty("java.home"), "bin", "java"),
    classpath: String = System.getProperty("java.class.path", ""),
    jvmArgs: Vector[String] = Vector.empty,
    workerArgs: Vector[String] = Vector.empty,
    requestTimeoutMillis: Long = 60000,
    /** Parent-side cross-file definition lookup: the child's PC calls back
      * over `pc/symbolDefinition` and this resolver (index-backed in
      * production) answers. Defaults to a no-op.
      */
    resolver: PcDefinitionResolver = PcDefinitionResolver.Empty
) extends PcWorkerApi
    with AutoCloseable:

  private final case class Session(process: Process, launcher: Launcher[PcWorkerApi], listening: Future[Void])

  private val lock = new Object
  private var session: Option[Session] = None
  private var closed = false

  // replay state, guarded by `lock`
  private var knownTargets = Map.empty[String, PcWorkerTargetParams]
  private var openBuffers = Map.empty[String, PcWorkerDidOpenParams]

  private val jsonrpcExecutor = Executors.newCachedThreadPool { r =>
    val t = new Thread(r, "ls-pc-forked-worker-client")
    t.setDaemon(true)
    t
  }

  def isAlive: Boolean = lock.synchronized(session.exists(_.process.isAlive))

  /** OS process id of the live child, if any (test hook for fault injection:
    * kill this pid and the next request respawns + replays).
    */
  def pid: Option[Long] = lock.synchronized(session.map(_.process.pid()))

  /** Kill the child (if any). The next request spawns a fresh one and replays
    * targets/buffers.
    */
  def restart(): Unit = lock.synchronized(killLocked())

  // --- PcWorkerApi -----------------------------------------------------------

  override def initializeTarget(params: PcWorkerTargetParams): CompletableFuture[String] =
    lock.synchronized { knownTargets = knownTargets.updated(params.bspId, params) }
    guarded("pc/initializeTarget")(_.initializeTarget(params))

  override def didOpen(params: PcWorkerDidOpenParams): CompletableFuture[String] =
    lock.synchronized { openBuffers = openBuffers.updated(params.uri, params) }
    guarded("pc/didOpen")(_.didOpen(params))

  override def didChange(params: PcWorkerChangeParams): CompletableFuture[String] =
    lock.synchronized {
      openBuffers.get(params.uri).foreach { open =>
        val updated = new PcWorkerDidOpenParams
        updated.targetId = open.targetId
        updated.uri = open.uri
        updated.text = params.text
        openBuffers = openBuffers.updated(params.uri, updated)
      }
    }
    guarded("pc/didChange")(_.didChange(params))

  override def didClose(params: PcWorkerUriParams): CompletableFuture[String] =
    lock.synchronized { openBuffers = openBuffers.removed(params.uri) }
    guarded("pc/didClose")(_.didClose(params))

  override def completion(params: PcWorkerPositionParams): CompletableFuture[CompletionList] =
    guarded("pc/completion")(_.completion(params))

  override def completionItemResolve(params: PcWorkerResolveParams): CompletableFuture[CompletionItem] =
    guarded("pc/completionResolve")(_.completionItemResolve(params))

  override def hover(params: PcWorkerPositionParams): CompletableFuture[Hover] =
    guarded("pc/hover")(_.hover(params))

  override def signatureHelp(params: PcWorkerPositionParams): CompletableFuture[SignatureHelp] =
    guarded("pc/signatureHelp")(_.signatureHelp(params))

  override def definition(params: PcWorkerPositionParams): CompletableFuture[PcWorkerDefinitionResult] =
    guarded("pc/definition")(_.definition(params))

  override def typeDefinition(params: PcWorkerPositionParams): CompletableFuture[PcWorkerDefinitionResult] =
    guarded("pc/typeDefinition")(_.typeDefinition(params))

  override def prepareRename(params: PcWorkerPositionParams): CompletableFuture[Range] =
    guarded("pc/prepareRename")(_.prepareRename(params))

  override def pluginStatus(): CompletableFuture[PcWorkerPluginStatus] =
    guarded("pc/pluginStatus")(_.pluginStatus())

  override def shutdown(): CompletableFuture[String] =
    val existing = lock.synchronized {
      closed = true
      session
    }
    existing match
      case None => CompletableFuture.completedFuture("ok")
      case Some(s) =>
        val result = new CompletableFuture[String]()
        try
          s.launcher.getRemoteProxy
            .shutdown()
            .orTimeout(5000, TimeUnit.MILLISECONDS)
            .whenComplete { (value, err) =>
              try lock.synchronized(killLocked())
              catch case _: Throwable => ()
              result.complete(if err != null then "ok (forced)" else value)
            }
        catch
          case NonFatal(_) =>
            lock.synchronized(killLocked())
            result.complete("ok (forced)")
        result

  override def close(): Unit =
    try shutdown().get(10, TimeUnit.SECONDS)
    catch case NonFatal(_) => lock.synchronized(killLocked())

  // --- internals ---------------------------------------------------------------

  private def guarded[A](name: String)(call: PcWorkerApi => CompletableFuture[A]): CompletableFuture[A] =
    val proxy =
      try ensureSession().launcher.getRemoteProxy
      catch case NonFatal(t) => return CompletableFuture.failedFuture(t)
    val fut =
      try call(proxy)
      catch case NonFatal(t) => return CompletableFuture.failedFuture(t)
    fut
      .orTimeout(requestTimeoutMillis, TimeUnit.MILLISECONDS)
      .whenComplete { (_, err) =>
        unwrap(err) match
          case _: TimeoutException =>
            // wedged worker: kill it; the next request respawns and replays
            System.err.println(s"[forked-pc-worker] request $name timed out after ${requestTimeoutMillis}ms; killing worker")
            lock.synchronized(killLocked())
          case _ => ()
      }

  private def unwrap(t: Throwable): Throwable = t match
    case e: CompletionException if e.getCause != null => e.getCause
    case other => other

  private def ensureSession(): Session = lock.synchronized {
    if closed then throw new IllegalStateException("ForkedPcWorker is closed")
    session.filter(_.process.isAlive) match
      case Some(alive) => alive
      case None =>
        session.foreach(_ => killLocked())
        val fresh = spawnLocked()
        session = Some(fresh)
        replayLocked(fresh)
        fresh
  }

  private def spawnLocked(): Session =
    val cmd =
      (Vector(javaExecutable.toString) ++ jvmArgs ++
        Vector("-cp", classpath, "ls.pc.PcWorkerMain") ++ workerArgs).asJava
    val builder = new ProcessBuilder(cmd)
    builder.redirectError(ProcessBuilder.Redirect.INHERIT)
    val process = builder.start()
    val launcher = new Launcher.Builder[PcWorkerApi]()
      .setLocalService(new ResolverPcWorkerClient(resolver))
      .setRemoteInterface(classOf[PcWorkerApi])
      .setInput(process.getInputStream)
      .setOutput(process.getOutputStream)
      .setExecutorService(jsonrpcExecutor)
      .create()
    val listening = launcher.startListening()
    Session(process, launcher, listening)

  /** Re-initialize targets and reopen buffers in a fresh child. Failures are
    * logged, not thrown: the triggering request will surface real errors.
    */
  private def replayLocked(s: Session): Unit =
    val proxy = s.launcher.getRemoteProxy
    knownTargets.values.foreach { t =>
      try proxy.initializeTarget(t).get(requestTimeoutMillis, TimeUnit.MILLISECONDS)
      catch case NonFatal(t2) => System.err.println(s"[forked-pc-worker] replay initializeTarget failed: $t2")
    }
    openBuffers.values.foreach { b =>
      try proxy.didOpen(b).get(requestTimeoutMillis, TimeUnit.MILLISECONDS)
      catch case NonFatal(t2) => System.err.println(s"[forked-pc-worker] replay didOpen failed: $t2")
    }

  private def killLocked(): Unit =
    session.foreach { s =>
      // Destroy the process first; the protocol reader thread then terminates
      // on stream EOF. Never cancel(true): the killing thread may *be* a
      // jsonrpc callback thread and must not interrupt itself.
      try
        s.process.destroy()
        if !s.process.waitFor(2, TimeUnit.SECONDS) then
          s.process.destroyForcibly()
          ()
      catch
        case _: InterruptedException =>
          s.process.destroyForcibly()
          Thread.interrupted() // clear the flag; pool threads are reused
          ()
        case NonFatal(_) =>
          s.process.destroyForcibly()
          ()
      try s.listening.cancel(false)
      catch case NonFatal(_) => ()
    }
    session = None
