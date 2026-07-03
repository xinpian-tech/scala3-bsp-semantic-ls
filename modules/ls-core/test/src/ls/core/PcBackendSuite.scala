package ls.core

import java.io.File
import java.nio.file.{Files, Path, Paths}
import java.util.concurrent.TimeUnit

import scala.concurrent.duration.*
import scala.jdk.CollectionConverters.*

import ls.doctor.PcSection
import ls.pc.{
  ForkedPcWorker,
  PcFacade,
  PcPluginInitContext,
  PcPluginManager,
  PcRequest,
  PcServicePlugin,
  PcSettings,
  PcTargetConfig
}
import org.eclipse.lsp4j.{CompletionItem, CompletionList}

/** The [[PcBackend]] seam and its two implementations.
  *
  *   - [[InProcessPcBackend]] over a facade: completion works through the seam
  *     and a throwing service plugin is contained (disabled + listed, request
  *     still returns).
  *   - [[ForkedPcBackend]] over a real child JVM: completion/hover work, the
  *     doctor reports "forked worker alive", and killing the worker pid respawns
  *     it (the LS stays up). Gated by `LS_PC_SKIP_FORK_TEST` like
  *     `ls.pc.ForkedWorkerSuite`.
  *   - `Bootstrap`/`Main` wiring routes the `--forked-pc` flag to the forked
  *     backend.
  */
