package ls.pc

import java.nio.file.{Files, Path, Paths}
import java.util.concurrent.{Executors, ScheduledExecutorService, ThreadFactory}

import scala.jdk.CollectionConverters.*
import scala.util.control.NonFatal

/** PC configuration for one BSP build target.
  *
  * @param bspId         BSP build target identifier (URI string)
  * @param classpath     full compile classpath of the target
  * @param scalacOptions scalac options reported by BSP for the target
  * @param sourceDirs    source roots visible to the PC (outline/source path)
  * @param scalaVersion  Scala version of the target (informational, exposed
  *                      to plugin hooks via [[PcTargetContext]])
  */
final case class PcTargetConfig(
    bspId: String,
    classpath: Vector[Path],
    scalacOptions: Vector[String],
    sourceDirs: Vector[Path] = Vector.empty,
    scalaVersion: String = "3.8.4"
)

/** Tunables for the PC layer. */
final case class PcSettings(
    workspaceRoot: Option[Path],
    /** Where plugin synthetic sources are materialized
      * (`.scala3-bsp-semantic-ls/pc/generated-sources` inside a workspace).
      */
    generatedSourcesRoot: Path,
    /** Live PC instance cap; least-recently-used instances beyond it are shut down. */
    maxLiveInstances: Int = 4,
    /** Synchronous request budget; a request over budget wedges the instance,
      * which is shut down and lazily recreated.
      */
    requestTimeoutMillis: Long = 15000
)

object PcSettings:
  /** Settings rooted in a workspace, following the plan 5.3 storage layout. */
  def forWorkspace(root: Path): PcSettings =
    PcSettings(
      workspaceRoot = Some(root),
      generatedSourcesRoot =
        root.resolve(".scala3-bsp-semantic-ls").resolve("pc").resolve("generated-sources")
    )

  /** Workspace-less settings (tests, ad-hoc embedding): synthetic sources go
    * to a fresh temp directory.
    */
  def ephemeral(): PcSettings =
    PcSettings(None, Files.createTempDirectory("ls-pc-generated-sources"))

/** Per-target presentation compiler lifecycle (plan Phase 8).
  *
  * - `getOrCreate` builds a [[PcInstance]] on first use, applying plugin
  *   patching first: compiler-plugin config options are appended, service
  *   plugins run `patchOptions`/`patchSourcePath`/`syntheticSources`, and
  *   synthetic sources are materialized under the generated-sources root.
  * - Live instances are capped ([[PcSettings.maxLiveInstances]]) with LRU
  *   eviction; evicted instances are shut down.
  * - A timed-out (wedged) request shuts the instance down via [[invalidate]];
  *   the next request for that target lazily recreates it.
  *
  * Thread-safe.
  */
