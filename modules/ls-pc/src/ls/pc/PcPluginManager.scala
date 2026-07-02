package ls.pc

import java.net.URLClassLoader
import java.nio.file.{Files, Path}
import java.util.ServiceLoader

import scala.util.control.NonFatal

import org.eclipse.lsp4j.{CompletionList, Diagnostic, Hover}

/** Load/self-test status of one configured PC compiler plugin spec. */
final case class CompilerPluginStatus(
    jars: Vector[String],
    options: Vector[String],
    loaded: Boolean,
    detail: String
)

/** Load/self-test status of one PC service plugin. */
final case class ServicePluginStatus(
    id: String,
    source: String,
    enabled: Boolean,
    selfTestOk: Boolean,
    selfTestDetail: String
)

/** A disabled plugin plus the reason it was disabled. */
final case class DisabledPlugin(id: String, reason: String)

/** Doctor-facing PC plugin report (plan 19: compiler plugins loaded, service
  * plugins loaded, self-test results, disabled plugins).
  */
final case class PcPluginStatusReport(
    compilerPlugins: Vector[CompilerPluginStatus],
    servicePlugins: Vector[ServicePluginStatus],
    disabled: Vector[DisabledPlugin]
)

/** Owns PC service plugins and the PC compiler-plugin configuration.
  *
  * Service plugins come from `java.util.ServiceLoader` over an isolated
  * `URLClassLoader` on user-configured jars, or from programmatic
  * [[register]] (used by tests and embedders). Every plugin is self-tested at
  * load: `id` must be non-empty and unique, and `initialize` must not throw.
  *
  * Every hook invocation is guarded: a hook that throws disables the plugin
  * (recording the throwable) and the request proceeds as if that hook were
  * identity — a plugin crash never fails a PC request (plan Phase 9:
  * "插件崩溃不影响主 LS"). Disabled plugins are listed in [[statusReport]].
  *
  * Thread-safe: registration and hook dispatch may happen concurrently.
  */
