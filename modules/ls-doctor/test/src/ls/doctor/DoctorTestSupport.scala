package ls.doctor

import java.nio.file.{Files, Path}

import ls.index.{OccFlags, RenameProfile, Span}
import ls.postings.*
import ls.sqlite.{MetaStore, SymbolInternRow}

/** Shared fixtures: a real temp MetaStore + SnapshotManager with one tiny
  * published segment (mirroring the plan-9.2 ingest steps by hand).
  */
object DoctorTestSupport:

  final case class Store(
      root: Path,
      meta: MetaStore,
      manager: SnapshotManager,
      segmentId: Long,
      manifestSegmentId: Long,
      segmentDir: Path
  ):
    def close(): Unit =
      manager.close()
      meta.close()

  def tempRoot(prefix: String): Path =
    val p = Files.createTempDirectory(s"ls-doctor-$prefix-")
    p.toFile.deleteOnExit()
    p

  /** Minimal segment: 1 doc, 1 target, 2 symbols, 1 ref group with one
    * reference + one definition, 1 rename group with one editable edit, and
    * two doc postings. occurrenceCount = 5.
    */
  def tinySegmentData: SegmentData =
    SegmentData(
      docs = Vector(
        SegmentDoc("src/a/A.scala", docId = 1L, epoch = 1, targetOrd = 0)
      ),
      targets = Vector(11L),
      symbols = Vector(
        SegmentSymbol("a/A#", symbolId = 100L, refGroupOrd = 0, renameGroupOrd = 0, defTargetOrd = 0),
        SegmentSymbol("a/A#foo().", symbolId = 101L, refGroupOrd = 0, renameGroupOrd = -1, defTargetOrd = 0)
      ),
      refOccurrences = Vector(
        Vector(GroupOcc(0, 1, 0, Span(3, 4, 3, 7), 0))
      ),
      defOccurrences = Vector(
        Vector(GroupOcc(0, 1, 0, Span(0, 6, 0, 9), OccFlags.Definition))
      ),
      renameOccurrences = Vector(
        Vector(GroupOcc(0, 1, 0, Span(3, 4, 3, 7), OccFlags.Editable))
      ),
      renameProfiles = Vector(
        RenameProfile(
          isLocal = false,
          isExternal = false,
          hasGeneratedOccurrences = false,
          hasReadonlyOccurrences = false,
          hasOverrideFamily = false,
          hasCompanion = false,
          editableOccurrenceCount = 1,
          unsafeReasonMask = 0L
        )
      ),
      docOccurrences = Vector(
        Vector(
          DocOcc(0, Span(0, 6, 0, 9), OccFlags.Definition),
          DocOcc(1, Span(3, 4, 3, 7), 0)
        )
      )
    )

  /** Opens a real MetaStore + SnapshotManager under a temp root, writes the
    * tiny segment, registers + activates it in the manifest, publishes the
    * snapshot, and fills targets/documents/symbol_intern.
    */
  def openStore(prefix: String): Store =
    val root = tempRoot(prefix)
    val meta = MetaStore.open(root.resolve("meta.sqlite"))
    val manager = new SnapshotManager(root.resolve("postings"))
    val segmentId = manager.nextSegmentId()
    val segmentDir = SegmentWriter.write(manager.root, segmentId, tinySegmentData, createdAtMs = 42L)
    val manifestId = meta.db.withWriteTransaction {
      val id = meta.insertSegment(
        path = segmentDir.toString,
        createdAtMs = 42L,
        minEpoch = 1L,
        maxEpoch = 1L,
        checksum = 7L
      )
      meta.activateSegment(id)
      id
    }
    manager.publish(SegmentReader.open(segmentDir))

    val targetId = meta.upsertTarget(
      bspId = "bsp://ws/a",
      scalaVersion = "3.8.4",
      classpathHash = "ch",
      optionsHash = "oh",
      semanticdbRoot = root.resolve("sdb").toString,
      sourceroot = root.resolve("src").toString,
      active = true
    )
    meta.upsertDocument(
      targetId = targetId,
      uri = "src/a/A.scala",
      semanticdbPath = root.resolve("sdb/META-INF/semanticdb/src/a/A.scala.semanticdb").toString,
      semanticdbMtimeMs = 1L,
      md5 = "00112233445566778899aabbccddeeff",
      generated = false,
      readonly = false
    )
    meta.internSymbols(
      Seq(
        SymbolInternRow(0L, "a/A#", None, 1L),
        SymbolInternRow(0L, "a/A#foo().", None, 2L)
      )
    )
    Store(root, meta, manager, segmentId, manifestId, segmentDir)

  /** Walks up from `start` until a directory containing flake.nix is found:
    * mill forks tests inside the repo's out/ tree, so this finds the real
    * repo root without hardcoding it.
    */
  def findRepoRoot(start: Path = Path.of("").toAbsolutePath): Option[Path] =
    Iterator
      .iterate(start.normalize)(p => Option(p.getParent).orNull)
      .takeWhile(_ != null)
      .find(p => Files.isRegularFile(p.resolve("flake.nix")))
