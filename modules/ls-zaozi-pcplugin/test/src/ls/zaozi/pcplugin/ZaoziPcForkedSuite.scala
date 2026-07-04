package ls.zaozi.pcplugin

import java.io.File
import java.nio.file.{Files, Path, Paths}
import java.util.concurrent.{CompletableFuture, TimeUnit}

import scala.jdk.CollectionConverters.*

import ls.pc.{
  CompilerPluginSpec,
  ForkedPcWorker,
  PcCompilerPluginConfig,
  PcPluginConfig,
  PcPluginConfigLoader,
  PcTargetConfig,
  PcWorkerDidOpenParams,
  PcWorkerPositionParams,
  PcWorkerTargetParams
}

/** Boots a REAL forked PC worker JVM (`ls.pc.PcWorkerMain`) with the built plugin
  * jar configured ONLY through a `pc-plugins.json` `compilerPlugins` entry passed
  * via `--plugin-config` — never through the target's scalac options. This is the
  * production/shipped default backend path (`Main.pcBackendMode` defaults to
  * `Forked`, and `WorkspaceState` forwards the workspace `pc-plugins.json` to the
  * child via `--plugin-config`). It proves that path end-to-end: the child loads
  * and runs the plugin, so go-to on `io.a` reaches `val a` across the process
  * boundary, and the child reports the compiler plugin as loaded. A no-config
  * child is the baseline (resolves to `selectDynamic`, not the field).
  *
  * Set LS_PC_SKIP_FORK_TEST=1 to skip where forking a JVM from the test JVM is
  * not possible (matching `ForkedWorkerSuite`).
  */
