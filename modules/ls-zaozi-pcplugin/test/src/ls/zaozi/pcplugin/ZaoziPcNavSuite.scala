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

  test("go-to on io.a lands on the `val a` name range, not just its line"):
    val (facade, tid) = newFacade(Vector(s"-Xplugin:$pluginJar"))
    val (locs) =
      try
        val uri = "file:///ls-zaozi-pcplugin-test/RangeBuffer.scala"
        facade.didOpen(tid, uri, fixture)
        val (line, ch) = cursor(fixture, "io.a", 3)
        facade.definition(uri, line, ch).locations.map { d =>
          val s = d.location.getRange.getStart; (s.getLine, s.getCharacter)
        }
      finally facade.shutdown()
    val (nameLine, nameChar) = cursor(fixture, "val a: Int = 0", 4) // the `a` in `val a`
    assert(
      locs.contains((nameLine, nameChar)),
      s"expected a definition at the `val a` name ($nameLine,$nameChar); got $locs"
    )

  test("with the plugin, go-to works through a real selectDynamic macro expansion (getRefViaFieldValName)"):
    val classes = macroFixtureClasses
    val buffer =
      """|package sample
         |import me.jiuyang.zaozi.reftpe.*
         |import me.jiuyang.zaozi.magic.DynamicSubfield
         |class MyBundle extends DynamicSubfield:
         |  val a: Int = 0
         |object Top:
         |  val io: Referable[MyBundle] = null.asInstanceOf[Referable[MyBundle]]
         |  val probe = io.a
         |""".stripMargin
    def defLinesFor(scalacOptions: Vector[String]): Vector[Int] =
      val genRoot = Files.createTempDirectory("ls-zaozi-macro-gen")
      val pm = new PcPluginManager(PcPluginInitContext(None, genRoot))
      val facade = new PcFacade(pm, PcSettings(None, genRoot, 4, 90000L))
      val tid = "zaoziMacroTarget"
      facade.registerTarget(PcTargetConfig(tid, libraryClasspath :+ classes, scalacOptions))
      try
        val uri = "file:///ls-zaozi-pcplugin-test/MacroBuffer.scala"
        facade.didOpen(tid, uri, buffer)
        val (line, ch) = cursor(buffer, "io.a", 3)
        facade.definition(uri, line, ch).locations.map(_.location.getRange.getStart.getLine)
      finally facade.shutdown()
    val valALine = lineOf(buffer, "val a: Int = 0")
    assert(defLinesFor(Vector(s"-Xplugin:$pluginJar")).contains(valALine), "macro-expanded io.a should reach val a")
    assert(!defLinesFor(Vector.empty).contains(valALine), "baseline macro-expanded io.a must not reach val a")

  test("nested io.f.g resolves BOTH segments and optional io.k resolves, each to its own field decl"):
    // A two-level bundle: RootBundle.f is itself a DynamicSubfield (SimpleBundle),
    // so `io.f` types as Referable[SimpleBundle] (via the macro) and `io.f.g` is a
    // second dynamic select. `k` is a second RootBundle field (the optional-field
    // position). The bundle classes live in the buffer so go-to lands on their
    // declarations; the macro API is the pre-compiled `macroFixtureClasses`.
    val classes = macroFixtureClasses
    val buffer =
      """|package sample
         |import me.jiuyang.zaozi.reftpe.*
         |import me.jiuyang.zaozi.magic.DynamicSubfield
         |class SimpleBundle extends DynamicSubfield:
         |  val g: Int = 0
         |class RootBundle extends DynamicSubfield:
         |  val f: SimpleBundle = null.asInstanceOf[SimpleBundle]
         |  val k: Int = 0
         |object Top:
         |  val io: Referable[RootBundle] = null.asInstanceOf[Referable[RootBundle]]
         |  val p1 = io.f
         |  val p2 = io.f.g
         |  val p3 = io.k
         |""".stripMargin
    def defLinesAt(scalacOptions: Vector[String], marker: String, offset: Int): Vector[Int] =
      val genRoot = Files.createTempDirectory("ls-zaozi-nested-gen")
      val pm = new PcPluginManager(PcPluginInitContext(None, genRoot))
      val facade = new PcFacade(pm, PcSettings(None, genRoot, 4, 90000L))
      val tid = "zaoziNestedTarget"
      facade.registerTarget(PcTargetConfig(tid, libraryClasspath :+ classes, scalacOptions))
      try
        val uri = "file:///ls-zaozi-pcplugin-test/NestedBuffer.scala"
        facade.didOpen(tid, uri, buffer)
        val (line, ch) = cursor(buffer, marker, offset)
        facade.definition(uri, line, ch).locations.map(_.location.getRange.getStart.getLine)
      finally facade.shutdown()

    val fLine = lineOf(buffer, "val f: SimpleBundle")
    val gLine = lineOf(buffer, "val g: Int")
    val kLine = lineOf(buffer, "val k: Int")
    val plugin = Vector(s"-Xplugin:$pluginJar")

    // cursor on the `f` of `io.f` (p1 line) -> RootBundle.f
    assert(defLinesAt(plugin, "io.f", 3).contains(fLine), s"io.f should reach RootBundle.f (line $fLine)")
    // cursor on the `g` of `io.f.g` (p2 line) -> SimpleBundle.g
    assert(defLinesAt(plugin, "io.f.g", 5).contains(gLine), s"nested io.f.g should reach SimpleBundle.g (line $gLine)")
    // cursor on the `k` of `io.k` (p3 line) -> RootBundle.k
    assert(defLinesAt(plugin, "io.k", 3).contains(kLine), s"optional io.k should reach RootBundle.k (line $kLine)")

    // baseline: without the plugin, the nested segment does NOT reach the field
    assert(
      !defLinesAt(Vector.empty, "io.f.g", 5).contains(gLine),
      "without the plugin, nested io.f.g must not reach SimpleBundle.g"
    )

  test("go-to resolves the field on a non-Interface Referable receiver (Writable)"):
    val wrapperFixture =
      """|package me.jiuyang.zaozi.magic { trait DynamicSubfield }
         |package me.jiuyang.zaozi.reftpe {
         |  import scala.language.dynamics
         |  trait Referable[T] extends scala.Dynamic:
         |    transparent inline def selectDynamic(name: String): Any = referHelper(this, name)
         |  trait Writable[T] extends Referable[T]
         |  def referHelper(r: Any, name: String): Any = null
         |}
         |package sample {
         |  import me.jiuyang.zaozi.reftpe.*
         |  import me.jiuyang.zaozi.magic.DynamicSubfield
         |  class MyBundle extends DynamicSubfield:
         |    val a: Int = 0
         |  object Top:
         |    val io: Writable[MyBundle] = null.asInstanceOf[Writable[MyBundle]]
         |    val probe = io.a
         |}
         |""".stripMargin
    val (facade, tid) = newFacade(Vector(s"-Xplugin:$pluginJar"))
    val lines =
      try
        val uri = "file:///ls-zaozi-pcplugin-test/WritableBuffer.scala"
        facade.didOpen(tid, uri, wrapperFixture)
        val (line, ch) = cursor(wrapperFixture, "io.a", 3)
        facade.definition(uri, line, ch).locations.map(_.location.getRange.getStart.getLine)
      finally facade.shutdown()
    assert(lines.contains(lineOf(wrapperFixture, "val a: Int = 0")), s"Writable receiver should resolve; got $lines")

  test("applyDynamic index/slice access is left as identity"):
    val applyFixture =
      """|package me.jiuyang.zaozi.magic { trait DynamicSubfield }
         |package me.jiuyang.zaozi.reftpe {
         |  import scala.language.dynamics
         |  trait Referable[T] extends scala.Dynamic:
         |    transparent inline def selectDynamic(name: String): Any = referHelper(this, name)
         |    transparent inline def applyDynamic(name: String)(inline args: Any*): Any = applyHelper(this, name, args)
         |  def referHelper(r: Any, name: String): Any = null
         |  def applyHelper(r: Any, name: String, args: Seq[Any]): Any = null
         |}
         |package sample {
         |  import me.jiuyang.zaozi.reftpe.*
         |  import me.jiuyang.zaozi.magic.DynamicSubfield
         |  class MyBundle extends DynamicSubfield:
         |    val vec: Int = 0
         |  object Top:
         |    val io: Referable[MyBundle] = null.asInstanceOf[Referable[MyBundle]]
         |    val probe = io.vec(0)
         |}
         |""".stripMargin
    def defLinesFor(scalacOptions: Vector[String]): Vector[Int] =
      val (facade, tid) = newFacade(scalacOptions)
      try
        val uri = "file:///ls-zaozi-pcplugin-test/ApplyBuffer.scala"
        facade.didOpen(tid, uri, applyFixture)
        // cursor on the `(0)` index-call, not the field name
        val (line, ch) = cursor(applyFixture, "io.vec(0)", 7)
        facade.definition(uri, line, ch).locations.map(_.location.getRange.getStart.getLine)
      finally facade.shutdown()
    assertEquals(
      defLinesFor(Vector(s"-Xplugin:$pluginJar")),
      defLinesFor(Vector.empty),
      "applyDynamic index/slice must be identity (unchanged by the plugin)"
    )

  test("hover is unchanged on a non-zaozi dynamic access and does not crash on a missing field"):
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
    def hoverText(text: String, scalacOptions: Vector[String]): String =
      val (facade, tid) = newFacade(scalacOptions)
      try
        val uri = "file:///ls-zaozi-pcplugin-test/AlienHover.scala"
        facade.didOpen(tid, uri, text)
        val (line, ch) = cursor(text, "io.a", 3)
        facade.hover(uri, line, ch).fold("")(_.toString)
      finally facade.shutdown()
    // non-zaozi: plugin must not change hover
    assertEquals(
      hoverText(alien, Vector(s"-Xplugin:$pluginJar")),
      hoverText(alien, Vector.empty),
      "hover on a non-zaozi dynamic access must be unchanged by the plugin"
    )
    // unresolved field: hover must not crash (returns some result, possibly empty)
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
         |    val probe = io.nope
         |}
         |""".stripMargin
    val (facade, tid) = newFacade(Vector(s"-Xplugin:$pluginJar"))
    try
      val uri = "file:///ls-zaozi-pcplugin-test/MissingHover.scala"
      facade.didOpen(tid, uri, missing)
      val (line, ch) = cursor(missing, "io.nope", 3)
      assert(facade.hover(uri, line, ch) != null, "hover on a missing field must not crash")
    finally facade.shutdown()

  /** Compile a mini zaozi API with a real `selectDynamic` macro (whose expansion is
    * `getRefViaFieldValName`) into a classes dir the PC target can reference. Macros
    * require a prior compilation unit, which this provides.
    *
    * The macro types `selectDynamic("f")` as `Referable[<type of field f>]` (like
    * zaozi's real macro), so a NESTED access `io.f.g` typechecks: `io.f` is a
    * `Referable[SimpleBundle]` on which `.g` is another dynamic select. Without
    * that precise result type the intermediate `io.f` would be `Any` and `.g`
    * would not compile, so nested resolution could not be unit-tested at all. */
  private lazy val macroFixtureClasses: Path =
    // Scala 3 rejects two top-level `package` clauses in one file, so split the
    // mini API into one file per package.
    val magicSrc =
      """|package me.jiuyang.zaozi.magic
         |trait DynamicSubfield:
         |  def getRefViaFieldValName(refer: Any, name: String): Any = null
         |""".stripMargin
    val reftpeSrc =
      """|package me.jiuyang.zaozi.reftpe
         |import scala.language.dynamics
         |import scala.quoted.*
         |trait Referable[T] extends scala.Dynamic:
         |  def _tpe: T
         |  def refer: Any = null
         |  transparent inline def selectDynamic(name: String): Any = ${ ZaoziFixtureMacros.sel[T]('this, 'name) }
         |object ZaoziFixtureMacros:
         |  def sel[T: Type](self: Expr[Referable[T]], name: Expr[String])(using Quotes): Expr[Any] =
         |    import quotes.reflect.*
         |    val fieldName = name.valueOrAbort
         |    val tRepr = TypeRepr.of[T]
         |    val fieldSym = tRepr.typeSymbol.fieldMember(fieldName)
         |    val fieldTpe = if fieldSym.exists then tRepr.memberType(fieldSym) else TypeRepr.of[Any]
         |    fieldTpe.asType match
         |      case '[ft] =>
         |        '{ $self._tpe.asInstanceOf[me.jiuyang.zaozi.magic.DynamicSubfield]
         |             .getRefViaFieldValName($self.refer, $name).asInstanceOf[Referable[ft]] }
         |""".stripMargin
    val srcDir = Files.createTempDirectory("ls-zaozi-macro-src")
    val magicFile = srcDir.resolve("Magic.scala")
    val reftpeFile = srcDir.resolve("Referable.scala")
    Files.writeString(magicFile, magicSrc)
    Files.writeString(reftpeFile, reftpeSrc)
    val outDir = Files.createTempDirectory("ls-zaozi-macro-classes")
    val cp = libraryClasspath.map(_.toString).mkString(File.pathSeparator)
    val reporter = dotty.tools.dotc.Main.process(
      Array("-d", outDir.toString, "-classpath", cp, magicFile.toString, reftpeFile.toString)
    )
    assert(!reporter.hasErrors, "mini-zaozi macro fixture failed to compile")
    outDir
