package ls.doctor

import java.nio.file.{Files, Path}

import scala.jdk.CollectionConverters.*
import scala.util.control.NonFatal

import ch.epfl.scala.bsp4j.InitializeBuildResult

import ls.bsp.BspProjectModel
import ls.pc.{CompilerPluginStatus, DisabledPlugin, PcPluginStatusReport, ServicePluginStatus}
import ls.postings.SnapshotManager
import ls.semanticdb.{Md5, SemanticdbLocator}
import ls.sqlite.{ActiveDocumentDigest, MetaStore}

/** Availability wrapper for one doctor section: a section that cannot be
  * gathered (subsystem not connected, resource threw) renders as
  * `unavailable: <reason>` instead of failing the whole report.
  */
enum SectionState[+A]:
  case Ready(value: A)
  case Unavailable(reason: String)

  def toOption: Option[A] = this match
    case Ready(v) => Some(v)
    case Unavailable(_) => None

  def fold[B](ifUnavailable: String => B)(ifReady: A => B): B = this match
    case Ready(v) => ifReady(v)
    case Unavailable(reason) => ifUnavailable(reason)

object SectionState:
  /** Runs a gather body, turning any non-fatal throwable into Unavailable. */
  def attempt[A](what: String)(body: => A): SectionState[A] =
    try Ready(body)
    catch case NonFatal(t) => Unavailable(s"$what gathering failed: ${Gather.describe(t)}")

private[doctor] object Gather:
  def describe(t: Throwable): String =
    val cls = t.getClass.getSimpleName
    Option(t.getMessage).filter(_.nonEmpty).map(m => s"$cls: $m").getOrElse(cls)

/** Precomputed document freshness statistics for the SemanticDB section.
  *
  * The doctor accepts these precomputed (from ingest reports / orchestrator
  * state) so gathering stays cheap: it never re-hashes sources. `stale` means
  * md5 mismatch between the source file and its SemanticDB `TextDocument`;
  * `missing` means no `.semanticdb` file for a known source. `uris` lists the
  * affected documents, capped at [[DocFreshnessStats.UriCap]].
  */
final case class DocFreshnessStats(
    fresh: Int,
    stale: Int,
    missing: Int,
    uris: Vector[String]
)

object DocFreshnessStats:
  val UriCap = 20

  /** Builds stats with the uri list capped at [[UriCap]]. */
  def of(fresh: Int, stale: Int, missing: Int, uris: Vector[String]): DocFreshnessStats =
    DocFreshnessStats(fresh, stale, missing, uris.take(UriCap))

/** BSP section (plan 19): server identity, target counts and the
  * IndexUnavailable target list (plan 4.2).
  */
final case class BspSection(
    serverName: Option[String],
    serverVersion: Option[String],
    targetCount: Int,
    /** bspIds of the Scala 3 targets in the project model, sorted. */
    scala3Targets: Vector[String],
    /** bspIds of targets without SemanticDB output, sorted. */
    indexUnavailableTargets: Vector[String]
)

object BspSection:
  /** Gathers the BSP section from an assembled project model plus (when the
    * session already initialized) the build/initialize result.
    */
  def gather(model: BspProjectModel, serverInfo: Option[InitializeBuildResult]): SectionState[BspSection] =
    SectionState.attempt("BSP"):
      BspSection(
        serverName = serverInfo.flatMap(r => Option(r.getDisplayName)).filter(_.nonEmpty),
        serverVersion = serverInfo.flatMap(r => Option(r.getVersion)).filter(_.nonEmpty),
        targetCount = model.targets.length,
        scala3Targets = model.targets.map(_.bspId).sorted,
        indexUnavailableTargets = model.unavailableTargets.map(_.bspId).sorted
      )

/** One SemanticDB root as seen on disk: the `META-INF/semanticdb` directory
  * under a target's targetroot plus its `.semanticdb` file count.
  */
final case class SemanticdbRootStatus(
    bspId: String,
    semanticdbRoot: String,
    exists: Boolean,
    semanticdbFileCount: Int
)

