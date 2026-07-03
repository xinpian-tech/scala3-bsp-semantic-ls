package ls.doctor

import java.nio.file.Files

import com.google.gson.JsonParser

import ch.epfl.scala.bsp4j.{BuildServerCapabilities, InitializeBuildResult}

import ls.bsp.{BspProjectModel, BspTarget}
import ls.pc.{
  CompilerPluginSpec,
  DisabledPlugin,
  PcCompilerPluginConfig,
  PcPluginInitContext,
  PcPluginManager,
  PcServicePlugin
}

import DoctorTestSupport.*

class RenderTest extends munit.FunSuite:

  // --- fixture: one fully populated DoctorInput ------------------------------

  private lazy val repoRoot =
    findRepoRoot().getOrElse(fail("could not locate the repo root (no flake.nix upwards of cwd)"))

  private lazy val fullInput: DoctorInput =
    val tmp = tempRoot("render")

    // BSP: one indexable + one IndexUnavailable target, real gather
    val app = BspTarget(
      bspId = "bsp://ws/app",
      displayName = "app",
      scalaVersion = "3.8.4",
      scalacOptions = Vector("-Xsemanticdb"),
      classDirectory = tmp.resolve("classes"),
      semanticdbRoot = Some(tmp.resolve("sdb")),
      sourceroot = Some(tmp),
      sources = Vector(tmp.resolve("A.scala")),
      directDeps = Vector.empty
    )
    val noSdb = app.copy(
      bspId = "bsp://ws/nosdb",
      displayName = "nosdb",
      semanticdbRoot = None,
      sourceroot = None
    )
    val model = BspProjectModel(
      targets = Vector(app, noSdb),
      uriToTarget = Map(tmp.resolve("A.scala").toUri.toString -> app.bspId)
    )
    val initResult =
      new InitializeBuildResult("Fake BSP", "9.9.9", "2.2.0", new BuildServerCapabilities())

    // SemanticDB: real gather over a fake targetroot
    val sdbDir = tmp.resolve("sdb/META-INF/semanticdb")
    Files.createDirectories(sdbDir)
    Files.write(sdbDir.resolve("A.scala.semanticdb"), Array[Byte](1))
    val semanticdb = SemanticdbSection.fromModel(
      model,
      Some(DocFreshnessStats.of(fresh = 1, stale = 1, missing = 1, uris = Vector("src/a/B.scala")))
    )

    // PC plugins: real PcPluginManager report with loaded/failed entries
    val pluginJar = tmp.resolve("plugin.jar")
    Files.write(pluginJar, Array[Byte](0x50, 0x4b))
    val manager = PcPluginManager(PcPluginInitContext(Some(tmp), tmp.resolve("gen")))
    manager.register(
      new PcServicePlugin:
        def id = "demo-plugin"
    )
    manager.register(
      new PcServicePlugin:
        def id = "broken-plugin"
        override def initialize(ctx: PcPluginInitContext): Unit =
          throw new IllegalStateException("boom \"quoted\"\nsecond line")
    )
    manager.setCompilerPluginConfig(
      PcCompilerPluginConfig(
        Vector(
          CompilerPluginSpec(Vector(pluginJar), Vector("demo:key:value")),
          CompilerPluginSpec(Vector(tmp.resolve("missing.jar")), Vector.empty)
        )
      )
    )

    DoctorInput(
      runtime = RuntimeSection.gather(),
      nix = NixSection.gather(repoRoot),
      bsp = BspSection.gather(model, Some(initResult)),
      semanticdb = semanticdb,
      sqlite = SectionState.Ready(
        SqliteSection(
          databasePath = tmp.resolve("meta.sqlite").toString,
          walEnabled = true,
          journalMode = "wal",
          ftsEnabled = true,
          activeSegmentId = Some(1L),
          activeSegmentPath = Some(tmp.resolve("postings/segments/segment-000001").toString),
          documentCount = 1L,
          symbolCount = 2L,
          walSizeBytes = 4096L
        )
      ),
      postings = SectionState.Ready(
        PostingsSection(
          segments = Vector(
            PostingsSegmentInfo(1L, tmp.resolve("postings/segments/segment-000001").toString, active = true)
          ),
          snapshotId = Some(1L),
          snapshotDocCount = Some(1),
          snapshotOccurrenceCount = Some(5L),
          compactionPending = 1,
          compactionPendingDirs = Vector(tmp.resolve("postings/segments/segment-000000").toString)
        )
      ),
      pc = SectionState.Ready(
        PcSection.gather(
          activeTargets = Vector.empty,
          registeredTargets = Vector("bsp://ws/app"),
          workerAlive = None
        )
      ),
      pcPlugins = SectionState.Ready(PcPluginsSection.gather(manager.statusReport))
    )

  // --- text rendering ---------------------------------------------------------

  test("render: every plan-19 heading appears, in order"):
    val text = Doctor.render(fullInput)
    val headings = Vector(
      "Runtime:",
      "Nix:",
      "BSP:",
      "SemanticDB:",
      "SQLite:",
      "Postings:",
      "PC:",
      "PC Plugins:"
    )
    val indices = headings.map { h =>
      val i = text.indexOf(s"$h\n")
      assert(i >= 0, s"heading '$h' missing in:\n$text")
      i
    }
    assertEquals(indices, indices.sorted, "headings out of plan-19 order")

  test("render: every plan-19 key line appears"):
    val text = Doctor.render(fullInput)
    val keyLines = Vector(
      "  Java: ",
      "  Native access: ",
      "  Compact Object Headers: ",
      "  AOT cache: ",
      "  flake detected: ",
      "  mill-ivy-fetcher input: ",
      "  ivy lock: nix/ivy-lock.nix",
      "  lock status: ",
      "  server: Fake BSP 9.9.9",
      "  targets: 2",
      "  Scala 3 targets: 2 (bsp://ws/app, bsp://ws/nosdb)",
      "  IndexUnavailable targets: 1 (bsp://ws/nosdb)",
      "  semanticdb roots: 1",
      "  fresh docs: 1",
      "  stale docs (md5 mismatch): 1",
      "  missing docs: 1",
      "  stale/missing uris: src/a/B.scala",
      "  database: ",
      "  WAL: enabled (journal_mode=wal)",
      "  FTS: enabled (workspace_symbols_fts present)",
      "  manifest generation: segment 1",
      "  documents: 1",
      "  symbols: 2",
      "  wal size: 4096 bytes",
      "  active segments: 1 of 1",
      "  snapshot id: 1",
      "  snapshot docs: 1",
      "  snapshot occurrences: 5",
      "  compaction pending: 1",
      "  worker status: in-process (no forked worker)",
      "  active targets: none",
      "  registered targets: 1 (bsp://ws/app)",
      "  compiler plugins loaded: 1 of 2",
      "  service plugins loaded: 1 of 2",
      "  self-test results:",
      "  disabled plugins: "
    )
    keyLines.foreach { k =>
      assert(text.contains(k), s"key line '$k' missing in:\n$text")
    }

  test("render: no 'null' leaks anywhere"):
    val text = Doctor.render(fullInput)
    assert(!text.contains("null"), s"'null' leaked into the report:\n$text")

  test("render: broken plugin is reported disabled with its self-test detail"):
    val text = Doctor.render(fullInput)
    assert(text.contains("broken-plugin"), text)
    assert(text.contains("self-test failed"), text)
    assert(text.contains("demo-plugin: ok"), text)

  // --- offline -----------------------------------------------------------------

  test("offline: runtime + nix gathered, everything else 'unavailable: not connected'"):
    val input = DoctorInput.offline(repoRoot)
    val text = Doctor.render(input)
    assert(text.contains("  Java: 25"), text)
    assert(text.contains("  flake detected: yes"), text)
    for heading <- Vector("BSP:", "SemanticDB:", "SQLite:", "Postings:", "PC:", "PC Plugins:") do
      assert(
        text.contains(s"$heading\n  unavailable: not connected"),
        s"'$heading' not marked unavailable in:\n$text"
      )
    assert(!text.contains("null"), text)

  // --- JSON ----------------------------------------------------------------------

  test("renderJson: gson parses it and all section names round-trip"):
    val json = Doctor.renderJson(fullInput)
    val root = JsonParser.parseString(json).getAsJsonObject
    val sections =
      Vector("runtime", "nix", "bsp", "semanticdb", "sqlite", "postings", "pc", "pcPlugins")
    sections.foreach(s => assert(root.has(s), s"section '$s' missing in $json"))
    assertEquals(root.keySet().size(), sections.length)
    assertEquals(root.getAsJsonObject("bsp").get("serverName").getAsString, "Fake BSP")
    assertEquals(root.getAsJsonObject("runtime").get("javaVersion").getAsString.take(2), "25")
    assertEquals(root.getAsJsonObject("postings").get("compactionPending").getAsInt, 1)

  test("renderJson: string escaping round-trips quotes and newlines"):
    val nasty = PcPluginsSection(
      compilerPlugins = Vector.empty,
      servicePlugins = Vector.empty,
      disabled = Vector(DisabledPlugin("we\"ird\\id", "line1\nline2\ttab"))
    )
    val input = DoctorInput.offline(repoRoot).copy(pcPlugins = SectionState.Ready(nasty))
    val root = JsonParser.parseString(Doctor.renderJson(input)).getAsJsonObject
    val d = root.getAsJsonObject("pcPlugins").getAsJsonArray("disabled").get(0).getAsJsonObject
    assertEquals(d.get("id").getAsString, "we\"ird\\id")
    assertEquals(d.get("reason").getAsString, "line1\nline2\ttab")

  test("renderJson: unavailable sections encode as an object with the reason"):
    val root = JsonParser.parseString(Doctor.renderJson(DoctorInput.offline(repoRoot))).getAsJsonObject
    assertEquals(root.getAsJsonObject("bsp").get("unavailable").getAsString, "not connected")
