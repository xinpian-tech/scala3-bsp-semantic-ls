package ls.core

import java.nio.file.Files

import ls.doctor.Doctor
import ls.index.Span
import ls.postings.*
import ls.sqlite.MetaStore

/** Startup recovery preserves a divergent `snapshots/current.json`: recovery
  * re-mmaps the manifest-active segment but must NOT rewrite the on-disk file,
  * so the doctor reports `snapshot file: divergent` rather than silently
  * healing it. Also pins the production storage location (the file lives at the
  * storage root, a sibling of `postings/`).
  */
class BootstrapRecoverySuite extends munit.FunSuite:

  private def minimalSegment: SegmentData =
    SegmentData(
      docs = Vector(SegmentDoc("file:///ws/A.scala", docId = 1, epoch = 1, targetOrd = 0)),
      targets = Vector(11L),
      symbols = Vector(SegmentSymbol("ws/A#", symbolId = 1L, refGroupOrd = 0)),
      refOccurrences = Vector(Vector(GroupOcc(0, 1, 0, Span(0, 0, 0, 1), 0))),
      defOccurrences = Vector(Vector.empty),
      renameOccurrences = Vector.empty,
      renameProfiles = Vector.empty,
      docOccurrences = Vector(Vector(DocOcc(0, Span(0, 0, 0, 1), 0)))
    )

  private def deleteRecursively(p: java.nio.file.Path): Unit =
    if Files.exists(p) then
      val stream = Files.walk(p)
      try stream.sorted(java.util.Comparator.reverseOrder()).forEach(q => Files.delete(q))
      finally stream.close()

  test("a manifest pointing at a deleted segment degrades gracefully and heals on the next ingest"):
    val ws = Files.createTempDirectory("ls-boot-missing-seg-")
    ws.toFile.deleteOnExit()
    val storage = Bootstrap.storageRootOf(ws)
    Files.createDirectories(storage)
    val postingsRoot = storage.resolve("postings")

    // persist a store whose manifest activates a segment, then delete its dir
    val segDir = SegmentWriter.write(postingsRoot, 1L, minimalSegment)
    val meta = MetaStore.open(storage.resolve("meta.sqlite"))
    meta.db.withWriteTransaction {
      val id = meta.insertSegment(segDir.toString, createdAtMs = 1L, minEpoch = 1L, maxEpoch = 1L, checksum = 0L)
      meta.activateSegment(id)
    }
    meta.close()
    deleteRecursively(segDir)
    assert(!Files.exists(segDir), "the active segment dir must be gone before boot")

    // boot: graceful degrade (Ready, not a hard crash) with a recovery note
    locally:
      val docs = new DocumentStore
      val overlay = new PcOverlay(docs)
      val state = Bootstrap.run(ws, Bootstrap.Config(connectBsp = (_, _) => None, log = _ => ()), docs, overlay)
      try
        val services =
          state.ready.getOrElse(fail(s"boot must degrade to Ready, got: ${state.statusLine}"))
        assert(
          services.notes.exists(_.contains("could not be recovered")),
          services.notes.mkString("\n")
        )
        // the doctor still renders without crashing
        assert(Doctor.render(DoctorCommand.input(services)).nonEmpty)
      finally state.ready.foreach(_.close())

    // heal: a fresh segment is written + activated (as the next ingest would);
    // re-boot recovers it cleanly
    val meta2 = MetaStore.open(storage.resolve("meta.sqlite"))
    val freshDir = SegmentWriter.write(postingsRoot, 2L, minimalSegment)
    meta2.db.withWriteTransaction {
      val id = meta2.insertSegment(freshDir.toString, createdAtMs = 2L, minEpoch = 1L, maxEpoch = 1L, checksum = 0L)
      meta2.activateSegment(id)
    }
    meta2.close()

    locally:
      val docs = new DocumentStore
      val overlay = new PcOverlay(docs)
      val healed = Bootstrap.run(ws, Bootstrap.Config(connectBsp = (_, _) => None, log = _ => ()), docs, overlay)
      try
        val services = healed.ready.getOrElse(fail(healed.statusLine))
        assert(
          services.notes.exists(_.contains("recovered postings segment")),
          services.notes.mkString("\n")
        )
      finally healed.ready.foreach(_.close())

  test("startup recovery preserves a divergent current.json and the doctor reports it divergent"):
    val ws = Files.createTempDirectory("ls-boot-recovery-")
    ws.toFile.deleteOnExit()
    val storage = Bootstrap.storageRootOf(ws)
    Files.createDirectories(storage)

    // a real segment under storage/postings, activated in the SQLite manifest
    val postingsRoot = storage.resolve("postings")
    val segDir = SegmentWriter.write(postingsRoot, 1L, minimalSegment)
    val meta = MetaStore.open(storage.resolve("meta.sqlite"))
    meta.db.withWriteTransaction {
      val id = meta.insertSegment(
        path = segDir.toString,
        createdAtMs = 1L,
        minEpoch = 1L,
        maxEpoch = 1L,
        checksum = 0L
      )
      meta.activateSegment(id)
    }
    meta.close()

    // a DIVERGENT current.json at the STORAGE root (sibling of postings/),
    // naming a different segment than the manifest-active one
    val divergentPath = postingsRoot.resolve("segments").resolve("segment-999999").toString
    CurrentSnapshotFile.writeAtomic(storage, CurrentSnapshotFile(999L, divergentPath, 1L, 42L))

    val docs = new DocumentStore
    val overlay = new PcOverlay(docs)
    val state = Bootstrap.run(ws, Bootstrap.Config(connectBsp = (_, _) => None, log = _ => ()), docs, overlay)
    try
      val services = state.ready.getOrElse(fail(state.statusLine))
      // recovery installed the active segment but did NOT overwrite current.json,
      // so the doctor still sees the divergence and reports it
      val report = Doctor.render(DoctorCommand.input(services))
      assert(report.contains("snapshot file: divergent"), s"expected divergence in:\n$report")
      // the divergent file survived recovery unchanged, at the storage root
      val cf = CurrentSnapshotFile
        .read(storage)
        .getOrElse(fail("current.json must exist at the storage root and survive recovery"))
      assertEquals(cf.path, divergentPath, "recovery must NOT overwrite the divergent current.json")
      // and it was never written under the postings root
      assert(
        !Files.exists(postingsRoot.resolve("snapshots").resolve("current.json")),
        "current.json must not live under the postings segment root"
      )
    finally state.ready.foreach(_.close())