/** SemanticDB doctor section: roots with existence and file counts, plus
  * (when precomputed) document freshness statistics, plus the store-derived
  * generated-source count and per-target staleness (both reported in this
  * SemanticDB area of the report).
  */
final case class SemanticdbSection(
    roots: Vector[SemanticdbRootStatus],
    freshness: Option[DocFreshnessStats],
    /** Active documents flagged `generated = 1` (from `documents.generated`). */
    generatedSourceCount: Long,
    /** bspIds (sorted, distinct) of targets owning ≥1 stale doc. */
    staleTargets: Vector[String]
)

object SemanticdbSection:
  /** Input row for [[gather]]: an indexable target and its targetroot (the
    * directory that contains `META-INF/semanticdb`).
    */
  final case class TargetRoot(bspId: String, targetroot: Path)

  /** Gathers root existence + `.semanticdb` file counts via
    * [[SemanticdbLocator]]; `stats` (when provided) comes precomputed from
    * the ingest layer so gathering stays cheap. `generatedSourceCount` and
    * `staleTargets` are supplied by the caller (ls-core, from the MetaStore).
    */
  def gather(
      targets: Vector[TargetRoot],
      stats: Option[DocFreshnessStats],
      generatedSourceCount: Long = 0L,
      staleTargets: Vector[String] = Vector.empty
  ): SectionState[SemanticdbSection] =
    SectionState.attempt("SemanticDB")(build(targets, stats, generatedSourceCount, staleTargets))

  /** Derives the [[TargetRoot]]s from a BSP project model (indexable targets
    * only) and gathers with caller-supplied generated/staleness values.
    */
  def fromModel(
      model: BspProjectModel,
      stats: Option[DocFreshnessStats],
      generatedSourceCount: Long,
      staleTargets: Vector[String]
  ): SectionState[SemanticdbSection] =
    SectionState.attempt("SemanticDB"):
      build(rootsOf(model), stats, generatedSourceCount, staleTargets)

  /** Production gather: reads the generated-source count and per-target
    * staleness from the MetaStore INSIDE the SemanticDB failure boundary, so a
    * store failure degrades this section to `unavailable` rather than crashing
    * the whole doctor report. This is the wiring `DoctorCommand` uses.
    */
  def fromModel(
      model: BspProjectModel,
      stats: Option[DocFreshnessStats],
      meta: MetaStore
  ): SectionState[SemanticdbSection] =
    SectionState.attempt("SemanticDB"):
      build(rootsOf(model), stats, meta.generatedDocumentCount(), staleTargets(meta.activeDocumentDigests()))

  private def rootsOf(model: BspProjectModel): Vector[TargetRoot] =
    model.indexableTargets.flatMap(t => t.semanticdbRoot.map(root => TargetRoot(t.bspId, root)))

  /** bspIds (sorted, distinct) of targets with at least one active document
    * whose source file exists but no longer matches the stored md5. The md5
    * comparison lives here (ls-doctor depends on ls-semanticdb) so the SQLite
    * store never re-hashes sources. Per-document I/O errors and missing sources
    * count as not-stale — a missing source is a separate condition, not md5
    * staleness. Never throws.
    */
  def staleTargets(digests: Vector[ActiveDocumentDigest]): Vector[String] =
    digests.iterator
      .filter(isStale)
      .map(_.bspId)
      .toVector
      .distinct
      .sorted

  private def isStale(d: ActiveDocumentDigest): Boolean =
    try
      val source = Path.of(d.sourceroot).resolve(d.uri)
      Files.isRegularFile(source) && !Md5.validate(Files.readString(source), d.md5).isFresh
    catch case NonFatal(_) => false

  private def build(
      targets: Vector[TargetRoot],
      stats: Option[DocFreshnessStats],
      generatedSourceCount: Long,
      staleTargets: Vector[String]
  ): SemanticdbSection =
    val roots = targets.sortBy(_.bspId).map { t =>
      try
        val locator = SemanticdbLocator(t.targetroot)
        val exists = Files.isDirectory(locator.semanticdbRoot)
        SemanticdbRootStatus(
          bspId = t.bspId,
          semanticdbRoot = locator.semanticdbRoot.toString,
          exists = exists,
          semanticdbFileCount = if exists then locator.listSemanticdbFiles().length else 0
        )
      catch
        case NonFatal(_) =>
          SemanticdbRootStatus(t.bspId, t.targetroot.toString, exists = false, semanticdbFileCount = 0)
    }
    SemanticdbSection(roots, stats, generatedSourceCount, staleTargets)

