package ls.bsp

import java.io.BufferedReader
import java.io.InputStream
import java.io.InputStreamReader
import java.io.OutputStream
import java.nio.charset.StandardCharsets
import java.nio.file.Path
import java.util.concurrent.CancellationException
import java.util.concurrent.CompletableFuture
import java.util.concurrent.ExecutionException
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
import java.util.concurrent.ThreadFactory
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException
import java.util.concurrent.atomic.AtomicInteger

import scala.concurrent.duration.*
import scala.jdk.CollectionConverters.*
import scala.util.control.NonFatal

import ch.epfl.scala.bsp4j.*
import org.eclipse.lsp4j.jsonrpc.Launcher

/** Remote interface this client speaks: core BSP plus the Scala extension
  * (buildTarget/scalacOptions and friends).
  */
trait LsBuildServer extends BuildServer with ScalaBuildServer

final case class BspSessionConfig(
    clientName: String = "scala3-bsp-semantic-ls",
    clientVersion: String = "0.1.0",
    requestTimeout: FiniteDuration = 30.seconds,
    shutdownTimeout: FiniteDuration = 5.seconds
)

/** Typed result of buildTarget/compile. */
enum BspCompileOutcome:
  case Ok(originId: Option[String])
  case Failed(statusCode: StatusCode, originId: Option[String])

  def isOk: Boolean = this match
    case Ok(_) => true
    case Failed(_, _) => false

/** One live connection to a BSP server. Every request is bounded by
  * `config.requestTimeout` and failures are rethrown as [[BspException]]
  * with a typed [[BspError]]. Not request-reentrant safeguards are needed:
  * lsp4j serializes writes internally, so the session is thread-safe.
  */
