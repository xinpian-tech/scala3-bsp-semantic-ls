package ls.pc

import java.io.File
import java.nio.file.{Files, Path, Paths}
import java.util.concurrent.atomic.{AtomicBoolean, AtomicInteger}

import scala.jdk.CollectionConverters.*

import org.eclipse.lsp4j.{CompletionItem, CompletionList, Diagnostic, Location, Position, Range}

/** Test plugins registered on the shared facade. */
object TestPlugins:

  /** Appends a marker completion item; proves afterCompletion runs. */
  final class MarkerCompletionPlugin extends PcServicePlugin:
    val marker = "ls-pc-marker-item"
    def id: String = "marker-completion"
    override def afterCompletion(req: PcRequest, result: CompletionList): CompletionList =
      val items = new java.util.ArrayList[CompletionItem](result.getItems)
      val extra = new CompletionItem(marker)
      items.add(extra)
      new CompletionList(result.isIncomplete, items)

  /** Adds a harmless compiler flag; proves patchOptions reaches the PC. */
  final class FlagOptionsPlugin extends PcServicePlugin:
    val flag = "-deprecation"
    def id: String = "flag-options"
    override def patchOptions(ctx: PcTargetContext, options: Vector[String]): Vector[String] =
      if options.contains(flag) then options else options :+ flag

  /** Throws in afterCompletion; must be disabled without failing the request. */
  final class ThrowingPlugin extends PcServicePlugin:
    def id: String = "throwing-plugin"
    override def afterCompletion(req: PcRequest, result: CompletionList): CompletionList =
      throw new RuntimeException("boom: intentional test crash")

  /** Contributes a synthetic source; proves materialization + source path. */
  final class SyntheticSourcePlugin extends PcServicePlugin:
    def id: String = "synthetic-source"
    override def syntheticSources(ctx: PcTargetContext): Vector[VirtualSource] =
      Vector(VirtualSource("Gen.scala", "object LsPcGenerated:\n  val fromPlugin = 42\n"))

  /** Appends one synthetic-uri and one foreign location to every definition
    * result, both claiming Workspace origin: the facade must re-mark them.
    */
  final class DefinitionAugmentPlugin(syntheticUri: String, foreignUri: String) extends PcServicePlugin:
    def id: String = "definition-augment"
    override def afterDefinition(req: PcRequest, result: DefinitionResult): DefinitionResult =
      val zero = new Range(new Position(0, 0), new Position(0, 3))
      result.copy(locations =
        result.locations ++ Vector(
          DefinitionLocation(new Location(syntheticUri, zero), DefinitionOrigin.Workspace),
          DefinitionLocation(new Location(foreignUri, zero), DefinitionOrigin.Workspace)
        )
      )

  /** Records filterPcDiagnostics invocations; returns diagnostics unchanged. */
  final class DiagnosticsProbePlugin extends PcServicePlugin:
    val invocations = new AtomicInteger(0)
    @volatile var lastSeen: Vector[Diagnostic] = Vector.empty
    def id: String = "diagnostics-probe"
    override def filterPcDiagnostics(req: PcRequest, diagnostics: Vector[Diagnostic]): Vector[Diagnostic] =
      invocations.incrementAndGet()
      lastSeen = diagnostics
      diagnostics

/** Loaded via ServiceLoader in ServiceLoaderSuite (the provider-configuration
  * file lives in a jar built at test runtime; the class itself is resolved
  * through the parent loader).
  */
final class ServiceLoadedTestPlugin extends PcServicePlugin:
  def id: String = "service-loaded-test"

/** One shared PC facade for all suites: PC instance creation and first-compile
  * are expensive, so every suite reuses this instance instead of making its own.
  */
object SharedPc:

  /** scala library jars extracted from this test JVM's own classpath. */
  lazy val libraryClasspath: Vector[Path] =
    val entries = System
      .getProperty("java.class.path", "")
      .split(File.pathSeparatorChar)
      .toVector
    val jars = entries.filter { e =>
      val name = Paths.get(e).getFileName.toString
      name.endsWith(".jar") && (name.startsWith("scala-library") || name.startsWith("scala3-library"))
    }
    assert(jars.nonEmpty, s"no scala library jar on test classpath: $entries")
    jars.map(Paths.get(_))

  val targetId = "testTarget"
  lazy val generatedSourcesRoot: Path = Files.createTempDirectory("ls-pc-test-gen")

  lazy val syntheticGenUri: String =
    generatedSourcesRoot.resolve(targetId).resolve("Gen.scala").toUri.toString
  val foreignUri = "file:///ls-pc-test/elsewhere/Other.scala"

  lazy val marker = new TestPlugins.MarkerCompletionPlugin
  lazy val flag = new TestPlugins.FlagOptionsPlugin
  lazy val thrower = new TestPlugins.ThrowingPlugin
  lazy val diagProbe = new TestPlugins.DiagnosticsProbePlugin

  lazy val pluginManager: PcPluginManager =
    val pm = new PcPluginManager(PcPluginInitContext(None, generatedSourcesRoot))
    pm.register(marker)
    pm.register(flag)
    pm.register(new TestPlugins.SyntheticSourcePlugin)
    pm.register(new TestPlugins.DefinitionAugmentPlugin(syntheticGenUri, foreignUri))
    pm.register(diagProbe)
    pm.register(thrower) // last: earlier hooks' contributions survive its crash
    pm

  def targetConfig: PcTargetConfig =
    PcTargetConfig(targetId, libraryClasspath, Vector.empty)

  lazy val facade: PcFacade =
    val f = new PcFacade(
      pluginManager,
      PcSettings(
        workspaceRoot = None,
        generatedSourcesRoot = generatedSourcesRoot,
        maxLiveInstances = 4,
        requestTimeoutMillis = 90000
      )
    )
    f.registerTarget(targetConfig)
    f

  private val uriCounter = new AtomicInteger(0)

  /** Open a fresh dirty buffer on the shared facade and return its uri. */
  def openBuffer(text: String): String =
    val uri = s"file:///ls-pc-test/Buffer${uriCounter.incrementAndGet()}.scala"
    facade.didOpen(targetId, uri, text)
    uri

  def labels(list: CompletionList): Vector[String] =
    list.getItems.asScala.toVector.map(_.getLabel)