/** SQLite section (plan 19): WAL + FTS status, manifest generation, counts. */
final case class SqliteSection(
    databasePath: String,
    walEnabled: Boolean,
    journalMode: String,
    ftsEnabled: Boolean,
    /** Manifest generation: the single active segment_manifest row. */
    activeSegmentId: Option[Long],
    activeSegmentPath: Option[String],
    /** Active rows in `documents`. */
    documentCount: Long,
    /** Rows in `symbol_intern`. */
    symbolCount: Long,
    /** Size of the `-wal` sidecar file in bytes. */
    walSizeBytes: Long
)

object SqliteSection:
  /** Gathers from a live [[MetaStore]]. Caller must respect the Db threading
    * contract (single-threaded use of this connection during the gather).
    */
  def gather(meta: MetaStore): SectionState[SqliteSection] =
    SectionState.attempt("SQLite"):
      val db = meta.db
      val journalMode =
        db.prepare("PRAGMA journal_mode").queryOne(_.columnText(0)).getOrElse("unknown")
      val ftsEnabled = db
        .prepare("SELECT count(*) FROM sqlite_master WHERE name = 'workspace_symbols_fts'")
        .queryOne(_.columnLong(0))
        .exists(_ > 0)
      val active = meta.activeSegment()
      val documentCount = db
        .prepare("SELECT count(*) FROM documents WHERE active = 1")
        .queryOne(_.columnLong(0))
        .getOrElse(0L)
      SqliteSection(
        databasePath = db.path,
        walEnabled = journalMode.equalsIgnoreCase("wal"),
        journalMode = journalMode,
        ftsEnabled = ftsEnabled,
        activeSegmentId = active.map(_.segmentId),
        activeSegmentPath = active.map(_.path),
        documentCount = documentCount,
        symbolCount = meta.symbolCount(),
        walSizeBytes = meta.walSizeBytes
      )

/** One segment_manifest row as shown by the doctor. */
final case class PostingsSegmentInfo(segmentId: Long, path: String, active: Boolean)

/** Postings section (plan 19): manifest segments, the published snapshot and
  * how many superseded-but-undeleted segment directories await compaction.
  */
final case class PostingsSection(
    segments: Vector[PostingsSegmentInfo],
    snapshotId: Option[Long],
    snapshotDocCount: Option[Int],
    snapshotOccurrenceCount: Option[Long],
    /** Segment dirs on disk that are not the manifest's active segment. */
    compactionPending: Int,
    compactionPendingDirs: Vector[String]
):
  def activeSegments: Vector[PostingsSegmentInfo] = segments.filter(_.active)