final class PcPluginManager(initContext: PcPluginInitContext):

  private final class Registered(val plugin: PcServicePlugin, val pluginId: String, val source: String):
    @volatile var disabledReason: Option[String] = None
    @volatile var disabledCause: Option[Throwable] = None
    @volatile var selfTestOk: Boolean = true
    @volatile var selfTestDetail: String = "ok"
    def enabled: Boolean = disabledReason.isEmpty

  private val lock = new Object
  private var registered = Vector.empty[Registered]
  /** Plugins that could not even be registered (bad id, load error, missing jar). */
  private var loadFailures = Vector.empty[ServicePluginStatus]
  private var compilerStatuses = Vector.empty[CompilerPluginStatus]
  private var compilerOptions = Vector.empty[String]

  // --- loading -------------------------------------------------------------

  /** Programmatic registration; runs the load self-test immediately. */
  def register(plugin: PcServicePlugin): Unit = registerInternal(plugin, "programmatic")

  /** Load service plugins via ServiceLoader from an isolated URLClassLoader
    * over `jars`. A missing jar or a broken provider is recorded as a load
    * failure, never thrown.
    */
  def loadServicePluginJars(jars: Vector[Path]): Unit =
    val (present, missing) = jars.partition(Files.exists(_))
    lock.synchronized:
      loadFailures = loadFailures ++ missing.map { jar =>
        ServicePluginStatus(
          id = jar.toString,
          source = jar.toString,
          enabled = false,
          selfTestOk = false,
          selfTestDetail = s"service plugin jar does not exist: $jar"
        )
      }
    if present.nonEmpty then
      try
        val loader =
          new URLClassLoader(present.map(_.toUri.toURL).toArray, classOf[PcServicePlugin].getClassLoader)
        val source = present.mkString(java.io.File.pathSeparator)
        ServiceLoader
          .load(classOf[PcServicePlugin], loader)
          .stream()
          .forEach { provider =>
            try registerInternal(provider.get(), source)
            catch
              case NonFatal(t) =>
                lock.synchronized:
                  loadFailures = loadFailures :+ ServicePluginStatus(
                    id = provider.`type`().getName,
                    source = source,
                    enabled = false,
                    selfTestOk = false,
                    selfTestDetail = s"provider failed to instantiate: ${describe(t)}"
                  )
          }
      catch
        case NonFatal(t) =>
          lock.synchronized:
            loadFailures = loadFailures :+ ServicePluginStatus(
              id = present.mkString(java.io.File.pathSeparator),
              source = "service-loader",
              enabled = false,
              selfTestOk = false,
              selfTestDetail = s"service loading failed: ${describe(t)}"
            )

  /** Apply the compiler-plugin configuration. Jar existence is validated
    * here: specs with missing jars are recorded as failed self-tests and
    * contribute no options; valid specs contribute `-Xplugin:`/`-P:` args to
    * [[compilerPluginOptions]] (appended to PC options only, never the build).
    */
  def setCompilerPluginConfig(config: PcCompilerPluginConfig): Unit =
    val statuses = config.plugins.map { spec =>
      val missing = spec.jars.filterNot(Files.exists(_))
      if spec.jars.isEmpty then
        CompilerPluginStatus(Vector.empty, spec.options, loaded = false, "self-test failed: no jars configured")
      else if missing.isEmpty then
        CompilerPluginStatus(spec.jars.map(_.toString), spec.toOptions, loaded = true, "ok")
      else
        CompilerPluginStatus(
          spec.jars.map(_.toString),
          spec.toOptions,
          loaded = false,
          s"self-test failed: missing jar(s): ${missing.mkString(", ")}"
        )
    }
    val options = config.plugins
      .filter(spec => spec.jars.nonEmpty && spec.jars.forall(Files.exists(_)))
      .flatMap(_.toOptions)
    lock.synchronized:
      compilerStatuses = statuses
      compilerOptions = options

  /** Load a full `pc-plugins.json` config: compiler plugins + service jars. */
  def applyConfig(config: PcPluginConfig): Unit =
    setCompilerPluginConfig(config.compilerPlugins)
    loadServicePluginJars(config.servicePluginJars)

  /** `-Xplugin:`/`-P:` arguments contributed by validated compiler-plugin
    * specs. [[PcWorkerManager]] appends these to each target's PC options.
    */
  def compilerPluginOptions: Vector[String] = lock.synchronized(compilerOptions)

  private def registerInternal(plugin: PcServicePlugin, source: String): Unit =
    val pluginId =
      try Option(plugin.id).getOrElse("")
      catch case NonFatal(_) => ""
    val rejection: Option[ServicePluginStatus] = lock.synchronized:
      if pluginId.isEmpty then
        Some(
          ServicePluginStatus(
            id = plugin.getClass.getName,
            source = source,
            enabled = false,
            selfTestOk = false,
            selfTestDetail = "self-test failed: plugin id is null or empty"
          )
        )
      else if registered.exists(_.pluginId == pluginId) then
        Some(
          ServicePluginStatus(
            id = pluginId,
            source = source,
            enabled = false,
            selfTestOk = false,
            selfTestDetail = s"self-test failed: duplicate plugin id '$pluginId'"
          )
        )
      else None
    rejection match
      case Some(failure) =>
        lock.synchronized:
          loadFailures = loadFailures :+ failure
      case None =>
        val reg = new Registered(plugin, pluginId, source)
        try plugin.initialize(initContext)
        catch
          case NonFatal(t) =>
            reg.selfTestOk = false
            reg.selfTestDetail = s"self-test failed: initialize threw ${describe(t)}"
            reg.disabledReason = Some(reg.selfTestDetail)
            reg.disabledCause = Some(t)
        lock.synchronized:
          registered = registered :+ reg

  // --- hook pipeline --------------------------------------------------------

  def patchOptions(ctx: PcTargetContext, options: Vector[String]): Vector[String] =
    foldHooks("patchOptions", options)((p, acc) => p.patchOptions(ctx, acc))

  def patchSourcePath(ctx: PcTargetContext, sourcePath: Vector[Path]): Vector[Path] =
    foldHooks("patchSourcePath", sourcePath)((p, acc) => p.patchSourcePath(ctx, acc))

  def syntheticSources(ctx: PcTargetContext): Vector[VirtualSource] =
    foldHooks("syntheticSources", Vector.empty[VirtualSource]) { (p, acc) =>
      acc ++ p.syntheticSources(ctx)
    }

  def beforeRequest(req: PcRequest): PcRequest =
    foldHooks("beforeRequest", req)((p, acc) => p.beforeRequest(acc))

  def afterCompletion(req: PcRequest, result: CompletionList): CompletionList =
    foldHooks("afterCompletion", result)((p, acc) => p.afterCompletion(req, acc))

  def afterHover(req: PcRequest, result: Option[Hover]): Option[Hover] =
    foldHooks("afterHover", result)((p, acc) => p.afterHover(req, acc))

  def afterDefinition(req: PcRequest, result: DefinitionResult): DefinitionResult =
    foldHooks("afterDefinition", result)((p, acc) => p.afterDefinition(req, acc))

  def filterPcDiagnostics(req: PcRequest, diagnostics: Vector[Diagnostic]): Vector[Diagnostic] =
    foldHooks("filterPcDiagnostics", diagnostics)((p, acc) => p.filterPcDiagnostics(req, acc))

  private def foldHooks[A](hook: String, initial: A)(f: (PcServicePlugin, A) => A): A =
    val plugins = lock.synchronized(registered)
    plugins.foldLeft(initial) { (acc, reg) =>
      if !reg.enabled then acc
      else
        try
          val out = f(reg.plugin, acc)
          if out == null then acc else out
        catch
          case NonFatal(t) =>
            disable(reg, hook, t)
            acc
    }

  private def disable(reg: Registered, hook: String, t: Throwable): Unit =
    reg.disabledReason = Some(s"hook '$hook' threw ${describe(t)}")
    reg.disabledCause = Some(t)

  private def describe(t: Throwable): String =
    val msg = Option(t.getMessage).getOrElse("")
    if msg.isEmpty then t.getClass.getName else s"${t.getClass.getName}: $msg"

  // --- reporting ------------------------------------------------------------

  /** Ids of plugins currently enabled for hook dispatch. */
  def enabledPluginIds: Vector[String] =
    lock.synchronized(registered).filter(_.enabled).map(_.pluginId)

  /** The recorded throwable that disabled `pluginId`, if any. */
  def disabledCause(pluginId: String): Option[Throwable] =
    lock.synchronized(registered).find(_.pluginId == pluginId).flatMap(_.disabledCause)

  def statusReport: PcPluginStatusReport =
    val (regs, failures, compilers) =
      lock.synchronized((registered, loadFailures, compilerStatuses))
    val serviceStatuses = regs.map { reg =>
      ServicePluginStatus(
        id = reg.pluginId,
        source = reg.source,
        enabled = reg.enabled,
        selfTestOk = reg.selfTestOk,
        selfTestDetail = reg.selfTestDetail
      )
    } ++ failures
    val disabled =
      regs.collect {
        case reg if reg.disabledReason.isDefined => DisabledPlugin(reg.pluginId, reg.disabledReason.get)
      } ++ failures.map(f => DisabledPlugin(f.id, f.selfTestDetail)) ++
        compilers.collect {
          case c if !c.loaded => DisabledPlugin(c.jars.mkString(","), c.detail)
        }
    PcPluginStatusReport(compilers, serviceStatuses, disabled)