class ZaoziPcForkedSuite extends munit.FunSuite:

  override def munitTimeout: scala.concurrent.duration.Duration =
    scala.concurrent.duration.Duration(5, "min")

  private def assumeForkAllowed(): Unit =
    assume(!sys.env.contains("LS_PC_SKIP_FORK_TEST"), "LS_PC_SKIP_FORK_TEST set: skipping fork test")

  private def get[A](f: CompletableFuture[A]): A = f.get(120, TimeUnit.SECONDS)

  private val pluginJar: Path =
    val p = sys.env.getOrElse(
      "ZAOZI_PCPLUGIN_JAR",
      fail("ZAOZI_PCPLUGIN_JAR unset — the test forkEnv must point at the built plugin jar")
    )
    val jar = Paths.get(p)
    assert(Files.isRegularFile(jar), s"plugin jar not found: $jar")
    jar

  private val libraryClasspath: Vector[Path] =
    val entries = System.getProperty("java.class.path", "").split(File.pathSeparatorChar).toVector
    val jars = entries.filter { e =>
      val n = Paths.get(e).getFileName.toString
      n.endsWith(".jar") && (n.startsWith("scala-library") || n.startsWith("scala3-library"))
    }.map(Paths.get(_))
    assert(jars.nonEmpty, s"no scala library jar on test classpath: $entries")
    jars

  /** A single-buffer, NON-macro reproduction of zaozi's Dynamic bundle-field API
    * under the real package names the plugin keys on — identical to the fixture
    * `ZaoziPcNavSuite` drives in-process, so the two backends prove the same
    * rewrite. `transparent inline selectDynamic` yields the same
    * `Inlined(call = io.selectDynamic("a"))` node as the real macro.
    */
  private val fixture =
    """|package me.jiuyang.zaozi.magic { trait DynamicSubfield }
       |package me.jiuyang.zaozi.reftpe {
       |  import scala.language.dynamics
       |  trait Referable[T] extends scala.Dynamic:
       |    transparent inline def selectDynamic(name: String): Any = referHelper(this, name)
       |  def referHelper(r: Any, name: String): Any = null
       |}
       |package sample {
       |  import me.jiuyang.zaozi.reftpe.*
       |  import me.jiuyang.zaozi.magic.DynamicSubfield
       |  class MyBundle extends DynamicSubfield:
       |    val a: Int = 0
       |  object Top:
       |    val io: Referable[MyBundle] = null.asInstanceOf[Referable[MyBundle]]
       |    val probe = io.a
       |}
       |""".stripMargin

  /** (line, character) of `marker` in `text`, offset into the marker. */
  private def cursor(text: String, marker: String, offsetInMarker: Int): (Int, Int) =
    val lines = text.split("\n", -1)
    var i = 0
    while i < lines.length do
      val idx = lines(i).indexOf(marker)
      if idx >= 0 then return (i, idx + offsetInMarker)
      i += 1
    fail(s"marker '$marker' not found in fixture")

  private def lineOf(text: String, marker: String): Int = cursor(text, marker, 0)._1

  /** Write a `pc-plugins.json` declaring the built plugin jar as a compiler plugin. */
  private def writePluginConfig(dir: Path): Path =
    val cfg = dir.resolve("pc-plugins.json")
    PcPluginConfigLoader.write(
      PcPluginConfig(
        PcCompilerPluginConfig(Vector(CompilerPluginSpec(Vector(pluginJar), Vector.empty))),
        Vector.empty
      ),
      cfg
    )
    cfg

  /** Open the fixture in a forked worker and return the go-to result line numbers
    * for `io.a`. The target carries NO `-Xplugin` in its scalac options: any plugin
    * effect must come from the child's `--plugin-config`.
    */
  private def defLinesForked(worker: ForkedPcWorker): Vector[Int] =
    val targetId = "zaoziForkedTarget"
    get(worker.initializeTarget(PcWorkerTargetParams.of(PcTargetConfig(targetId, libraryClasspath, Vector.empty))))
    val open = new PcWorkerDidOpenParams
    open.targetId = targetId
    open.uri = "file:///ls-zaozi-pcplugin-forked/Buffer.scala"
    open.text = fixture
    get(worker.didOpen(open))
    val (line, ch) = cursor(fixture, "io.a", 3) // on the `a` of `io.a`
    val pos = new PcWorkerPositionParams
    pos.uri = open.uri
    pos.line = line
    pos.character = ch
    get(worker.definition(pos)).locations.asScala.toVector.map(_.getRange.getStart.getLine)

  test("forked worker loads the plugin from pc-plugins.json (--plugin-config): go-to on io.a reaches val a"):
    assumeForkAllowed()
    val valALine = lineOf(fixture, "val a: Int = 0")
    val genDir = Files.createTempDirectory("ls-zaozi-forked-gen")
    val cfgDir = Files.createTempDirectory("ls-zaozi-forked-cfg")
    val cfg = writePluginConfig(cfgDir)

    val worker = new ForkedPcWorker(
      workerArgs = Vector(
        "--generated-sources", genDir.toString,
        "--plugin-config", cfg.toString,
        "--timeout-ms", "90000"
      ),
      requestTimeoutMillis = 120000
    )
    try
      val lines = defLinesForked(worker)
      assert(
        lines.contains(valALine),
        s"forked + plugin: go-to on io.a should reach `val a` (line $valALine) across the process boundary; got $lines"
      )
      // The child must report the configured compiler plugin as loaded — proof
      // the `--plugin-config` file reached the child's PcPluginManager.
      val loaded = get(worker.pluginStatus()).compilerPlugins.asScala.toVector
      assert(loaded.nonEmpty, "child should report the configured compiler plugin")
      assert(
        loaded.exists(cp => cp.loaded && cp.jars.asScala.exists(_ == pluginJar.toString)),
        s"child should report the plugin jar as loaded; got ${loaded.map(cp => (cp.jars, cp.loaded))}"
      )
    finally worker.close()

  test("baseline: a forked worker with NO plugin config resolves io.a to selectDynamic, not val a"):
    assumeForkAllowed()
    val valALine = lineOf(fixture, "val a: Int = 0")
    val genDir = Files.createTempDirectory("ls-zaozi-forked-gen-base")
    val worker = new ForkedPcWorker(
      workerArgs = Vector("--generated-sources", genDir.toString, "--timeout-ms", "90000"),
      requestTimeoutMillis = 120000
    )
    try
      val lines = defLinesForked(worker)
      assert(
        !lines.contains(valALine),
        s"baseline (no plugin config): go-to on io.a must NOT reach `val a`; got $lines"
      )
    finally worker.close()