object PostingsSection:
  /** Gathers manifest rows from the MetaStore, snapshot facts from a briefly
    * retained current snapshot, and compaction debt by diffing the on-disk
    * `segments/` directory against the manifest's active segment path.
    */
  def gather(meta: MetaStore, manager: SnapshotManager): SectionState[PostingsSection] =
    SectionState.attempt("Postings"):
      val segments = meta
        .allSegments()
        .map(r => PostingsSegmentInfo(r.segmentId, r.path, r.active))
      val snapshotFacts = manager.withCurrent { s =>
        (s.snapshotId, s.docCount, s.reader.occurrenceCount)
      }
      val activePaths = segments.filter(_.active).flatMap(s => normalize(s.path)).toSet
      val pendingDirs = onDiskSegmentDirs(manager.segmentsDir)
        .filter(dir => !normalize(dir.toString).exists(activePaths.contains))
        .map(_.toString)
        .sorted
      PostingsSection(
        segments = segments,
        snapshotId = snapshotFacts.map(_._1),
        snapshotDocCount = snapshotFacts.map(_._2),
        snapshotOccurrenceCount = snapshotFacts.map(_._3),
        compactionPending = pendingDirs.length,
        compactionPendingDirs = pendingDirs
      )

  private def normalize(path: String): Option[String] =
    try Some(Path.of(path).toAbsolutePath.normalize.toString)
    catch case NonFatal(_) => None

  private def onDiskSegmentDirs(segmentsDir: Path): Vector[Path] =
    if !Files.isDirectory(segmentsDir) then Vector.empty
    else
      val stream = Files.list(segmentsDir)
      try
        stream
          .iterator()
          .asScala
          .filter(p => p.getFileName.toString.startsWith("segment-") && Files.isDirectory(p))
          .toVector
      finally stream.close()

/** PC section (plan 19): worker status plus active/registered targets. */
final case class PcSection(
    /** Human-readable worker status string. */
    workerStatus: String,
    /** Targets with a live PC instance, sorted. */
    activeTargets: Vector[String],
    /** Targets registered with the PC facade, sorted. */
    registeredTargets: Vector[String]
)

object PcSection:
  /** `workerAlive`: `Some(isAlive)` when a forked PC worker is configured
    * (from `ForkedPcWorker.isAlive`), `None` for the in-process worker.
    */
  def gather(
      activeTargets: Vector[String],
      registeredTargets: Vector[String],
      workerAlive: Option[Boolean]
  ): PcSection =
    val status = workerAlive match
      case Some(true) => "forked worker alive"
      case Some(false) => "forked worker not running"
      case None => "in-process (no forked worker)"
    PcSection(status, activeTargets.sorted, registeredTargets.sorted)

/** PC Plugins section (plan 19), taken verbatim from the plugin manager's
  * [[PcPluginStatusReport]]: compiler plugins loaded, service plugins loaded,
  * self-test results, disabled plugins with reasons.
  */
final case class PcPluginsSection(
    compilerPlugins: Vector[CompilerPluginStatus],
    servicePlugins: Vector[ServicePluginStatus],
    disabled: Vector[DisabledPlugin]
)

object PcPluginsSection:
  def gather(report: PcPluginStatusReport): PcPluginsSection =
    PcPluginsSection(
      compilerPlugins = report.compilerPlugins,
      servicePlugins = report.servicePlugins,
      disabled = report.disabled
    )

/** Everything [[Doctor.render]] needs, in plan-19 section order. Runtime and
  * Nix are always gatherable; the remaining sections carry a [[SectionState]]
  * so a not-yet-bootstrapped or failed subsystem renders as
  * `unavailable: <reason>` (never throws).
  */
final case class DoctorInput(
    runtime: RuntimeSection,
    nix: NixSection,
    bsp: SectionState[BspSection],
    semanticdb: SectionState[SemanticdbSection],
    sqlite: SectionState[SqliteSection],
    postings: SectionState[PostingsSection],
    pc: SectionState[PcSection],
    pcPlugins: SectionState[PcPluginsSection]
)

object DoctorInput:
  val NotConnected = "not connected"

  /** Pre-bootstrap doctor input: gathers Runtime + Nix only; every subsystem
    * section is `unavailable: not connected`.
    */
  def offline(workspaceRoot: Path): DoctorInput =
    DoctorInput(
      runtime = RuntimeSection.gather(),
      nix = NixSection.gather(workspaceRoot),
      bsp = SectionState.Unavailable(NotConnected),
      semanticdb = SectionState.Unavailable(NotConnected),
      sqlite = SectionState.Unavailable(NotConnected),
      postings = SectionState.Unavailable(NotConnected),
      pc = SectionState.Unavailable(NotConnected),
      pcPlugins = SectionState.Unavailable(NotConnected)
    )
