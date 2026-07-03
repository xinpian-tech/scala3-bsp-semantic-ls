package ls.postings

import java.nio.file.{Files, Path}

/** Startup janitor: `cleanupOrphans` removes writer debris and non-active
  * segment directories while never deleting the manifest-active segment.
  */
class SnapshotJanitorTest extends munit.FunSuite:

  private def newRoot(): Path =
    val root = Files.createTempDirectory("ls-janitor-")
    root.toFile.deleteOnExit()
    root

  private def mkdir(p: Path): Path =
    Files.createDirectories(p)
    p

  test("removes tmp debris and non-active segment dirs, keeping the active one"):
    val root = newRoot()
    val manager = SnapshotManager(root)
    val seg1 = mkdir(manager.segmentsDir.resolve("segment-000001"))
    val active = mkdir(manager.segmentsDir.resolve("segment-000002"))
    Files.write(active.resolve("header.bin"), Array[Byte](1, 2, 3))
    val tmp = mkdir(root.resolve("tmp-scratch"))

    val deleted = manager.cleanupOrphans(Some(active))

    assert(Files.isDirectory(active), "active segment kept")
    assert(Files.isRegularFile(active.resolve("header.bin")), "active contents kept")
    assert(!Files.exists(seg1), "orphan segment removed")
    assert(!Files.exists(tmp), "tmp debris removed")
    assertEquals(deleted.toSet, Set(seg1, tmp))

  test("with no active segment removes every segment dir and tmp debris"):
    val root = newRoot()
    val manager = SnapshotManager(root)
    val seg1 = mkdir(manager.segmentsDir.resolve("segment-000001"))
    val seg2 = mkdir(manager.segmentsDir.resolve("segment-000002"))
    val tmp = mkdir(root.resolve("tmp-a"))

    val deleted = manager.cleanupOrphans(None)

    assert(!Files.exists(seg1) && !Files.exists(seg2) && !Files.exists(tmp))
    assertEquals(deleted.toSet, Set(seg1, seg2, tmp))

  test("protects the active path even when the manifest points at a missing dir (divergence)"):
    val root = newRoot()
    val manager = SnapshotManager(root)
    val orphan = mkdir(manager.segmentsDir.resolve("segment-000001"))
    // The manifest-active path does not exist on disk (recovery would have failed).
    val missingActive = manager.segmentsDir.resolve("segment-000009")

    val deleted = manager.cleanupOrphans(Some(missingActive))

    assert(!Files.exists(orphan), "orphan removed")
    assert(!Files.exists(missingActive), "missing active is simply absent, not created")
    assertEquals(deleted, List(orphan))

  test("no-op on an empty postings root"):
    val root = newRoot()
    val manager = SnapshotManager(root)
    assertEquals(manager.cleanupOrphans(None), Nil)
