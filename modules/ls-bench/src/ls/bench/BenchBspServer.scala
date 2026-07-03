package ls.bench

import java.io.{PipedInputStream, PipedOutputStream}
import java.nio.file.{Files, Path}
import java.util.concurrent.{CompletableFuture, Executors}

import scala.jdk.CollectionConverters.*

import ch.epfl.scala.bsp4j.*
import org.eclipse.lsp4j.jsonrpc.Launcher

import ls.bsp.{BspSession, BspSessionConfig}

/** Minimal in-process BSP server serving `n` synthetic Scala 3 targets
  * (`t0..t{n-1}`, each depending on the previous), used only to time
  * `ProjectModelLoader.load`. Each target advertises one file source, a
  * `-semanticdb-target:` root and a `-sourceroot`, so the loaded model has
  * indexable targets with dependency edges — the ground truth the BSP-import
  * bench row checks against.
  */
final class BenchBspServer(workspaceRoot: Path, n: Int) extends BuildServer with ScalaBuildServer:

  @volatile var client: BuildClient = scala.compiletime.uninitialized

  def idOf(i: Int): String = s"bsp://bench/t$i"
  private def tid(i: Int) = new BuildTargetIdentifier(idOf(i))
  def sourceUri(i: Int): String = workspaceRoot.resolve("src").resolve(s"t$i.scala").toUri.toString
  def semanticdbRoot(i: Int): Path = workspaceRoot.resolve("out").resolve(s"t$i")
  private def classDir(i: Int): Path = semanticdbRoot(i).resolve("classes")

  private def data = new ScalaBuildTarget("org.scala-lang", "3.8.4", "3", ScalaPlatform.JVM, java.util.List.of())

  private def target(i: Int): BuildTarget =
    val caps = new BuildTargetCapabilities(); caps.setCanCompile(true)
    val deps = if i > 0 then java.util.List.of(tid(i - 1)) else java.util.List.of[BuildTargetIdentifier]()
    val t = new BuildTarget(tid(i), java.util.List.of(), java.util.List.of("scala"), deps, caps)
    t.setDisplayName(s"t$i")
    t.setDataKind(BuildTargetDataKind.SCALA)
    t.setData(data)
    t

  private def indexOf(id: String): Int = id.stripPrefix("bsp://bench/t").toInt

  override def buildInitialize(params: InitializeBuildParams): CompletableFuture[InitializeBuildResult] =
    val caps = new BuildServerCapabilities()
    caps.setCompileProvider(new CompileProvider(java.util.List.of("scala")))
    CompletableFuture.completedFuture(
      new InitializeBuildResult("bench-bsp", "0.0.1", Bsp4j.PROTOCOL_VERSION, caps)
    )
  override def onBuildInitialized(): Unit = ()
  override def buildShutdown(): CompletableFuture[Object] = CompletableFuture.completedFuture(null)
  override def onBuildExit(): Unit = ()

  override def workspaceBuildTargets(): CompletableFuture[WorkspaceBuildTargetsResult] =
    CompletableFuture.completedFuture(
      new WorkspaceBuildTargetsResult((0 until n).map(target).asJava)
    )

  override def buildTargetSources(params: SourcesParams): CompletableFuture[SourcesResult] =
    val items = params.getTargets.asScala.map { id =>
      val i = indexOf(id.getUri)
      new SourcesItem(id, java.util.List.of(new SourceItem(sourceUri(i), SourceItemKind.FILE, false)))
    }
    CompletableFuture.completedFuture(new SourcesResult(items.asJava))

  override def buildTargetScalacOptions(params: ScalacOptionsParams): CompletableFuture[ScalacOptionsResult] =
    val items = params.getTargets.asScala.map { id =>
      val i = indexOf(id.getUri)
      new ScalacOptionsItem(
        id,
        java.util.List.of("-Xsemanticdb", s"-semanticdb-target:${semanticdbRoot(i)}", "-sourceroot", workspaceRoot.toString),
        java.util.List.of(),
        classDir(i).toUri.toString
      )
    }
    CompletableFuture.completedFuture(new ScalacOptionsResult(items.asJava))

  private def unsupported[A](m: String): CompletableFuture[A] =
    CompletableFuture.failedFuture(new UnsupportedOperationException(m))
  override def buildTargetInverseSources(p: InverseSourcesParams): CompletableFuture[InverseSourcesResult] =
    unsupported("inverseSources")
  override def buildTargetCompile(p: CompileParams): CompletableFuture[CompileResult] =
    CompletableFuture.completedFuture(new CompileResult(StatusCode.OK))
  override def workspaceReload(): CompletableFuture[Object] = unsupported("reload")
  override def buildTargetDependencySources(p: DependencySourcesParams): CompletableFuture[DependencySourcesResult] =
    unsupported("dependencySources")
  override def buildTargetDependencyModules(p: DependencyModulesParams): CompletableFuture[DependencyModulesResult] =
    unsupported("dependencyModules")
  override def buildTargetResources(p: ResourcesParams): CompletableFuture[ResourcesResult] =
    unsupported("resources")
  override def buildTargetOutputPaths(p: OutputPathsParams): CompletableFuture[OutputPathsResult] =
    unsupported("outputPaths")
  override def buildTargetRun(p: RunParams): CompletableFuture[RunResult] = unsupported("run")
  override def buildTargetTest(p: TestParams): CompletableFuture[TestResult] = unsupported("test")
  override def debugSessionStart(p: DebugSessionParams): CompletableFuture[DebugSessionAddress] =
    unsupported("debug")
  override def buildTargetCleanCache(p: CleanCacheParams): CompletableFuture[CleanCacheResult] =
    unsupported("cleanCache")
  override def onRunReadStdin(p: ReadParams): Unit = ()
  override def buildTargetScalaTestClasses(p: ScalaTestClassesParams): CompletableFuture[ScalaTestClassesResult] =
    unsupported("scalaTestClasses")
  override def buildTargetScalaMainClasses(p: ScalaMainClassesParams): CompletableFuture[ScalaMainClassesResult] =
    unsupported("scalaMainClasses")

object BenchBspServer:

  /** An initialized in-process [[BspSession]] talking to a fresh
    * [[BenchBspServer]] with `n` targets, over piped streams. Returns the
    * session and a close hook.
    */
  def connect(workspaceRoot: Path, n: Int): (BspSession, () => Unit) =
    Files.createDirectories(workspaceRoot.resolve("src"))
    val fake = new BenchBspServer(workspaceRoot, n)
    val toClient = new PipedInputStream(1 << 20)
    val serverOut = new PipedOutputStream(toClient)
    val toServer = new PipedInputStream(1 << 20)
    val clientOut = new PipedOutputStream(toServer)
    val exec = Executors.newCachedThreadPool { r =>
      val t = new Thread(r, "bench-bsp-server"); t.setDaemon(true); t
    }
    val serverLauncher = new Launcher.Builder[BuildClient]()
      .setLocalService(fake)
      .setRemoteInterface(classOf[BuildClient])
      .setInput(toServer)
      .setOutput(serverOut)
      .setExecutorService(exec)
      .create()
    fake.client = serverLauncher.getRemoteProxy
    val listening = serverLauncher.startListening()
    val session = BspSession.connect(workspaceRoot, toClient, clientOut)
    session.initialize()
    val close = () =>
      try session.shutdown()
      catch case scala.util.control.NonFatal(_) => ()
      listening.cancel(true)
      exec.shutdownNow()
      ()
    (session, close)
