package ls.doctor

import java.io.{PipedInputStream, PipedOutputStream}
import java.util.concurrent.{CompletableFuture, Executors}

import scala.jdk.CollectionConverters.*

import ch.epfl.scala.bsp4j.*
import org.eclipse.lsp4j.jsonrpc.Launcher

import ls.bsp.{BspSession, LsBuildServer, ProjectModelLoader}

/** Classpath-reality check (wave 2a): mtags-interfaces 1.6.7 evicts lsp4j to
  * 1.0.0, so bsp4j 2.2.0-M2 (built against lsp4j-jsonrpc 0.20.1) must run on
  * lsp4j-jsonrpc 1.0.0. This test drives the *real* bsp4j Launcher machinery
  * on both sides (client via [[BspSession.connect]], server via a second
  * lsp4j `Launcher` over piped streams) and then feeds the results into the
  * doctor's BSP section, proving the binary compatibility this module's
  * classpath relies on.
  */
class BspLauncherCompatTest extends munit.FunSuite:

  /** Minimal in-module fake: two Scala 3 targets (`app` depends on `lib`),
    * neither producing SemanticDB, so both are IndexUnavailable.
    */
  private final class MiniBuildServer extends BuildServer with ScalaBuildServer:
    @volatile var client: BuildClient = scala.compiletime.uninitialized

    private def id(name: String) = new BuildTargetIdentifier(s"bsp://mini/$name")

    override def buildInitialize(
        params: InitializeBuildParams
    ): CompletableFuture[InitializeBuildResult] =
      val caps = new BuildServerCapabilities()
      caps.setCompileProvider(new CompileProvider(java.util.List.of("scala")))
      CompletableFuture.completedFuture(
        new InitializeBuildResult("mini-bsp", "1.2.3", Bsp4j.PROTOCOL_VERSION, caps)
      )

    override def onBuildInitialized(): Unit = ()

    override def buildShutdown(): CompletableFuture[Object] =
      CompletableFuture.completedFuture(new Object)

    override def onBuildExit(): Unit = ()

    override def workspaceBuildTargets(): CompletableFuture[WorkspaceBuildTargetsResult] =
      def target(name: String, deps: List[String]): BuildTarget =
        val caps = new BuildTargetCapabilities()
        caps.setCanCompile(true)
        val t = new BuildTarget(
          id(name),
          java.util.List.of(),
          java.util.List.of("scala"),
          deps.map(id).asJava,
          caps
        )
        t.setDisplayName(name)
        t.setDataKind(BuildTargetDataKind.SCALA)
        t.setData(
          new ScalaBuildTarget("org.scala-lang", "3.8.4", "3", ScalaPlatform.JVM, java.util.List.of())
        )
        t
      CompletableFuture.completedFuture(
        new WorkspaceBuildTargetsResult(java.util.List.of(target("lib", Nil), target("app", List("lib"))))
      )

    override def buildTargetSources(params: SourcesParams): CompletableFuture[SourcesResult] =
      val items = params.getTargets.asScala.map(t => new SourcesItem(t, java.util.List.of()))
      CompletableFuture.completedFuture(new SourcesResult(items.asJava))

    override def buildTargetScalacOptions(
        params: ScalacOptionsParams
    ): CompletableFuture[ScalacOptionsResult] =
      val items = params.getTargets.asScala.map { t =>
        new ScalacOptionsItem(
          t,
          java.util.List.of("-deprecation"),
          java.util.List.of(),
          s"file:///tmp/mini/${t.getUri.split('/').last}/classes/"
        )
      }
      CompletableFuture.completedFuture(new ScalacOptionsResult(items.asJava))

    override def buildTargetCompile(params: CompileParams): CompletableFuture[CompileResult] =
      val result = new CompileResult(StatusCode.OK)
      result.setOriginId(params.getOriginId)
      CompletableFuture.completedFuture(result)

    private def unsupported[A](method: String): CompletableFuture[A] =
      CompletableFuture.failedFuture(new UnsupportedOperationException(method))

    override def workspaceReload(): CompletableFuture[Object] = unsupported("workspace/reload")
    override def buildTargetInverseSources(
        params: InverseSourcesParams
    ): CompletableFuture[InverseSourcesResult] = unsupported("buildTarget/inverseSources")
    override def buildTargetDependencySources(
        params: DependencySourcesParams
    ): CompletableFuture[DependencySourcesResult] = unsupported("buildTarget/dependencySources")
    override def buildTargetDependencyModules(
        params: DependencyModulesParams
    ): CompletableFuture[DependencyModulesResult] = unsupported("buildTarget/dependencyModules")
    override def buildTargetResources(
        params: ResourcesParams
    ): CompletableFuture[ResourcesResult] = unsupported("buildTarget/resources")
    override def buildTargetOutputPaths(
        params: OutputPathsParams
    ): CompletableFuture[OutputPathsResult] = unsupported("buildTarget/outputPaths")
    override def buildTargetRun(params: RunParams): CompletableFuture[RunResult] =
      unsupported("buildTarget/run")
    override def buildTargetTest(params: TestParams): CompletableFuture[TestResult] =
      unsupported("buildTarget/test")
    override def debugSessionStart(
        params: DebugSessionParams
    ): CompletableFuture[DebugSessionAddress] = unsupported("debugSession/start")
    override def buildTargetCleanCache(
        params: CleanCacheParams
    ): CompletableFuture[CleanCacheResult] = unsupported("buildTarget/cleanCache")
    override def onRunReadStdin(params: ReadParams): Unit = ()
    override def buildTargetScalaTestClasses(
        params: ScalaTestClassesParams
    ): CompletableFuture[ScalaTestClassesResult] = unsupported("buildTarget/scalaTestClasses")
    override def buildTargetScalaMainClasses(
        params: ScalaMainClassesParams
    ): CompletableFuture[ScalaMainClassesResult] = unsupported("buildTarget/scalaMainClasses")

  test("bsp4j 2.2.0-M2 Launcher works on lsp4j-jsonrpc 1.0.0 and feeds BspSection.gather"):
    // jsonrpc version actually resolved on this classpath
    val jsonrpcJar = classOf[Launcher[?]].getProtectionDomain.getCodeSource.getLocation.toString
    assert(
      jsonrpcJar.contains("1.0.0"),
      s"expected the evicted lsp4j-jsonrpc 1.0.0 on the classpath, got $jsonrpcJar"
    )

    val clientToServer = new PipedInputStream(64 * 1024)
    val clientOut = new PipedOutputStream(clientToServer)
    val serverToClient = new PipedInputStream(64 * 1024)
    val serverOut = new PipedOutputStream(serverToClient)

    val server = new MiniBuildServer
    val executor = Executors.newCachedThreadPool()
    val serverLauncher: Launcher[BuildClient] = new Launcher.Builder[BuildClient]()
      .setLocalService(server)
      .setRemoteInterface(classOf[BuildClient])
      .setInput(clientToServer)
      .setOutput(serverOut)
      .setExecutorService(executor)
      .create()
    server.client = serverLauncher.getRemoteProxy
    val listening = serverLauncher.startListening()

    val workspaceRoot = DoctorTestSupport.tempRoot("bsp-compat")
    val session = BspSession.connect(workspaceRoot, serverToClient, clientOut)
    try
      val initResult = session.initialize()
      assertEquals(initResult.getDisplayName, "mini-bsp")
      assertEquals(initResult.getVersion, "1.2.3")

      val model = ProjectModelLoader.load(session)
      assertEquals(model.targets.map(_.bspId).sorted, Vector("bsp://mini/app", "bsp://mini/lib"))
      assert(session.compile(Vector("bsp://mini/app")).isOk)

      BspSection.gather(model, Some(initResult)) match
        case SectionState.Unavailable(reason) => fail(s"unexpectedly unavailable: $reason")
        case SectionState.Ready(section) =>
          assertEquals(section.serverName, Some("mini-bsp"))
          assertEquals(section.serverVersion, Some("1.2.3"))
          assertEquals(section.targetCount, 2)
          assertEquals(section.scala3Targets, Vector("bsp://mini/app", "bsp://mini/lib"))
          // no SemanticDB options -> both targets are IndexUnavailable
          assertEquals(section.indexUnavailableTargets, Vector("bsp://mini/app", "bsp://mini/lib"))
    finally
      session.close()
      listening.cancel(true)
      executor.shutdownNow()
      ()