final class PcWorkerManager(
    val pluginManager: PcPluginManager,
    val settings: PcSettings,
    /** Cross-file definition lookup plugged into the PC's `SymbolSearch`
      * seam; defaults to a no-op so embedders without an index are unchanged.
      */
    resolver: PcDefinitionResolver = PcDefinitionResolver.Empty
):

  private final case class Entry(
      config: PcTargetConfig,
      instance: PcInstance,
      syntheticFiles: Vector[(Path, Boolean)] // (path, sticky)
  )

  private val lock = new Object
  // LRU: access-ordered LinkedHashMap, eldest evicted when over cap.
  private val live = new java.util.LinkedHashMap[String, Entry](16, 0.75f, true)
  @volatile private var closed = false

  private def daemonFactory(name: String): ThreadFactory = r =>
    val t = new Thread(r, name)
    t.setDaemon(true)
    t

  private val executor = Executors.newCachedThreadPool(daemonFactory("ls-pc-worker"))
  private val scheduler: ScheduledExecutorService =
    Executors.newSingleThreadScheduledExecutor(daemonFactory("ls-pc-scheduler"))

  /** [[IndexBackedSymbolSearch]] per distinct classpath: its lazy classpath
    * index is expensive to build, so it must survive LRU eviction / timeout
    * re-creation of PC instances and be shared by targets with an identical
    * classpath.
    */
  private val symbolSearches =
    new java.util.concurrent.ConcurrentHashMap[Vector[Path], IndexBackedSymbolSearch]

  private def symbolSearchFor(classpath: Vector[Path]): IndexBackedSymbolSearch =
    symbolSearches.computeIfAbsent(classpath, cp => new IndexBackedSymbolSearch(resolver, cp))

  /** Get the live instance for `config.bspId`, creating (or recreating, if the
    * config changed) it as needed. May evict + shut down the least recently
    * used instance when over the cap.
    */
  def getOrCreate(config: PcTargetConfig): PcInstance =
    require(!closed, "PcWorkerManager is shut down")
    val (result, toDispose) = lock.synchronized {
      val cached = Option(live.get(config.bspId))
      cached match
        case Some(entry) if entry.config == config =>
          (entry.instance, Vector.empty[Entry])
        case other =>
          val stale = other.toVector
          stale.foreach(_ => live.remove(config.bspId))
          val entry = createEntry(config)
          live.put(config.bspId, entry)
          val evicted = Vector.newBuilder[Entry]
          while live.size > math.max(1, settings.maxLiveInstances) do
            val it = live.entrySet().iterator()
            val eldest = it.next()
            evicted += eldest.getValue
            it.remove()
          (entry.instance, stale ++ evicted.result())
    }
    toDispose.foreach(dispose)
    result

  /** Run `f` against the target's instance; a timed-out request shuts the
    * instance down so the next request recreates it.
    */
  def run[A](config: PcTargetConfig)(f: PcInstance => A): A =
    val instance = getOrCreate(config)
    try f(instance)
    catch
      case t: PcTimeoutException =>
        invalidate(config.bspId)
        throw t

  /** Shut down and drop the instance for `bspId` (if any); it is lazily
    * recreated on the next request. Returns true when an instance existed.
    */
  def invalidate(bspId: String): Boolean =
    val removed = lock.synchronized(Option(live.remove(bspId)))
    removed.foreach(dispose)
    removed.isDefined

  /** Restart a target's PC: dispose the current instance; the next request
    * recreates it with freshly patched options and synthetic sources.
    */
  def restartTarget(bspId: String): Boolean = invalidate(bspId)

  /** Targets with a live PC instance, least recently used first. */
  def activeTargets: Vector[String] =
    lock.synchronized(live.keySet().asScala.toVector)

  def shutdownAll(): Unit =
    closed = true
    val entries = lock.synchronized {
      val all = live.values().asScala.toVector
      live.clear()
      all
    }
    entries.foreach(dispose)
    executor.shutdown()
    scheduler.shutdown()

  // --- creation --------------------------------------------------------------

  private def createEntry(config: PcTargetConfig): Entry =
    val ctx = PcTargetContext(
      bspId = config.bspId,
      scalaVersion = config.scalaVersion,
      classpath = config.classpath,
      workspaceRoot = settings.workspaceRoot
    )
    // 1. compiler-plugin config contributes -Xplugin/-P args (PC only, never the build)
    val withCompilerPlugins = config.scalacOptions ++ pluginManager.compilerPluginOptions
    // 2. service plugins patch options
    val options = pluginManager.patchOptions(ctx, withCompilerPlugins)
    // 3. synthetic sources are materialized under the generated-sources root
    val synthetic = pluginManager.syntheticSources(ctx)
    val targetDir = settings.generatedSourcesRoot.resolve(sanitize(config.bspId))
    val written: Vector[(Path, Boolean)] = synthetic.flatMap { vs =>
      try
        val target = targetDir.resolve(vs.path).normalize()
        require(target.startsWith(targetDir), s"synthetic source escapes generated-sources dir: ${vs.path}")
        Files.createDirectories(target.getParent)
        Files.writeString(target, vs.content)
        Some((target, vs.sticky))
      catch case NonFatal(_) => None
    }
    // 4. service plugins patch the source path
    val baseSourcePath =
      config.sourceDirs ++ (if written.nonEmpty then Vector(targetDir) else Vector.empty)
    val sourcePath = pluginManager.patchSourcePath(ctx, baseSourcePath)
    val sourcePathSupplier: java.util.function.Supplier[java.util.List[Path]] =
      () => sourcePath.asJava

    var base: scala.meta.pc.PresentationCompiler = new dotty.tools.pc.ScalaPresentationCompiler()
      .withExecutorService(executor)
      .withScheduledExecutorService(scheduler)
      // SymbolSearch seam: cross-file go-to routes to the resolver, scope-mode
      // classpath completion runs over this target's classpath, member-mode
      // workspace extension-method discovery routes to resolver.searchMethods.
      .withSearch(symbolSearchFor(config.classpath))
    settings.workspaceRoot.foreach(root => base = base.withWorkspace(root))
    val underlying =
      base.newInstance(config.bspId, config.classpath.asJava, options.asJava, sourcePathSupplier)

    val instance = new PcInstance(
      targetId = config.bspId,
      effectiveOptions = options,
      effectiveSourcePath = sourcePath,
      syntheticUris = written.map(_._1.toUri.toString).toSet,
      underlying = underlying,
      timeoutMillis = settings.requestTimeoutMillis
    )
    Entry(config, instance, written)

  private def dispose(entry: Entry): Unit =
    try entry.instance.shutdown()
    catch case NonFatal(_) => ()
    entry.syntheticFiles.foreach { case (path, sticky) =>
      if !sticky then
        try Files.deleteIfExists(path)
        catch case NonFatal(_) => ()
    }

  private def sanitize(bspId: String): String =
    bspId.map(c => if c.isLetterOrDigit || c == '.' || c == '-' || c == '_' then c else '_')
