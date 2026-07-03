package ls.core

import java.nio.file.{Files, Path}

import ls.bsp.{BspCompileOutcome, BspProjectModel, BspTarget}
import ls.doctor.Doctor
import ls.pc.{PcFacade, PcPluginInitContext, PcPluginManager, PcSettings}
import ls.postings.SnapshotManager
import ls.rename.{
  CompileService,
  DocumentHighlightService,
  QueryOrchestrator,
  ReferencesEngine,
  RenameEngine
}
import ls.rename.ingest.{IngestPipeline, TargetSpec, WorkspaceTargets}
import ls.semanticdb.Md5
import ls.sqlite.MetaStore

/** Production doctor-command wiring: a real Ready [[CoreServices]] whose
  * MetaStore is seeded with one generated and one stale-md5 document, driving
  * [[DoctorCommand.input]] end to end. Pins that the generated-source and
  * stale-target lines come from the live store (not defaults) and that a
  * MetaStore failure degrades the SemanticDB section instead of crashing the
  * whole report.
  */
class DoctorCommandSuite extends munit.FunSuite:

  private final class NoopCompiler extends CompileService:
    def compile(targets: Seq[String]): BspCompileOutcome = BspCompileOutcome.Ok(None)

  private var toClose: List[() => Unit] = Nil
  override def afterAll(): Unit = toClose.foreach(f => f())

  private val LibId = "bsp://ws/lib"

  /** A Ready CoreServices with a model that has one indexable target and a
    * MetaStore seeded with one generated (fresh) doc and one stale-md5 doc.
    */
  private def readyServices(): CoreServices =
    val root = Files.createTempDirectory("ls-core-doctor")
    val srcRoot = root.resolve("src")
    val sdbRoot = root.resolve("sdb")
    Files.createDirectories(srcRoot)

    val meta = MetaStore.open(root.resolve("meta.sqlite"))
    val snapshots = SnapshotManager(root.resolve("postings"))
    val pipeline = IngestPipeline(meta, snapshots)
    val orchestrator = QueryOrchestrator(meta, snapshots, pipeline)
    val pluginManager =
      new PcPluginManager(PcPluginInitContext(None, Files.createTempDirectory("ls-core-doctor-gen")))
    val pc = new PcFacade(pluginManager, PcSettings.ephemeral())

    val targetId = meta.upsertTarget(
      bspId = LibId,
      scalaVersion = "3.8.4",
      classpathHash = "ch",
      optionsHash = "oh",
      semanticdbRoot = sdbRoot.toString,
      sourceroot = srcRoot.toString,
      active = true
    )

    // stale-md5 doc: the source on disk no longer matches the stored md5
    val staleRel = "pkg/S.scala"
    val staleFile = srcRoot.resolve(staleRel)
    Files.createDirectories(staleFile.getParent)
    Files.writeString(staleFile, "object S // current content on disk")
    meta.upsertDocument(
      targetId = targetId,
      uri = staleRel,
      semanticdbPath = sdbRoot.resolve("META-INF/semanticdb/pkg/S.scala.semanticdb").toString,
      semanticdbMtimeMs = 1L,
      md5 = Md5.computeHex("object S // the OLDER version that was indexed"),
      generated = false,
      readonly = false
    )

    // generated doc: flagged generated and fresh (md5 matches disk)
    val genRel = "pkg/G.scala"
    val genFile = srcRoot.resolve(genRel)
    val genText = "object G // generated"
    Files.createDirectories(genFile.getParent)
    Files.writeString(genFile, genText)
    meta.upsertDocument(
      targetId = targetId,
      uri = genRel,
      semanticdbPath = sdbRoot.resolve("META-INF/semanticdb/pkg/G.scala.semanticdb").toString,
      semanticdbMtimeMs = 1L,
      md5 = Md5.computeHex(genText),
      generated = true,
      readonly = false
    )

    val libTarget = BspTarget(
      bspId = LibId,
      displayName = "lib",
      scalaVersion = "3.8.4",
      scalacOptions = Vector("-Xsemanticdb"),
      classDirectory = root.resolve("classes"),
      semanticdbRoot = Some(sdbRoot),
      sourceroot = Some(srcRoot),
      sources = Vector.empty,
      directDeps = Vector.empty
    )
    val model = BspProjectModel(targets = Vector(libTarget), uriToTarget = Map.empty)

    val services = CoreServices(
      workspaceRoot = root,
      storageRoot = root,
      meta = meta,
      snapshots = snapshots,
      pipeline = pipeline,
      orchestrator = orchestrator,
      references = ReferencesEngine(orchestrator),
      highlights = DocumentHighlightService(orchestrator),
      rename = RenameEngine(orchestrator, new NoopCompiler),
      compiler = new NoopCompiler,
      session = None,
      serverInfo = None,
      model = Some(model),
      workspaceTargets = WorkspaceTargets(
        Vector(TargetSpec(bspId = LibId, semanticdbRoot = sdbRoot, sourceroot = srcRoot))
      ),
      pc = pc,
      pcConfigs = Map.empty,
      uriToTarget = Map.empty,
      uris = WorkspaceUris(Vector(root), orchestrator),
      notes = Vector.empty
    )
    toClose ::= (() => services.close())
    services

  // Indented body lines under a top-level `<heading>:` line, up to the next
  // top-level heading (a non-indented, non-empty line).
  private def sectionBody(report: String, heading: String): String =
    val lines = report.linesIterator.toVector
    val start = lines.indexWhere(_ == s"$heading:")
    assert(start >= 0, s"heading '$heading:' not found in:\n$report")
    val rest = lines.drop(start + 1)
    val end = rest.indexWhere(l => l.nonEmpty && !l.startsWith(" "))
    (if end < 0 then rest else rest.take(end)).mkString("\n")

  test("doctor reports generated-source status and stale targets from the live MetaStore"):
    val services = readyServices()
    val report = Doctor.render(DoctorCommand.input(services))
    val semanticdb = sectionBody(report, "SemanticDB")
    assert(
      semanticdb.contains("generated source status: 1"),
      s"expected one generated doc under SemanticDB, got:\n$semanticdb"
    )
    assert(
      semanticdb.contains(s"stale targets: 1 ($LibId)"),
      s"expected the stale target under SemanticDB, got:\n$semanticdb"
    )
    // and never in the SQLite section
    val sqlite = sectionBody(report, "SQLite")
    assert(!sqlite.contains("generated source status"), s"leaked into SQLite:\n$sqlite")
    assert(!sqlite.contains("stale targets"), s"leaked into SQLite:\n$sqlite")

  test("doctor degrades to SemanticDB unavailable when the MetaStore fails, not crash"):
    val services = readyServices()
    // Simulate a failing store: a closed MetaStore throws on every query.
    services.meta.close()
    // The report must still be produced (no throw), with the SemanticDB section
    // rendered as section-level unavailable — i.e. its body IS the unavailable
    // line, not merely a Ready body that happens to contain the word (e.g. the
    // "doc freshness: unavailable: not computed yet" sub-line).
    val report = Doctor.render(DoctorCommand.input(services))
    val semanticdb = sectionBody(report, "SemanticDB")
    assert(
      semanticdb.trim.startsWith("unavailable:"),
      s"SemanticDB should be section-level unavailable, got:\n$semanticdb"
    )
    assert(report.contains("Runtime:"), s"report truncated:\n$report")
