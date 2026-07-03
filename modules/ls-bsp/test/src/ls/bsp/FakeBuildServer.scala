package ls.bsp

import java.nio.file.Path
import java.util.concurrent.CompletableFuture
import java.util.concurrent.atomic.AtomicBoolean
import java.util.concurrent.atomic.AtomicInteger

import scala.jdk.CollectionConverters.*

import ch.epfl.scala.bsp4j.*

/** In-process BSP server with canned data, served over the same lsp4j
  * jsonrpc Launcher machinery as a real server. Three Scala 3 targets with
  * a <- b <- c dependencies (b depends on a, c on b):
  *
  *   - a: DIRECTORY source item, `-Xsemanticdb` + `-semanticdb-target:`
  *     override (colon form) + two-token `-sourceroot` form
  *   - b: FILE source item, plain `-Ysemanticdb` (targetroot = classDirectory)
  *   - c: FILE source item, no SemanticDB flags at all
  *
  * plus a Scala 2 target and a Java-only target that the loader must filter
  * out. Compiling the pseudo target `broken` returns StatusCode.ERROR.
  */
final class FakeBuildServer(
    workspaceRoot: Path,
    aSourceDir: Path,
    bSourceFile: Path,
    cSourceFile: Path,
    val semanticdbOverride: Path,
    advertiseInverseSources: Boolean,
    advertiseDependencySources: Boolean = false,
    advertiseOutputPaths: Boolean = false
) extends BuildServer
    with ScalaBuildServer:

  @volatile var client: BuildClient = scala.compiletime.uninitialized

  val initializeReceived = new AtomicBoolean(false)
  val initializedNotified = new AtomicBoolean(false)
  val shutdownRequested = new AtomicBoolean(false)
  val exitReceived = new AtomicBoolean(false)
  val inverseSourcesCalls = new AtomicInteger(0)
  val workspaceBuildTargetsCalls = new AtomicInteger(0)
  val dependencySourcesCalls = new AtomicInteger(0)
  val outputPathsCalls = new AtomicInteger(0)

  def idOf(name: String): String = s"bsp://workspace/$name"
  val brokenId: String = idOf("broken")

  def classDirectoryOf(name: String): Path =
    workspaceRoot.resolve("out").resolve(name).resolve("classes")

  private def targetId(name: String) = new BuildTargetIdentifier(idOf(name))

  private def buildTarget(
      name: String,
      languages: List[String],
      deps: List[String],
      data: Object
  ): BuildTarget =
    val caps = new BuildTargetCapabilities()
    caps.setCanCompile(true)
    val t = new BuildTarget(
      targetId(name),
      java.util.List.of(),
      languages.asJava,
      deps.map(targetId).asJava,
      caps
    )
    t.setDisplayName(name)
    if data != null then
      t.setDataKind(BuildTargetDataKind.SCALA)
      t.setData(data)
    t

  private def scala3Data: ScalaBuildTarget =
    new ScalaBuildTarget("org.scala-lang", "3.8.4", "3", ScalaPlatform.JVM, java.util.List.of())

  private def scala2Data: ScalaBuildTarget =
    new ScalaBuildTarget("org.scala-lang", "2.13.16", "2.13", ScalaPlatform.JVM, java.util.List.of())

  // --- core BSP ---

  override def buildInitialize(
      params: InitializeBuildParams
  ): CompletableFuture[InitializeBuildResult] =
    initializeReceived.set(true)
    val caps = new BuildServerCapabilities()
    caps.setCompileProvider(new CompileProvider(java.util.List.of("scala")))
    caps.setInverseSourcesProvider(advertiseInverseSources)
    caps.setDependencySourcesProvider(advertiseDependencySources)
    caps.setOutputPathsProvider(advertiseOutputPaths)
    CompletableFuture.completedFuture(
      new InitializeBuildResult("fake-bsp-server", "0.0.1", Bsp4j.PROTOCOL_VERSION, caps)
    )

  override def onBuildInitialized(): Unit = initializedNotified.set(true)

  override def buildShutdown(): CompletableFuture[Object] =
    shutdownRequested.set(true)
    CompletableFuture.completedFuture(new Object)

  override def onBuildExit(): Unit = exitReceived.set(true)

  override def workspaceBuildTargets(): CompletableFuture[WorkspaceBuildTargetsResult] =
    workspaceBuildTargetsCalls.incrementAndGet()
    val targets = java.util.List.of(
      buildTarget("a", List("scala"), Nil, scala3Data),
      buildTarget("b", List("scala"), List("a"), scala3Data),
      buildTarget("c", List("scala"), List("b"), scala3Data),
      buildTarget("scala2", List("scala"), Nil, scala2Data),
      buildTarget("java-only", List("java"), Nil, null)
    )
    CompletableFuture.completedFuture(new WorkspaceBuildTargetsResult(targets))

  override def buildTargetSources(params: SourcesParams): CompletableFuture[SourcesResult] =
    def item(name: String) = name match
      case "a" =>
        new SourcesItem(
          targetId("a"),
          java.util.List.of(
            new SourceItem(aSourceDir.toUri.toString, SourceItemKind.DIRECTORY, false)
          )
        )
      case "b" =>
        new SourcesItem(
          targetId("b"),
          java.util.List.of(new SourceItem(bSourceFile.toUri.toString, SourceItemKind.FILE, false))
        )
      case "c" =>
        new SourcesItem(
          targetId("c"),
          java.util.List.of(new SourceItem(cSourceFile.toUri.toString, SourceItemKind.FILE, false))
        )
      case other => new SourcesItem(targetId(other), java.util.List.of())
    val items = params.getTargets.asScala.map(id => item(id.getUri.stripPrefix("bsp://workspace/")))
    CompletableFuture.completedFuture(new SourcesResult(items.asJava))

  override def buildTargetInverseSources(
      params: InverseSourcesParams
  ): CompletableFuture[InverseSourcesResult] =
    inverseSourcesCalls.incrementAndGet()
    if !advertiseInverseSources then
      CompletableFuture.failedFuture(
        new UnsupportedOperationException("inverseSources capability not advertised")
      )
    else
      val uri = params.getTextDocument.getUri
      val path = Path.of(java.net.URI.create(uri))
      val owner =
        if path.startsWith(aSourceDir) then Some("a")
        else if path == bSourceFile then Some("b")
        else if path == cSourceFile then Some("c")
        else None
      CompletableFuture.completedFuture(
        new InverseSourcesResult(owner.map(targetId).toList.asJava)
      )

  override def buildTargetCompile(params: CompileParams): CompletableFuture[CompileResult] =
    val requested = params.getTargets.asScala.map(_.getUri).toVector
    if requested.contains(brokenId) then
      val result = new CompileResult(StatusCode.ERROR)
      result.setOriginId(params.getOriginId)
      CompletableFuture.completedFuture(result)
    else
      // Notifications flow to the client while the request is in flight.
      val diagnostic = new Diagnostic(
        new Range(new Position(0, 1), new Position(0, 5)),
        "value unused in fake target"
      )
      diagnostic.setSeverity(DiagnosticSeverity.WARNING)
      val publish = new PublishDiagnosticsParams(
        new TextDocumentIdentifier(bSourceFile.toUri.toString),
        params.getTargets.get(0),
        java.util.List.of(diagnostic),
        true
      )
      publish.setOriginId(params.getOriginId)
      client.onBuildPublishDiagnostics(publish)
      client.onBuildLogMessage(new LogMessageParams(MessageType.INFO, "fake compile log"))
      client.onBuildShowMessage(new ShowMessageParams(MessageType.WARNING, "fake compile show"))
      client.onBuildTargetDidChange(
        new DidChangeBuildTarget(java.util.List.of(new BuildTargetEvent(params.getTargets.get(0))))
      )
      val result = new CompileResult(StatusCode.OK)
      result.setOriginId(params.getOriginId)
      CompletableFuture.completedFuture(result)

  // --- Scala extension ---

  override def buildTargetScalacOptions(
      params: ScalacOptionsParams
  ): CompletableFuture[ScalacOptionsResult] =
    def item(name: String): ScalacOptionsItem =
      val options: java.util.List[String] = name match
        case "a" =>
          java.util.List.of(
            "-deprecation",
            "-Xsemanticdb",
            s"-semanticdb-target:$semanticdbOverride",
            "-sourceroot",
            workspaceRoot.toString
          )
        case "b" => java.util.List.of("-Ysemanticdb")
        case _ => java.util.List.of("-deprecation")
      new ScalacOptionsItem(
        targetId(name),
        options,
        java.util.List.of(),
        classDirectoryOf(name).toUri.toString
      )
    val items = params.getTargets.asScala.map(id => item(id.getUri.stripPrefix("bsp://workspace/")))
    CompletableFuture.completedFuture(new ScalacOptionsResult(items.asJava))

  // --- endpoints this fake does not serve ---

  private def unsupported[A](method: String): CompletableFuture[A] =
    CompletableFuture.failedFuture(new UnsupportedOperationException(method))

  override def workspaceReload(): CompletableFuture[Object] = unsupported("workspace/reload")
  override def buildTargetDependencySources(
      params: DependencySourcesParams
  ): CompletableFuture[DependencySourcesResult] =
    if !advertiseDependencySources then unsupported("buildTarget/dependencySources")
    else
      dependencySourcesCalls.incrementAndGet()
      val items = params.getTargets.asScala.map { id =>
        new DependencySourcesItem(id, java.util.List.of(s"file:///dep/${id.getUri.stripPrefix("bsp://workspace/")}-sources.jar"))
      }
      CompletableFuture.completedFuture(new DependencySourcesResult(items.asJava))
  override def buildTargetDependencyModules(
      params: DependencyModulesParams
  ): CompletableFuture[DependencyModulesResult] = unsupported("buildTarget/dependencyModules")
  override def buildTargetResources(
      params: ResourcesParams
  ): CompletableFuture[ResourcesResult] = unsupported("buildTarget/resources")
  override def buildTargetOutputPaths(
      params: OutputPathsParams
  ): CompletableFuture[OutputPathsResult] =
    if !advertiseOutputPaths then unsupported("buildTarget/outputPaths")
    else
      outputPathsCalls.incrementAndGet()
      val items = params.getTargets.asScala.map { id =>
        new OutputPathsItem(id, java.util.List.of(new OutputPathItem(s"file:///out/${id.getUri.stripPrefix("bsp://workspace/")}", OutputPathItemKind.DIRECTORY)))
      }
      CompletableFuture.completedFuture(new OutputPathsResult(items.asJava))
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
