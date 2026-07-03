package ls.doctor

import java.nio.file.Files

import ls.postings.CurrentSnapshotFile
import ls.semanticdb.Md5

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
        assertEquals(section.walSizeBytes, s.meta.walSizeBytes)
        assert(section.walSizeBytes >= 0L, s"wal size was ${section.walSizeBytes}")

  store.test("generated-source count and per-target staleness base case: none generated, none stale"): s =>
    // base fixture: the only doc is non-generated and its source is absent
    // (resolves to a missing path), so it is neither generated nor stale.
    assertEquals(s.meta.generatedDocumentCount(), 0L)
    assertEquals(SemanticdbSection.staleTargets(s.meta.activeDocumentDigests()), Vector.empty[String])

  store.test("generated-source count and per-target staleness from documents"): s =>
    // A second target with a real sourceroot on disk under the store root.
    val sourceroot = s.root.resolve("srcB")
    val targetB = s.meta.upsertTarget(
      bspId = "bsp://ws/b",
      scalaVersion = "3.8.4",
      classpathHash = "chB",
      optionsHash = "ohB",
      semanticdbRoot = s.root.resolve("sdbB").toString,
      sourceroot = sourceroot.toString,
      active = true
    )

    // Stale doc: the source file on disk differs from the stored md5.
    val staleRel = "pkg/B.scala"
    val staleFile = sourceroot.resolve(staleRel)
    Files.createDirectories(staleFile.getParent)
    Files.writeString(staleFile, "object B // current content on disk")
    s.meta.upsertDocument(
      targetId = targetB,
      uri = staleRel,
      semanticdbPath = s.root.resolve("sdbB/META-INF/semanticdb/pkg/B.scala.semanticdb").toString,
      semanticdbMtimeMs = 1L,
      md5 = Md5.computeHex("object B // an OLDER version that was indexed"),
      generated = false,
      readonly = false
    )

    // Generated doc: flagged generated, and fresh (source md5 matches stored).
    val genRel = "pkg/G.scala"
    val genFile = sourceroot.resolve(genRel)
    val genText = "object G // generated"
    Files.createDirectories(genFile.getParent)
    Files.writeString(genFile, genText)
    s.meta.upsertDocument(
      targetId = targetB,
      uri = genRel,
      semanticdbPath = s.root.resolve("sdbB/META-INF/semanticdb/pkg/G.scala.semanticdb").toString,
      semanticdbMtimeMs = 1L,
      md5 = Md5.computeHex(genText),
      generated = true,
      readonly = false
    )

    // exactly the one generated doc (G); the base doc and the stale doc are not generated
    assertEquals(s.meta.generatedDocumentCount(), 1L)
    // exactly the one target with a stale-md5 doc; the generated doc is fresh and the base
    // target's source is absent (missing, not stale)
    assertEquals(SemanticdbSection.staleTargets(s.meta.activeDocumentDigests()), Vector("bsp://ws/b"))

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
        // publish wrote snapshots/current.json naming the active segment
        assertEquals(section.snapshotFile, SnapshotFileStatus.Consistent)

  store.test("PostingsSection: a current.json that diverges from the manifest is reported, not trusted"): s =>
    // tamper current.json to name a different segment path than the manifest active one
    CurrentSnapshotFile.writeAtomic(
      s.manager.root,
      CurrentSnapshotFile(
        segmentId = 999L,
        path = s.manager.segmentsDir.resolve("segment-999999").toString,
        publishedAtMs = 1L,
        generation = 99L
      )
    )
    PostingsSection.gather(s.meta, s.manager) match
      case SectionState.Unavailable(reason) => fail(s"unexpectedly unavailable: $reason")
      case SectionState.Ready(section) =>
        assertEquals(section.snapshotFile, SnapshotFileStatus.Divergent)

  store.test("PostingsSection: a missing current.json is reported as missing"): s =>
    Files.deleteIfExists(CurrentSnapshotFile.pathIn(s.manager.root))
    PostingsSection.gather(s.meta, s.manager) match
      case SectionState.Unavailable(reason) => fail(s"unexpectedly unavailable: $reason")
      case SectionState.Ready(section) =>
        assertEquals(section.snapshotFile, SnapshotFileStatus.Missing)

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