class PcBackendSuite extends munit.FunSuite:
  override def munitTimeout: Duration = 5.minutes

  /** scala library jars from this test JVM's classpath, for the PC target. */
  private lazy val libraryClasspath: Vector[Path] =
    val entries = System.getProperty("java.class.path", "").split(File.pathSeparatorChar).toVector
    val jars = entries.filter { e =>
      val n = Paths.get(e).getFileName.toString
      n.endsWith(".jar") && (n.startsWith("scala-library") || n.startsWith("scala3-library"))
    }
    assert(jars.nonEmpty, s"no scala library jar on classpath: $entries")
    jars.map(Paths.get(_))

  private val targetId = "backendTarget"
  private def targetConfig: PcTargetConfig = PcTargetConfig(targetId, libraryClasspath, Vector.empty)

  private def assumeForkAllowed(): Unit =
    assume(!sys.env.contains("LS_PC_SKIP_FORK_TEST"), "LS_PC_SKIP_FORK_TEST set: skipping fork test")

  /** Appends a marker completion item; proves afterCompletion runs. */
  private final class MarkerPlugin extends PcServicePlugin:
    val marker = "ls-core-backend-marker"
    def id: String = "core-marker-plugin"
    override def afterCompletion(req: PcRequest, result: CompletionList): CompletionList =
      val items = new java.util.ArrayList[CompletionItem](result.getItems)
      items.add(new CompletionItem(marker))
      new CompletionList(result.isIncomplete, items)

  /** Throws in afterCompletion; must be disabled without failing the request. */
  private final class ThrowingPlugin extends PcServicePlugin:
    def id: String = "core-throwing-plugin"
    override def afterCompletion(req: PcRequest, result: CompletionList): CompletionList =
      throw new RuntimeException("boom: intentional core-test crash")

  // ---------------------------------------------------------- in-process backend

  test("in-process backend serves completion and contains a throwing plugin"):
    val gen = Files.createTempDirectory("ls-core-pc-inproc-gen")
    val marker = new MarkerPlugin
    val pm = new PcPluginManager(PcPluginInitContext(None, gen))
    pm.register(marker)
    pm.register(new ThrowingPlugin) // last: the marker contribution must survive its crash
    val facade = new PcFacade(
      pm,
      PcSettings(workspaceRoot = None, generatedSourcesRoot = gen, maxLiveInstances = 4, requestTimeoutMillis = 90000L)
    )
    val backend = new InProcessPcBackend(facade)
    try
      assertEquals(backend.workerAlive, None)
      assertEquals(backend.workerPid, None)
      backend.registerTarget(targetConfig)
      val uri = "file:///ls-core-test/Inproc.scala"
      backend.didOpen(targetId, uri, "object Inproc:\n  val xs = List(1)\n  val ys = xs.\n")
      assert(backend.bufferText(uri).isDefined)

      val labels = backend.completion(uri, 2, "  val ys = xs.".length).getItems.asScala.map(_.getLabel)
      assert(labels.exists(_.startsWith("map")), labels.take(20).toString)
      // the marker plugin's contribution survives the throwing plugin's crash
      assert(labels.contains(marker.marker), labels.toString)
      // and the throwing plugin is disabled + listed with its reason
      val disabled = backend.pluginStatus.disabled
      assert(disabled.exists(_.id == "core-throwing-plugin"), disabled.map(_.id).toString)
      assert(disabled.exists(d => d.id == "core-throwing-plugin" && d.reason.contains("afterCompletion")), disabled.toString)
    finally backend.shutdown()

  // ------------------------------------------------------------- forked backend

  test("forked backend: completion works, worker alive, killing the pid respawns it"):
    assumeForkAllowed()
    val gen = Files.createTempDirectory("ls-core-pc-forked-gen")
    val worker = new ForkedPcWorker(
      workerArgs = Vector("--generated-sources", gen.toString, "--timeout-ms", "90000"),
      requestTimeoutMillis = 120000
    )
    val backend = new ForkedPcBackend(worker)
    try
      backend.registerTarget(targetConfig)
      val uri = "file:///ls-core-test/Forked.scala"
      backend.didOpen(targetId, uri, "object Forked:\n  val xs = List(1)\n  val ys = xs.\n")

      def completeLabels(): Vector[String] =
        backend.completion(uri, 2, "  val ys = xs.".length).getItems.asScala.toVector.map(_.getLabel)

      assert(completeLabels().exists(_.startsWith("map")), "initial completion")
      // hover works across the process boundary
      assert(backend.hover(uri, 1, "  val x".length).isDefined, "hover")

      // doctor input: the forked worker reports alive
      assertEquals(backend.workerAlive, Some(true))
      assert(backend.workerPid.isDefined)
      val section = PcSection.gather(backend.activeTargets, backend.registeredTargets, backend.workerAlive)
      assertEquals(section.workerStatus, "forked worker alive")

      // fault injection: obtain the pid via the test hook and kill that process;
      // the LS stays up and the next completion respawns + replays the buffer.
      val pid = backend.workerPid.get
      val handle = ProcessHandle.of(pid)
      assert(handle.isPresent, s"no OS process for pid $pid")
      handle.get.destroyForcibly()
      handle.get.onExit().get(30, TimeUnit.SECONDS)

      assert(completeLabels().exists(_.startsWith("map")), "completion after respawn")
      assertEquals(backend.workerAlive, Some(true))
    finally backend.shutdown()

  // --------------------------------------------------------------- flag wiring

  test("Main resolves the PC backend mode from flags"):
    assertEquals(Main.pcBackendMode(Array("--forked-pc")), PcBackendMode.Forked)
    assertEquals(Main.pcBackendMode(Array("--in-process-pc")), PcBackendMode.InProcess)
    // process isolation is now the production default (flipped once the forked
    // worker-kill end-to-end over real Mill BSP was green)
    assertEquals(Main.pcBackendMode(Array.empty[String]), PcBackendMode.Forked)
    assertEquals(Main.pcBackendMode(Array("--forked-pc", "--in-process-pc")), PcBackendMode.Forked)

  test("Bootstrap with Forked mode wires a forked backend (no BSP, no child spawn)"):
    val ws = Files.createTempDirectory("ls-core-forked-bootstrap")
    val docs = new DocumentStore
    val overlay = new PcOverlay(docs)
    val cfg = Bootstrap.Config(connectBsp = (_, _) => None, pcBackendMode = PcBackendMode.Forked)
    val state = Bootstrap.run(ws, cfg, docs, overlay)
    val s = state.ready.getOrElse(fail(s"bootstrap not ready: ${state.statusLine}"))
    try
      assert(s.pc.isInstanceOf[ForkedPcBackend], s.pc.getClass.getName)
      // forked backend reports Some(_) (worker not yet spawned: no targets), not
      // the in-process None — proves the flag selected the forked backend.
      assert(s.pc.workerAlive.isDefined, s.pc.workerAlive.toString)
    finally s.close()

  test("forked mode does not load configured PC plugins in the main process (in-process does)"):
    val ws = Files.createTempDirectory("ls-core-forked-plugin-iso")
    // a present, valid (empty) plugin config in the conventional location
    val cfgFile = ls.pc.PcPluginConfigLoader.defaultPath(ws)
    Files.createDirectories(cfgFile.getParent)
    Files.writeString(cfgFile, """{"compilerPlugins": []}""")

    def boot(mode: PcBackendMode): WorkspaceState =
      val docs = new DocumentStore
      Bootstrap.run(ws, Bootstrap.Config(connectBsp = (_, _) => None, pcBackendMode = mode), docs, new PcOverlay(docs))

    // Forked: the config is passed to the child, so the MAIN process must NOT load
    // plugins (else a plugin crash on load defeats process isolation).
    val forked = boot(PcBackendMode.Forked)
    val fs = forked.ready.getOrElse(fail(s"forked bootstrap not ready: ${forked.statusLine}"))
    try
      assert(fs.pc.isInstanceOf[ForkedPcBackend], fs.pc.getClass.getName)
      assert(
        !fs.notes.exists(_.contains("applied PC plugin config")),
        s"forked mode must not load configured plugins in the main process: ${fs.notes}"
      )
    finally fs.close()

    // In-process: the same present config IS loaded in this JVM.
    val inproc = boot(PcBackendMode.InProcess)
    val is = inproc.ready.getOrElse(fail(s"in-process bootstrap not ready: ${inproc.statusLine}"))
    try
      assert(
        is.notes.exists(_.contains("applied PC plugin config")),
        s"in-process mode should load the present plugin config: ${is.notes}"
      )
    finally is.close()

  test("Bootstrap defaults to the in-process backend (no BSP)"):
    val ws = Files.createTempDirectory("ls-core-inproc-bootstrap")
    val docs = new DocumentStore
    val overlay = new PcOverlay(docs)
    val state = Bootstrap.run(ws, Bootstrap.Config(connectBsp = (_, _) => None), docs, overlay)
    val s = state.ready.getOrElse(fail(s"bootstrap not ready: ${state.statusLine}"))
    try
      assert(s.pc.isInstanceOf[InProcessPcBackend], s.pc.getClass.getName)
      assertEquals(s.pc.workerAlive, None)
    finally s.close()
