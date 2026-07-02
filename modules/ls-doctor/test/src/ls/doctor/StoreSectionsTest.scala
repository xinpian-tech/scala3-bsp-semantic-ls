package ls.doctor

import java.nio.file.Files

import DoctorTestSupport.*

/** SQLite + Postings sections gathered from a real temp MetaStore and
  * SnapshotManager with one published segment.
  */
class StoreSectionsTest extends munit.FunSuite:

  private val store = FunFixture[Store](
    setup = test => openStore(test.name.replaceAll("\\W+", "-").take(24)),
    teardown = _.close()
  )

  store.test("SqliteSection: WAL on, FTS on, manifest generation, counts"): s =>
    SqliteSection.gather(s.meta) match
      case SectionState.Unavailable(reason) => fail(s"unexpectedly unavailable: $reason")
      case SectionState.Ready(section) =>
        assert(section.walEnabled, s"journal_mode was ${section.journalMode}")
        assertEquals(section.journalMode.toLowerCase, "wal")
        assert(section.ftsEnabled, "workspace_symbols_fts probe failed")
        assertEquals(section.activeSegmentId, Some(s.manifestSegmentId))
        assertEquals(section.activeSegmentPath, Some(s.segmentDir.toString))
        assertEquals(section.documentCount, 1L)
        assertEquals(section.symbolCount, 2L)
        assertEquals(section.databasePath, s.meta.db.path)

  store.test("PostingsSection: active segment, snapshot id and counts"): s =>
    PostingsSection.gather(s.meta, s.manager) match
      case SectionState.Unavailable(reason) => fail(s"unexpectedly unavailable: $reason")
      case SectionState.Ready(section) =>
        assertEquals(section.segments.length, 1)
        assertEquals(section.activeSegments.map(_.segmentId), Vector(s.manifestSegmentId))
        assertEquals(section.activeSegments.map(_.path), Vector(s.segmentDir.toString))
        assertEquals(section.snapshotId, Some(s.segmentId))
        assertEquals(section.snapshotDocCount, Some(1))
        assertEquals(section.snapshotOccurrenceCount, Some(5L))
        assertEquals(section.compactionPending, 0)
        assertEquals(section.compactionPendingDirs, Vector.empty[String])

  store.test("PostingsSection: superseded-but-undeleted dir counts as compaction pending"): s =>
    val leftover = s.manager.segmentsDir.resolve("segment-000099")
    Files.createDirectories(leftover)
    PostingsSection.gather(s.meta, s.manager) match
      case SectionState.Unavailable(reason) => fail(s"unexpectedly unavailable: $reason")
      case SectionState.Ready(section) =>
        assert(section.compactionPending > 0, "expected pending compaction work")
        assertEquals(section.compactionPending, 1)
        assertEquals(section.compactionPendingDirs, Vector(leftover.toString))
        // the active segment itself is never counted as pending
        assert(!section.compactionPendingDirs.contains(s.segmentDir.toString))

  store.test("SemanticdbSection: root existence and file counts via the locator"): s =>
    // fake targetroot with two .semanticdb files
    val targetroot = s.root.resolve("sdb")
    val sdbDir = targetroot.resolve("META-INF/semanticdb/src/a")
    Files.createDirectories(sdbDir)
    Files.write(sdbDir.resolve("A.scala.semanticdb"), Array[Byte](1, 2, 3))
    Files.write(sdbDir.resolve("B.scala.semanticdb"), Array[Byte](4, 5))
    val missingRoot = s.root.resolve("no-such-targetroot")
    val stats = DocFreshnessStats.of(fresh = 1, stale = 1, missing = 0, uris = Vector("src/a/B.scala"))
    val targets = Vector(
      SemanticdbSection.TargetRoot("bsp://ws/a", targetroot),
      SemanticdbSection.TargetRoot("bsp://ws/b", missingRoot)
    )
    SemanticdbSection.gather(targets, Some(stats)) match
      case SectionState.Unavailable(reason) => fail(s"unexpectedly unavailable: $reason")
      case SectionState.Ready(section) =>
        assertEquals(section.roots.length, 2)
        val a = section.roots.find(_.bspId == "bsp://ws/a").get
        assert(a.exists)
        assertEquals(a.semanticdbFileCount, 2)
        assertEquals(a.semanticdbRoot, targetroot.resolve("META-INF/semanticdb").toString)
        val b = section.roots.find(_.bspId == "bsp://ws/b").get
        assert(!b.exists)
        assertEquals(b.semanticdbFileCount, 0)
        assertEquals(section.freshness, Some(stats))

  test("DocFreshnessStats.of caps the uri list at 20"):
    val stats = DocFreshnessStats.of(0, 30, 0, Vector.tabulate(30)(i => s"src/F$i.scala"))
    assertEquals(stats.uris.length, DocFreshnessStats.UriCap)
    assertEquals(stats.stale, 30)
