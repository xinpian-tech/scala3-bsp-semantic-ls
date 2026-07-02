package ls.core

import java.nio.file.{Files, Path}

import ls.postings.SnapshotManager
import ls.rename.QueryOrchestrator
import ls.rename.ingest.IngestPipeline
import ls.sqlite.MetaStore

class UrisSuite extends munit.FunSuite:

  test("path -> uri -> path round-trips"):
    val path = Path.of("/tmp/ls-core/some dir/File.scala")
    val uri = Uris.toUri(path)
    assert(uri.startsWith("file:///"), uri)
    assertEquals(Uris.toPath(uri), path)

  test("uri with percent-encoding round-trips through normalize"):
    val encoded = "file:///tmp/ls-core/some%20dir/File.scala"
    val normalized = Uris.normalize(encoded)
    assertEquals(Uris.toPath(normalized), Path.of("/tmp/ls-core/some dir/File.scala"))

  test("normalize collapses equivalent spellings to one key"):
    val canonical = Uris.normalize("file:///tmp/ls-core/a/B.scala")
    assertEquals(Uris.normalize("file:/tmp/ls-core/a/B.scala"), canonical)
    assertEquals(Uris.normalize("file:///tmp/ls-core/a/../a/B.scala"), canonical)

  test("normalize passes through non-file uris unchanged"):
    assertEquals(Uris.normalize("untitled:Untitled-1"), "untitled:Untitled-1")

  test("sdbUri relativizes under the sourceroot with forward slashes"):
    val root = Path.of("/tmp/ws")
    assertEquals(Uris.sdbUri(root, Path.of("/tmp/ws/a/src/Main.scala")), Some("a/src/Main.scala"))
    assertEquals(Uris.sdbUri(root, Path.of("/tmp/ws")), None)
    assertEquals(Uris.sdbUri(root, Path.of("/tmp/other/Main.scala")), None)

  test("sdbUri <-> fromSdbUri round-trips"):
    val root = Path.of("/tmp/ws")
    val abs = Path.of("/tmp/ws/x/y/Z.scala")
    val sdb = Uris.sdbUri(root, abs).get
    assertEquals(Uris.fromSdbUri(root, sdb), abs)

  test("WorkspaceUris resolves via the deepest matching sourceroot and disk fallback"):
    val dir = Files.createTempDirectory("ls-core-uris")
    val inner = dir.resolve("inner")
    Files.createDirectories(inner.resolve("src"))
    val file = inner.resolve("src").resolve("A.scala")
    Files.writeString(file, "class A\n")

    val storeDir = Files.createTempDirectory("ls-core-uris-store")
    val meta = MetaStore.open(storeDir.resolve("meta.sqlite"))
    try
      val manager = SnapshotManager(storeDir.resolve("postings"))
      val orchestrator = QueryOrchestrator(meta, manager, IngestPipeline(meta, manager))
      val uris = WorkspaceUris(Vector(dir, inner), orchestrator)

      // deepest sourceroot (inner) wins for the sdb-uri direction
      assertEquals(uris.toSdbUri(Uris.toUri(file)), Some("src/A.scala"))
      // reverse direction falls back to probing sourceroots for the file
      assertEquals(uris.toFileUri("src/A.scala"), Some(Uris.toUri(file)))
      assertEquals(uris.toFileUri("src/Missing.scala"), None)
      assertEquals(uris.toSdbUri("not-a-uri"), None)
    finally meta.close()
