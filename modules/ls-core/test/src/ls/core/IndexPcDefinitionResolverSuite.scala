package ls.core

import java.io.File
import java.nio.file.{Files, Paths}

import ls.postings.SnapshotManager
import ls.rename.ingest.{IngestPipeline, TargetSpec, WorkspaceTargets}
import ls.sqlite.MetaStore

/** [[IndexPcDefinitionResolver]] must answer `SymbolSearch.definition` with the
  * declaration of EXACTLY the requested SemanticDB symbol, not the whole ref
  * group. A class and its companion object share a ref group (v1 alias policy),
  * so scanning the group's definitions would leak the companion's declaration
  * into cross-file go-to on the class (and vice versa).
  */
class IndexPcDefinitionResolverSuite extends munit.FunSuite:

  override def munitTimeout: scala.concurrent.duration.Duration =
    scala.concurrent.duration.Duration(5, "min")

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

  // A class and its companion object: `p/Core#` and `p/Core.` share one ref group.
  private val source =
    """|package p
       |class Core(val x: Int):
       |  def ping(): Int = x
       |object Core:
       |  def make(): Core = new Core(0)
       |""".stripMargin

  test("definition returns only the requested symbol's decl, not its companion alias group"):
    val root = Files.createTempDirectory("ls-pcdef-resolver-")
    root.toFile.deleteOnExit()
    val src = root.resolve("src/p/Core.scala")
    Files.createDirectories(src.getParent)
    Files.writeString(src, source)
    val classes = root.resolve("out")
    Files.createDirectories(classes)
    val reporter = dotty.tools.dotc.Main.process(
      Array(
        "-Xsemanticdb",
        "-sourceroot",
        root.toString,
        "-d",
        classes.toString,
        "-classpath",
        libraryJars.mkString(File.pathSeparator),
        src.toString
      )
    )
    assert(!reporter.hasErrors, s"fixture failed to compile:\n${reporter.allErrors.mkString("\n")}")

    val storeDir = root.resolve("store")
    Files.createDirectories(storeDir)
    val meta = MetaStore.open(storeDir.resolve("meta.sqlite"))
    val snapshots = SnapshotManager(storeDir.resolve("postings"))
    try
      val report = IngestPipeline(meta, snapshots).ingest(
        WorkspaceTargets(Vector(TargetSpec(bspId = "p", semanticdbRoot = classes, sourceroot = root)))
      )
      assert(report.docsIndexed > 0, s"ingest indexed no docs: $report")

      val resolver = new IndexPcDefinitionResolver(meta, snapshots)
      val lines = source.split("\n", -1)
      val classLine = lines.indexWhere(_.contains("class Core"))
      val objLine = lines.indexWhere(_.startsWith("object Core"))
      assert(classLine >= 0 && objLine >= 0 && classLine != objLine, s"class=$classLine obj=$objLine")

      // The class symbol resolves to the class decl only, NOT the companion decl.
      val classDefLines =
        resolver.definition("p/Core#", "file:///buffer.scala").map(_.getRange.getStart.getLine).toSet
      assert(classDefLines.nonEmpty, "definition of p/Core# returned nothing")
      assert(classDefLines.contains(classLine), s"should include the class decl (line $classLine); got $classDefLines")
      assert(
        !classDefLines.contains(objLine),
        s"must NOT leak the companion object decl (line $objLine); got $classDefLines"
      )

      // Symmetric: the companion symbol resolves to the object decl only.
      val objDefLines =
        resolver.definition("p/Core.", "file:///buffer.scala").map(_.getRange.getStart.getLine).toSet
      assert(objDefLines.contains(objLine), s"should include the object decl (line $objLine); got $objDefLines")
      assert(!objDefLines.contains(classLine), s"must NOT leak the class decl (line $classLine); got $objDefLines")
    finally
      snapshots.close()
      meta.close()