final class BspSession private[bsp] (
    val workspaceRoot: Path,
    process: Option[Process],
    input: InputStream,
    output: OutputStream,
    executor: ExecutorService,
    server: LsBuildServer,
    listening: java.util.concurrent.Future[Void],
    config: BspSessionConfig
):
  @volatile private var initializeResult: Option[InitializeBuildResult] = None
  @volatile private var closed = false

  def isClosed: Boolean = closed

  /** Alive-flag of the launched server process; None for stream-connected
    * sessions (tests, sockets).
    */
  def serverProcessAlive: Option[Boolean] = process.map(_.isAlive)

  /** Capabilities from build/initialize; None before [[initialize]] ran. */
  def serverCapabilities: Option[BuildServerCapabilities] =
    initializeResult.map(_.getCapabilities)

  /** build/initialize (languageIds = ["scala"]) followed by the
    * build/initialized notification.
    */
  def initialize(): InitializeBuildResult =
    val caps = new BuildClientCapabilities(java.util.List.of("scala"))
    val params = new InitializeBuildParams(
      config.clientName,
      config.clientVersion,
      Bsp4j.PROTOCOL_VERSION,
      workspaceRoot.toUri.toString,
      caps
    )
    val result = request("build/initialize")(_.buildInitialize(params))
    initializeResult = Some(result)
    notification("build/initialized")(_.onBuildInitialized())
    result

  def workspaceBuildTargets(): Vector[BuildTarget] =
    val result = request("workspace/buildTargets")(_.workspaceBuildTargets())
    listOf("workspace/buildTargets", result.getTargets)

  def buildTargetSources(bspIds: Vector[String]): Vector[SourcesItem] =
    val params = new SourcesParams(idsOf(bspIds))
    val result = request("buildTarget/sources")(_.buildTargetSources(params))
    listOf("buildTarget/sources", result.getItems)

  def buildTargetScalacOptions(bspIds: Vector[String]): Vector[ScalacOptionsItem] =
    val params = new ScalacOptionsParams(idsOf(bspIds))
    val result = request("buildTarget/scalacOptions")(_.buildTargetScalacOptions(params))
    listOf("buildTarget/scalacOptions", result.getItems)

  /** buildTarget/compile. Diagnostics arrive through the client handlers
    * while the request is in flight; the status code is returned typed.
    */
  def compile(bspIds: Vector[String], originId: Option[String] = None): BspCompileOutcome =
    val params = new CompileParams(idsOf(bspIds))
    originId.foreach(params.setOriginId)
    val result = request("buildTarget/compile")(_.buildTargetCompile(params))
    val resultOrigin = Option(result.getOriginId)
    result.getStatusCode match
      case StatusCode.OK => BspCompileOutcome.Ok(resultOrigin)
      case null =>
        throw BspException(BspError.InvalidResponse("buildTarget/compile", "missing statusCode"))
      case other => BspCompileOutcome.Failed(other, resultOrigin)

  /** Raw buildTarget/inverseSources call, regardless of capabilities. */
  def serverInverseSources(uri: String): Vector[String] =
    val params = new InverseSourcesParams(new TextDocumentIdentifier(uri))
    val result = request("buildTarget/inverseSources")(_.buildTargetInverseSources(params))
    listOf("buildTarget/inverseSources", result.getTargets).map(_.getUri)

  /** Uses the server when it advertises inverseSourcesProvider, otherwise
    * falls back to the local uri -> target map of the project model.
    */
  def inverseSources(uri: String, model: BspProjectModel): Vector[String] =
    val advertised = serverCapabilities.exists { caps =>
      Option(caps.getInverseSourcesProvider).exists(_.booleanValue)
    }
    if advertised then serverInverseSources(uri)
    else model.uriToTarget.get(uri).toVector

  /** Graceful buildShutdown + build/exit, then stream/process teardown. Each
    * step is best-effort and bounded by `config.shutdownTimeout`.
    */
  def shutdown(): Unit =
    if closed then return
    try
      try
        server.buildShutdown().get(config.shutdownTimeout.toMillis, TimeUnit.MILLISECONDS)
        ()
      catch
        case _: InterruptedException => Thread.currentThread().interrupt()
        case NonFatal(_) => ()
      try server.onBuildExit()
      catch case NonFatal(_) => ()
    finally close()

  /** Hard teardown: stops listening, closes streams, terminates the server
    * process (waiting `config.shutdownTimeout`, then destroy, then forcibly).
    */
  def close(): Unit =
    if closed then return
    closed = true
    listening.cancel(true)
    closeQuietly(output)
    closeQuietly(input)
    executor.shutdown()
    process.foreach(terminate)

  private def terminate(p: Process): Unit =
    if waitQuietly(p, config.shutdownTimeout.toMillis) then return
    p.destroy()
    if waitQuietly(p, 1000L) then return
    p.destroyForcibly()
    waitQuietly(p, 1000L)
    ()

  private def waitQuietly(p: Process, millis: Long): Boolean =
    try p.waitFor(millis, TimeUnit.MILLISECONDS)
    catch
      case _: InterruptedException =>
        Thread.currentThread().interrupt()
        false

  private def closeQuietly(c: java.io.Closeable): Unit =
    try c.close()
    catch case NonFatal(_) => ()

  private def idsOf(bspIds: Vector[String]): java.util.List[BuildTargetIdentifier] =
    bspIds.map(new BuildTargetIdentifier(_)).asJava

  private def listOf[A](method: String, list: java.util.List[A]): Vector[A] =
    if list == null then throw BspException(BspError.InvalidResponse(method, "missing list field"))
    list.asScala.toVector

  private def request[A](method: String)(call: LsBuildServer => CompletableFuture[A]): A =
    if closed then throw BspException(BspError.SessionClosed(method))
    val future =
      try call(server)
      catch case NonFatal(e) => throw BspException(BspError.RequestFailed(method, describe(e)))
    try future.get(config.requestTimeout.toMillis, TimeUnit.MILLISECONDS)
    catch
      case _: TimeoutException =>
        future.cancel(true)
        throw BspException(BspError.RequestTimeout(method, config.requestTimeout.toMillis))
      case e: ExecutionException =>
        val cause = if e.getCause != null then e.getCause else e
        throw BspException(BspError.RequestFailed(method, describe(cause)))
      case _: CancellationException =>
        throw BspException(BspError.RequestFailed(method, "request was cancelled"))
      case _: InterruptedException =>
        Thread.currentThread().interrupt()
        future.cancel(true)
        throw BspException(BspError.RequestFailed(method, "interrupted while waiting for response"))

  private def notification(method: String)(call: LsBuildServer => Unit): Unit =
    if closed then throw BspException(BspError.SessionClosed(method))
    try call(server)
    catch case NonFatal(e) => throw BspException(BspError.RequestFailed(method, describe(e)))

  private def describe(t: Throwable): String =
    val cls = t.getClass.getSimpleName
    Option(t.getMessage).filter(_.nonEmpty).map(m => s"$cls: $m").getOrElse(cls)

