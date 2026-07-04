package ls.postings

import java.nio.file.{Files, Path}

import ls.index.*
import TestSupport.*

/** `snapshots/current.json`: publish writes it atomically with the segment
  * identity, a positive publish timestamp, and a monotonic generation that
  * survives across managers; render/parse round-trips.
  */
class CurrentSnapshotFileSuite extends munit.FunSuite:

  private def genData(gen: Int): SegmentData =
    SegmentData(
      docs = Vector(SegmentDoc("file:///gen/Doc.scala", docId = 1, epoch = gen, targetOrd = 0)),
      targets = Vector(77L),
      symbols = Vector(SegmentSymbol(s"gen/Sym$gen.", symbolId = gen.toLong, refGroupOrd = 0)),
      refOccurrences = Vector(Vector(GroupOcc(0, gen, 0, Span(1, 0, 1, 3 + gen), 0))),
      defOccurrences = Vector(Vector.empty),
      renameOccurrences = Vector.empty,
      renameProfiles = Vector.empty,
      docOccurrences = Vector(Vector(DocOcc(0, Span(1, 0, 1, 3 + gen), 0)))
    )

  private def publish(manager: SnapshotManager, gen: Int): (Long, Path) =
    val segId = manager.nextSegmentId()
    val dir = SegmentWriter.write(manager.root, segId, genData(gen))
    manager.publish(SegmentReader.open(dir))
    (segId, dir)

  test("publish writes current.json atomically with the segment identity and a positive timestamp"):
    val manager = SnapshotManager(tempRoot("current-json"))
    try
      val (segId, dir) = publish(manager, 1)
      val file = CurrentSnapshotFile.pathIn(manager.root)
      assert(Files.isRegularFile(file), s"expected $file to exist")
      // the atomic move leaves no partial temp file behind
      assert(!Files.exists(manager.root.resolve("snapshots").resolve("current.json.tmp")))
      val cf = manager.readCurrentFile().getOrElse(fail("current.json did not parse"))
      assertEquals(cf.segmentId, segId)
      assertEquals(cf.path, dir.toString)
      assert(cf.publishedAtMs > 0L, s"publishedAtMs was ${cf.publishedAtMs}")
      assertEquals(cf.generation, 1L)
    finally manager.close()

  test("each publish increments the generation and rewrites current.json"):
    val manager = SnapshotManager(tempRoot("current-json-gen"))
    try
      publish(manager, 1)
      assertEquals(manager.readCurrentFile().map(_.generation), Some(1L))
      val (segId2, dir2) = publish(manager, 2)
      val cf = manager.readCurrentFile().getOrElse(fail("current.json did not parse"))
      assertEquals(cf.generation, 2L)
      assertEquals(cf.segmentId, segId2)
      assertEquals(cf.path, dir2.toString)
    finally manager.close()

  test("a new manager seeds its generation from an existing current.json"):
    val root = tempRoot("current-json-seed")
    val m1 = SnapshotManager(root)
    publish(m1, 1)
    publish(m1, 2)
    m1.close()
    assertEquals(CurrentSnapshotFile.read(root).map(_.generation), Some(2L))
    // a fresh manager over the same root continues the generation, not restart at 1
    val m2 = SnapshotManager(root)
    try
      publish(m2, 3)
      assertEquals(m2.readCurrentFile().map(_.generation), Some(3L))
    finally m2.close()

  test("writeAtomic replaces an existing current.json (rewrite, not FileAlreadyExists)"):
    // Rewriting over an existing current.json must REPLACE it, not fail. On this
    // Linux FS ATOMIC_MOVE already replaces via rename(2), so this documents the
    // rewrite contract that ATOMIC_MOVE + REPLACE_EXISTING guarantees on every
    // provider (the integration path is covered by the publish-twice test above).
    val root = tempRoot("current-json-replace")
    CurrentSnapshotFile.writeAtomic(root, CurrentSnapshotFile(1L, "/seg/1", 100L, 1L))
    CurrentSnapshotFile.writeAtomic(root, CurrentSnapshotFile(2L, "/seg/2", 200L, 2L))
    assertEquals(CurrentSnapshotFile.read(root), Some(CurrentSnapshotFile(2L, "/seg/2", 200L, 2L)))
    assert(!Files.exists(root.resolve("snapshots").resolve("current.json.tmp")))

  test("render/parse round-trips, including a path with spaces"):
    val f = CurrentSnapshotFile(7L, "/tmp/some dir/segment-000007", 123L, 4L)
    val root = tempRoot("current-json-roundtrip")
    CurrentSnapshotFile.writeAtomic(root, f)
    assertEquals(CurrentSnapshotFile.read(root), Some(f))

  test("read returns None when the file is absent"):
    assertEquals(CurrentSnapshotFile.read(tempRoot("current-json-absent")), None)

  test("current.json is written under the currentFileRoot, not the postings segment root"):
    val storage = tempRoot("current-json-loc")
    val postingsRoot = storage.resolve("postings")
    // segments under storage/postings; current.json a sibling at the storage root
    val manager = SnapshotManager(postingsRoot, storage)
    try
      publish(manager, 1)
      assert(
        Files.isRegularFile(storage.resolve("snapshots").resolve("current.json")),
        "current.json must be written at the storage root (sibling of postings/)"
      )
      assert(
        !Files.exists(postingsRoot.resolve("snapshots").resolve("current.json")),
        "current.json must NOT be written under the postings segment root"
      )
      assertEquals(manager.readCurrentFile().map(_.generation), Some(1L))
    finally manager.close()
