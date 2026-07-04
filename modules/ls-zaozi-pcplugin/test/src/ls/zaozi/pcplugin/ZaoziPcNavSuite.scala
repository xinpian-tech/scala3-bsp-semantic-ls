package ls.zaozi.pcplugin

import java.io.File
import java.nio.file.{Files, Path, Paths}

import ls.pc.{DefinitionOrigin, PcFacade, PcPluginInitContext, PcPluginManager, PcSettings, PcTargetConfig}

/** Drives the real dotty presentation compiler with the built plugin jar loaded
  * via `-Xplugin`, and proves the plugin steers go-to on a zaozi-shaped dynamic
  * field access to the field declaration. Each test builds its own isolated
  * facade so plugins/targets never leak between tests.
  */
class ZaoziPcNavSuite extends munit.FunSuite:

  override def munitTimeout: scala.concurrent.duration.Duration =
    scala.concurrent.duration.Duration(5, "min")

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

  /** A single-buffer, NON-macro reproduction of zaozi's Dynamic bundle-field
    * API under the real package names the plugin keys on. `transparent inline
    * selectDynamic` yields the same `Inlined(call = io.selectDynamic("a"))` node
    * as the real macro, with no separate compilation.
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

  private def newFacade(scalacOptions: Vector[String]): (PcFacade, String) =
    val genRoot = Files.createTempDirectory("ls-zaozi-pcplugin-gen")
    val pm = new PcPluginManager(PcPluginInitContext(None, genRoot))
    val facade = new PcFacade(pm, PcSettings(None, genRoot, 4, 90000L))
    val targetId = "zaoziNavTarget"
    facade.registerTarget(PcTargetConfig(targetId, libraryClasspath, scalacOptions))
    (facade, targetId)

  /** (line, character) of `marker` in `text`, offset into the marker. */
  private def cursor(text: String, marker: String, offsetInMarker: Int): (Int, Int) =
    val lines = text.split("\n", -1)
    var i = 0
    while i < lines.length do
      val idx = lines(i).indexOf(marker)
      if idx >= 0 then return (i, idx + offsetInMarker)
      i += 1
    fail(s"marker '$marker' not found in fixture")

  private def lineOf(text: String, marker: String): Int =
    cursor(text, marker, 0)._1

  private def defLines(facade: PcFacade, targetId: String): Vector[Int] =
    val uri = "file:///ls-zaozi-pcplugin-test/Buffer.scala"
    facade.didOpen(targetId, uri, fixture)
    val (line, ch) = cursor(fixture, "io.a", 3) // on the `a` of `io.a`
    val result = facade.definition(uri, line, ch)
    result.locations.map(_.location.getRange.getStart.getLine)

  test("with the plugin, go-to on a dynamic io.a resolves to the field val a; without it, it does not"):
    val valALine = lineOf(fixture, "val a: Int = 0")

    val (withPlugin, tid1) = newFacade(Vector(s"-Xplugin:$pluginJar"))
    val withLines =
      try defLines(withPlugin, tid1)
      finally withPlugin.shutdown()
    assert(
      withLines.contains(valALine),
      s"with the plugin, go-to on io.a should reach `val a` (line $valALine); got def lines $withLines"
    )

    val (noPlugin, tid2) = newFacade(Vector.empty)
    val baseLines =
      try defLines(noPlugin, tid2)
      finally noPlugin.shutdown()
    assert(
      !baseLines.contains(valALine),
      s"without the plugin, go-to on io.a must NOT reach `val a`; baseline def lines $baseLines"
    )

  test("with the plugin, hover on a dynamic io.a describes the field type; without it, it does not"):
    def hoverText(scalacOptions: Vector[String]): String =
      val (facade, tid) = newFacade(scalacOptions)
      try
        val uri = "file:///ls-zaozi-pcplugin-test/HoverBuffer.scala"
        facade.didOpen(tid, uri, fixture)
        val (line, ch) = cursor(fixture, "io.a", 3)
        facade.hover(uri, line, ch).fold("")(_.toString)
      finally facade.shutdown()

    val withHover = hoverText(Vector(s"-Xplugin:$pluginJar"))
    val baseHover = hoverText(Vector.empty)
    assert(
      withHover.contains("Int"),
      s"with the plugin, hover on io.a should describe the `val a: Int` field; got: $withHover"
    )
    assert(
      !baseHover.contains("Int"),
      s"without the plugin, hover on io.a should not describe the field type; got: $baseHover"
    )

  test("the plugin does not rewrite a non-zaozi scala.Dynamic access"):
    // Same shape, but the receiver is NOT a me.jiuyang.zaozi.reftpe.Referable.
    val alien =
      """|package other {
         |  import scala.language.dynamics
         |  trait Widget[T] extends scala.Dynamic:
         |    transparent inline def selectDynamic(name: String): Any = widgetHelper(this, name)
         |  def widgetHelper(r: Any, name: String): Any = null
         |  class Panel:
         |    val a: Int = 0
         |  object Top:
         |    val io: Widget[Panel] = null.asInstanceOf[Widget[Panel]]
         |    val probe = io.a
         |}
         |""".stripMargin
    def defLinesFor(scalacOptions: Vector[String]): Vector[Int] =
      val (facade, tid) = newFacade(scalacOptions)
      try
        val uri = "file:///ls-zaozi-pcplugin-test/AlienBuffer.scala"
        facade.didOpen(tid, uri, alien)
        val (line, ch) = cursor(alien, "io.a", 3)
        facade.definition(uri, line, ch).locations.map(_.location.getRange.getStart.getLine)
      finally facade.shutdown()

    val valALine = lineOf(alien, "val a: Int = 0")
    val withPlugin = defLinesFor(Vector(s"-Xplugin:$pluginJar"))
    val baseline = defLinesFor(Vector.empty)
    assertEquals(
      withPlugin,
      baseline,
      s"a non-zaozi Dynamic access must be unchanged by the plugin (with=$withPlugin base=$baseline)"
    )
    assert(
      !withPlugin.contains(valALine),
      s"the plugin must not resolve a non-zaozi Panel#a; got def lines $withPlugin"
    )

  test("the plugin degrades to identity (no crash) on a dynamic access to a missing field"):
    val missing =
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
         |    val probe = io.notAField
         |}
         |""".stripMargin
    val (facade, tid) = newFacade(Vector(s"-Xplugin:$pluginJar"))
    try
      val uri = "file:///ls-zaozi-pcplugin-test/MissingBuffer.scala"
      facade.didOpen(tid, uri, missing)
      val (line, ch) = cursor(missing, "io.notAField", 3)
      // The request must return (no exception from the guarded phase).
      val result = facade.definition(uri, line, ch)
      assert(result != null, "definition on a missing dynamic field must still return a result")
    finally facade.shutdown()