object BspSession:
  private val threadCounter = new AtomicInteger(1)

  /** Launches the BSP server process described by a connection file (argv as
    * given, cwd = workspace root) and connects over its stdio.
    */
  def launch(
      workspaceRoot: Path,
      details: BspConnectionDetails,
      handlers: BspClientHandlers = BspClientHandlers(),
      config: BspSessionConfig = BspSessionConfig()
  ): BspSession =
    val name = Option(details.getName).getOrElse("<unnamed>")
    val argv = Option(details.getArgv).map(_.asScala.toVector).getOrElse(Vector.empty)
    if argv.isEmpty then
      throw BspException(BspError.LaunchFailed(name, "connection file has empty argv"))
    val process =
      try
        val pb = new ProcessBuilder(argv.asJava)
        pb.directory(workspaceRoot.toFile)
        pb.start()
      catch
        case NonFatal(e) =>
          throw BspException(
            BspError.LaunchFailed(name, Option(e.getMessage).getOrElse(e.getClass.getSimpleName))
          )
    pumpStderr(process, handlers)
    make(workspaceRoot, Some(process), process.getInputStream, process.getOutputStream, handlers, config)

  /** Connects over arbitrary streams (in-process servers in tests, sockets).
    * `input` carries server -> client messages, `output` client -> server.
    */
  def connect(
      workspaceRoot: Path,
      input: InputStream,
      output: OutputStream,
      handlers: BspClientHandlers = BspClientHandlers(),
      config: BspSessionConfig = BspSessionConfig()
  ): BspSession =
    make(workspaceRoot, None, input, output, handlers, config)

  private def make(
      workspaceRoot: Path,
      process: Option[Process],
      input: InputStream,
      output: OutputStream,
      handlers: BspClientHandlers,
      config: BspSessionConfig
  ): BspSession =
    val executor = Executors.newCachedThreadPool(daemonFactory("bsp-session"))
    val client = new ForwardingBuildClient(handlers)
    val launcher: Launcher[LsBuildServer] = new Launcher.Builder[LsBuildServer]()
      .setLocalService(client)
      .setRemoteInterface(classOf[LsBuildServer])
      .setInput(input)
      .setOutput(output)
      .setExecutorService(executor)
      .create()
    val listening = launcher.startListening()
    new BspSession(
      workspaceRoot,
      process,
      input,
      output,
      executor,
      launcher.getRemoteProxy,
      listening,
      config
    )

  private def pumpStderr(process: Process, handlers: BspClientHandlers): Unit =
    val t = new Thread(
      () =>
        try
          val reader =
            new BufferedReader(new InputStreamReader(process.getErrorStream, StandardCharsets.UTF_8))
          var line = reader.readLine()
          while line != null do
            handlers.onServerStderr(line)
            line = reader.readLine()
        catch case NonFatal(_) => (),
      s"bsp-server-stderr-${threadCounter.getAndIncrement()}"
    )
    t.setDaemon(true)
    t.start()

  private def daemonFactory(prefix: String): ThreadFactory = runnable =>
    val t = new Thread(runnable, s"$prefix-${threadCounter.getAndIncrement()}")
    t.setDaemon(true)
    t
