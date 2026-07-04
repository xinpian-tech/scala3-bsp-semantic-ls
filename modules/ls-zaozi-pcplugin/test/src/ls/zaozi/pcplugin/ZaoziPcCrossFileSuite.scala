package ls.zaozi.pcplugin

import java.io.File
import java.nio.file.{Files, Path, Paths}

import ls.core.IndexPcDefinitionResolver
import ls.pc.{PcDefinitionResolver, PcFacade, PcPluginInitContext, PcPluginManager, PcSettings, PcTargetConfig}
import ls.postings.SnapshotManager
import ls.rename.ingest.{IngestPipeline, TargetSpec, WorkspaceTargets}
import ls.sqlite.MetaStore

/** CROSS-FILE go-to-definition through the index-backed `SymbolSearch`.
  *
  * A zaozi-shaped "library" (the mini `Referable`/`DynamicSubfield` API plus a
  * `LibBundle` with a normal method and a dynamic field) is compiled with
  * `-Xsemanticdb` into a classes dir — NOT the open buffer — and ingested into
  * a real index (MetaStore + postings). The presentation compiler then serves
  * a buffer that only USES the library:
  *
  *   - `io2.normalMethod()` — a plain member: the PC resolves the symbol from
  *     the classpath and must get its LOCATION from the index resolver;
  *   - `io.a` — a Dynamic bundle-field access: the zaozi plugin (`-Xplugin`)
  *     rewrites it to the real field symbol, whose location again only the
  *     index knows.
  *
  * The no-resolver baseline pins causality: same buffer, same plugin, no
  * index — no cross-file locations.
  */
