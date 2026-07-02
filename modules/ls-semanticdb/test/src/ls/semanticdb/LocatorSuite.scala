package ls.semanticdb

import java.nio.file.{Files, Path}

class LocatorSuite extends munit.FunSuite:

  private def deleteRecursively(root: Path): Unit =
    import scala.jdk.CollectionConverters.*
    val stream = Files.walk(root)
    try stream.iterator().asScala.toVector.reverse.foreach(Files.deleteIfExists(_))
    finally stream.close()

  private val targetroot = FunFixture[Path](
    setup = _ => Files.createTempDirectory(Path.of(System.getProperty("user.dir")), "targetroot-"),
    teardown = root => deleteRecursively(root)
  )

  targetroot.test("lists semanticdb files recursively and sorted"): root =>
    val sdbRoot = root.resolve("META-INF/semanticdb")
    Files.createDirectories(sdbRoot.resolve("src/main/scala/a"))
    Files.createDirectories(sdbRoot.resolve("src/test"))
    val f1 = sdbRoot.resolve("src/main/scala/a/B.scala.semanticdb")
    val f2 = sdbRoot.resolve("src/test/T.scala.semanticdb")
    val junk = sdbRoot.resolve("src/test/notes.txt")
    Files.write(f1, Array[Byte](1))
    Files.write(f2, Array[Byte](2))
    Files.write(junk, Array[Byte](3))

    val locator = SemanticdbLocator(root)
    assertEquals(locator.listSemanticdbFiles(), Vector(f1, f2).sortBy(_.toString))

  targetroot.test("returns empty when the semanticdb root is missing"): root =>
    assertEquals(SemanticdbLocator(root).listSemanticdbFiles(), Vector.empty)

  targetroot.test("maps source-relative path to semanticdb file and back"): root =>
    val locator = SemanticdbLocator(root)
    val rel = "src/main/scala/a/B.scala"
    val expected = root.resolve("META-INF/semanticdb/src/main/scala/a/B.scala.semanticdb")
    assertEquals(locator.semanticdbFileFor(rel), expected)
    assertEquals(locator.sourceRelativePathFor(expected), Some(rel))

  targetroot.test("round-trips every listed file"): root =>
    val sdbRoot = root.resolve("META-INF/semanticdb")
    Files.createDirectories(sdbRoot.resolve("x/y"))
    Files.write(sdbRoot.resolve("x/y/Z.scala.semanticdb"), Array[Byte](0))
    val locator = SemanticdbLocator(root)
    for f <- locator.listSemanticdbFiles() do
      val rel = locator.sourceRelativePathFor(f)
      assertEquals(rel.map(locator.semanticdbFileFor), Some(f))

  targetroot.test("rejects files outside the semanticdb root or without suffix"): root =>
    val locator = SemanticdbLocator(root)
    assertEquals(locator.sourceRelativePathFor(root.resolve("elsewhere/A.scala.semanticdb")), None)
    assertEquals(
      locator.sourceRelativePathFor(root.resolve("META-INF/semanticdb/A.scala")),
      None
    )
    intercept[IllegalArgumentException](locator.semanticdbFileFor("/absolute/A.scala"))
    intercept[IllegalArgumentException](locator.semanticdbFileFor("../../escape.scala"))
