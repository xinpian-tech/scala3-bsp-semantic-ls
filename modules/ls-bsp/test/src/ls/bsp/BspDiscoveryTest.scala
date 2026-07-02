package ls.bsp

import java.nio.file.Files
import java.nio.file.Path

import scala.jdk.CollectionConverters.*

class BspDiscoveryTest extends munit.FunSuite:

  private def connectionJson(name: String, argv: List[String]): String =
    val argvJson = argv.map(a => "\"" + a + "\"").mkString("[", ",", "]")
    s"""{
       |  "name": "$name",
       |  "argv": $argvJson,
       |  "version": "1.4.2",
       |  "bspVersion": "2.1.1",
       |  "languages": ["scala"]
       |}""".stripMargin

  private def withWorkspace[A](body: Path => A): A =
    val ws = Files.createTempDirectory("ls-bsp-discovery")
    try body(ws)
    finally
      val stream = Files.walk(ws)
      try stream.sorted(java.util.Comparator.reverseOrder()).forEach(p => Files.deleteIfExists(p))
      finally stream.close()

  test("discovers .bsp/*.json files, sorted deterministically by server name") {
    withWorkspace { ws =>
      val bspDir = Files.createDirectories(ws.resolve(".bsp"))
      // File names sort the other way around than server names on purpose.
      Files.writeString(bspDir.resolve("a-file.json"), connectionJson("zeta-build", List("zeta", "bsp")))
      Files.writeString(bspDir.resolve("z-file.json"), connectionJson("alpha-build", List("alpha", "bsp", "--stdio")))
      Files.writeString(bspDir.resolve("notes.txt"), "not a connection file")

      val result = BspDiscovery.discover(ws)
      assertEquals(result.invalid, Vector.empty[BspError.InvalidConnectionFile])
      assertEquals(result.candidates.map(_.details.getName), Vector("alpha-build", "zeta-build"))
      assertEquals(result.candidates.map(_.path.getFileName.toString), Vector("z-file.json", "a-file.json"))

      val picked = BspDiscovery.pick(ws).get
      assertEquals(picked.details.getName, "alpha-build")
      assertEquals(picked.details.getArgv.asScala.toList, List("alpha", "bsp", "--stdio"))
      assertEquals(picked.details.getBspVersion, "2.1.1")
      assertEquals(BspDiscovery.required(ws).details.getName, "alpha-build")
    }
  }

  test("malformed and incomplete files are reported, valid ones still win") {
    withWorkspace { ws =>
      val bspDir = Files.createDirectories(ws.resolve(".bsp"))
      Files.writeString(bspDir.resolve("good.json"), connectionJson("good-build", List("good")))
      Files.writeString(bspDir.resolve("broken.json"), "{ this is not json")
      Files.writeString(bspDir.resolve("no-argv.json"), """{"name":"no-argv","argv":[]}""")
      Files.writeString(bspDir.resolve("empty.json"), "")

      val result = BspDiscovery.discover(ws)
      assertEquals(result.candidates.map(_.details.getName), Vector("good-build"))
      assertEquals(result.invalid.size, 3)
      val invalidFiles = result.invalid.map(e => Path.of(e.path).getFileName.toString).toSet
      assertEquals(invalidFiles, Set("broken.json", "no-argv.json", "empty.json"))
      assert(result.invalid.forall(_.message.startsWith("invalid BSP connection file")))
      assertEquals(result.preferred.map(_.details.getName), Some("good-build"))
    }
  }

  test("workspace without .bsp directory or valid files") {
    withWorkspace { ws =>
      assertEquals(BspDiscovery.discover(ws), BspDiscoveryResult(Vector.empty, Vector.empty))
      assertEquals(BspDiscovery.pick(ws), None)
      val ex = intercept[BspException](BspDiscovery.required(ws))
      ex.error match
        case BspError.NoConnectionFile(root) => assertEquals(root, ws.toString)
        case other => fail(s"expected NoConnectionFile, got $other")
    }
  }