class ZaoziPcCrossFileSuite extends munit.FunSuite:

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

  private val libraryJars: Vector[String] =
    val jars = System
      .getProperty("java.class.path", "")
      .split(File.pathSeparatorChar)
      .toVector
      .filter { e =>
        val n = Paths.get(e).getFileName.toString
        n.endsWith(".jar") && (n.startsWith("scala-library") || n.startsWith("scala3-library"))
      }
    assert(jars.nonEmpty, "no scala library jar on test classpath")
    jars

  private val libUri = "lib/src/sample/Lib.scala"

  /** The separately-compiled library: zaozi mini-API + a bundle with a normal
    * member and a Dynamic subfield. Definitions live HERE, never in the buffer.
    */
  private val libSource =
    """package me.jiuyang.zaozi.magic { trait DynamicSubfield }
      |package me.jiuyang.zaozi.reftpe {
      |  import scala.language.dynamics
      |  trait Referable[T] extends scala.Dynamic:
      |    transparent inline def selectDynamic(name: String): Any = referHelper(this, name)
      |  def referHelper(r: Any, name: String): Any = null
      |}
      |package sample {
      |  import me.jiuyang.zaozi.magic.DynamicSubfield
      |  class LibBundle extends DynamicSubfield:
      |    val a: Int = 0
      |    def normalMethod(): Int = 1
      |}
      |""".stripMargin

  /** The open buffer: only USES of the library symbols. */
  private val bufferText =
    """import me.jiuyang.zaozi.reftpe.*
      |import sample.LibBundle
      |
      |object Use:
      |  val io: Referable[LibBundle] = null.asInstanceOf[Referable[LibBundle]]
      |  val io2: LibBundle = new LibBundle
      |  val x = io.a
      |  val y = io2.normalMethod()
      |""".stripMargin

  /** Compiled + ingested library fixture, built once for the suite. */
  private final case class Fixture(
      root: Path,
      classesDir: Path,
      meta: MetaStore,
      snapshots: SnapshotManager,
      resolver: PcDefinitionResolver
  )

  private lazy val fixture: Fixture =
    val root = Files.createTempDirectory("ls-zaozi-crossfile-")
    root.toFile.deleteOnExit()
    val src = root.resolve(libUri)
    Files.createDirectories(src.getParent)
    Files.writeString(src, libSource)
    val classes = root.resolve("out-lib")
    Files.createDirectories(classes)
    val args = Array(
      "-Xsemanticdb",
      "-sourceroot",
      root.toString,
      "-d",
      classes.toString,
      "-classpath",
      libraryJars.mkString(File.pathSeparator),
      src.toString
    )
    val reporter = dotty.tools.dotc.Main.process(args)
    assert(!reporter.hasErrors, s"library scalac failed:\n${reporter.allErrors.mkString("\n")}")

    val storeDir = root.resolve("store")
    Files.createDirectories(storeDir)
    val meta = MetaStore.open(storeDir.resolve("meta.sqlite"))
    val snapshots = SnapshotManager(storeDir.resolve("postings"))
    val pipeline = IngestPipeline(meta, snapshots)
    val report = pipeline.ingest(
      WorkspaceTargets(Vector(TargetSpec(bspId = "lib", semanticdbRoot = classes, sourceroot = root)))
    )
    assert(report.docsIndexed > 0, s"library ingest indexed no docs: $report")
    Fixture(root, classes, meta, snapshots, new IndexPcDefinitionResolver(meta, snapshots))

  override def afterAll(): Unit =
    try
      fixture.snapshots.close()
      fixture.meta.close()
    catch case scala.util.control.NonFatal(_) => ()

  private def newFacade(resolver: PcDefinitionResolver): (PcFacade, String) =
    val genRoot = Files.createTempDirectory("ls-zaozi-crossfile-gen")
    val pm = new PcPluginManager(PcPluginInitContext(None, genRoot))
    val facade = new PcFacade(pm, PcSettings(None, genRoot, 4, 90000L), resolver)
    val targetId = "zaoziCrossFileTarget"
    facade.registerTarget(
      PcTargetConfig(
        targetId,
        libraryJars.map(Paths.get(_)) :+ fixture.classesDir,
        Vector(s"-Xplugin:$pluginJar")
      )
    )
    (facade, targetId)

  /** (line, character) of `marker` in `text`, offset into the marker. */
  private def cursor(text: String, marker: String, offsetInMarker: Int): (Int, Int) =
    val lines = text.split("\n", -1)
    var i = 0
    while i < lines.length do
      val idx = lines(i).indexOf(marker)
      if idx >= 0 then return (i, idx + offsetInMarker)
      i += 1
    fail(s"marker '$marker' not found")

  /** Definition locations at the cursor, as (fileName, startLine0). */
  private def definitionsAt(
      facade: PcFacade,
      targetId: String,
      uri: String,
      marker: String,
      offset: Int
  ): Vector[(String, Int)] =
    val (line, ch) = cursor(bufferText, marker, offset)
    facade
      .definition(uri, line, ch)
      .locations
      .map(dl =>
        (
          dl.location.getUri.split('/').lastOption.getOrElse(dl.location.getUri),
          dl.location.getRange.getStart.getLine
        )
      )

  test("index-backed resolver: cross-file go-to reaches the library for BOTH a normal member and a Dynamic field"):
    val libLines = libSource.split("\n", -1)
    val normalDeclLine = libLines.indexWhere(_.contains("def normalMethod(): Int = 1"))
    val fieldDeclLine = libLines.indexWhere(_.contains("val a: Int = 0"))
    assert(normalDeclLine >= 0 && fieldDeclLine >= 0)

    val (facade, targetId) = newFacade(fixture.resolver)
    try
      val uri = "file:///ls-zaozi-crossfile/UseBuffer.scala"
      facade.didOpen(targetId, uri, bufferText)

      // normal member: io2.normalMethod() -> `def normalMethod` in Lib.scala
      val normalLocs = definitionsAt(facade, targetId, uri, "io2.normalMethod", "io2.norm".length)
      assert(
        normalLocs.contains(("Lib.scala", normalDeclLine)),
        s"cross-file NORMAL member definition missing: got $normalLocs, want (Lib.scala, $normalDeclLine)"
      )

      // Dynamic field: io.a -> plugin resolves the field symbol, index the location
      val dynamicLocs = definitionsAt(facade, targetId, uri, "io.a", 3)
      assert(
        dynamicLocs.contains(("Lib.scala", fieldDeclLine)),
        s"cross-file DYNAMIC field definition missing: got $dynamicLocs, want (Lib.scala, $fieldDeclLine)"
      )
    finally facade.shutdown()

  test("baseline: without the index resolver the same cross-file queries return no library location"):
    val (facade, targetId) = newFacade(PcDefinitionResolver.Empty)
    try
      val uri = "file:///ls-zaozi-crossfile/BaselineBuffer.scala"
      facade.didOpen(targetId, uri, bufferText)
      val normalLocs = definitionsAt(facade, targetId, uri, "io2.normalMethod", "io2.norm".length)
      val dynamicLocs = definitionsAt(facade, targetId, uri, "io.a", 3)
      assert(
        !(normalLocs ++ dynamicLocs).exists(_._1 == "Lib.scala"),
        s"baseline must not reach the library without the index: normal=$normalLocs dynamic=$dynamicLocs"
      )
    finally facade.shutdown()
