package ls.bench

import java.io.{ByteArrayOutputStream, PrintStream}
import java.nio.charset.StandardCharsets
import java.nio.file.Files

import scala.concurrent.duration.{Duration, DurationInt}

import ls.sqlite.MetaStore

class BenchSuite extends munit.FunSuite:

  override def munitTimeout: Duration = 300.seconds

  test("corpus generator produces consistent ground truth"):
    val dir = Files.createTempDirectory("ls-bench-corpus")
    val meta = MetaStore.open(dir.resolve("meta.sqlite"))
    try
      val params = CorpusParams(docs = 20, symbolsPerDoc = 3, occurrences = 500, targets = 3)
      val corpus = Corpus.generate(params, meta)

      // exact budget: every symbol gets one definition, refs match the plan
      assertEquals(corpus.refCountOf.sum, params.occurrences)
      assertEquals(corpus.data.defOccurrences.map(_.length).sum, params.symbolCount)
      // ref + def + rename(ref+def) + doc(ref+def) postings
      assertEquals(
        corpus.totalOccurrences,
        3L * (params.occurrences + params.symbolCount)
      )
      // per-target counts sum to the group totals
      for g <- 0 until params.symbolCount do
        assertEquals(corpus.refCountPerTarget(g).sum, corpus.refCountOf(g), s"group $g")
      // doc occurrence counts cover everything once
      assertEquals(corpus.docOccCount.sum, params.occurrences + params.symbolCount)
      // zipf shape: the hottest group dominates the rarest
      val hot = corpus.hotGroups(1).head
      val rare = corpus.rareGroups(1).head
      assert(corpus.refCountOf(hot) > corpus.refCountOf(rare))
      // FTS rows landed: every display name is findable
      val hits = meta.workspaceSymbolSearch("Sym0", 10)
      assert(hits.nonEmpty)
      assert(hits.exists(_.displayName == "Sym0"), hits.map(_.displayName).toString)
    finally meta.close()

  test("tiny run renders the report and passes all consistency checks"):
    val buffer = new ByteArrayOutputStream()
    val exit = BenchMain.run(Array("--tiny"), new PrintStream(buffer, true, StandardCharsets.UTF_8))
    val report = buffer.toString(StandardCharsets.UTF_8)
    assertEquals(exit, 0, report)
    for expected <- List(
        "scala3-bsp-semantic-ls bench (tiny)",
        "segment: write=",
        "workspace/symbol fts (prefix)",
        "workspace/symbol fuzzy",
        "references hot (all targets)",
        "references hot (pruned)",
        "references medium (all targets)",
        "references rare (all targets)",
        "references rare (pruned)",
        "symbolAt (doc postings)",
        "doc scan (full)",
        "sqlite-ffm-call-overhead:",
        "ns/call",
        "occurrence-set:",
        "occurrences verified against ground truth",
        "semanticdb-ingest-1k",
        "cold-start",
        "warm-start",
        "rename small (rare)",
        "rename large (hot)",
        "bsp-import-",
        "pc completion",
        "pc plugin overhead:",
        "consistency: all checks passed"
      )
    do assert(report.contains(expected), s"missing '$expected' in:\n$report")

    // metric rows are well-formed: non-negative and monotonic (p50 <= p95 <= p99)
    for name <- List(
        "workspace/symbol fts (prefix)",
        "references hot (all targets)",
        "symbolAt (doc postings)",
        "rename large (hot)",
        "pc completion"
      )
    do
      val (p50, p95, p99, _) = metricRow(report, name)
      assert(p50 >= 0 && p95 >= 0 && p99 >= 0, s"$name: negative percentile in\n$report")
      assert(p50 <= p95 + 1e-9 && p95 <= p99 + 1e-9, s"$name: percentiles not monotonic ($p50/$p95/$p99)")

  test("a ground-truth mismatch makes the bench exit non-zero"):
    val buffer = new ByteArrayOutputStream()
    val exit = BenchMain.run(
      Array("--tiny", "--inject-failure"),
      new PrintStream(buffer, true, StandardCharsets.UTF_8)
    )
    val report = buffer.toString(StandardCharsets.UTF_8)
    assertNotEquals(exit, 0, report)
    assert(report.contains("consistency:") && report.contains("FAILED"), report)

  test("ingest tiers are sized by document count (1000 smoke, 10000/100000 full)"):
    // the first SdbCorpusParams field is the DOCUMENT count, not a symbol count
    assertEquals(BenchMain.configFor("smoke", Array.empty).ingestTiers.map(_._2.docs).toList, List(1000))
    val full = BenchMain.configFor("full", Array("--full")).ingestTiers
    assertEquals(full.map(_._1).toList, List("semanticdb-ingest-1k", "semanticdb-ingest-10k", "semanticdb-ingest-100k"))
    assertEquals(full.map(_._2.docs).toList, List(1000, 10000, 100000))
    // --tier-docs-cap shrinks documents so a full run can be exercised cheaply
    assertEquals(
      BenchMain.configFor("full", Array("--full", "--tier-docs-cap", "40")).ingestTiers.map(_._2.docs).toList,
      List(40, 40, 40)
    )

  test("full mode renders all three ingest-tier rows (docs-capped, cheap)"):
    val cfg = BenchMain
      .configFor("full", Array("--full", "--tier-docs-cap", "40"))
      .copy(
        corpus = CorpusParams(docs = 30, symbolsPerDoc = 3, occurrences = 800, targets = 3),
        queries = 20,
        bspSizes = Vector(3),
        renameSamples = 2,
        pcSamples = 5
      )
    val buffer = new ByteArrayOutputStream()
    val exit = BenchMain.runWith(cfg, new PrintStream(buffer, true, StandardCharsets.UTF_8))
    val report = buffer.toString(StandardCharsets.UTF_8)
    assertEquals(exit, 0, report)
    for row <- List(
        "bench (full)",
        "semanticdb-ingest-1k",
        "semanticdb-ingest-10k",
        "semanticdb-ingest-100k"
      )
    do assert(report.contains(row), s"missing '$row' in:\n$report")

  test("a real occurrence-set mismatch trips the ingest gate (not a bare check)"):
    val cfg = BenchMain.configFor("tiny", Array("--tiny")).copy(corruptOccset = true)
    val buffer = new ByteArrayOutputStream()
    val exit = BenchMain.runWith(cfg, new PrintStream(buffer, true, StandardCharsets.UTF_8))
    val report = buffer.toString(StandardCharsets.UTF_8)
    assertNotEquals(exit, 0, report)
    assert(report.contains("occurrence-set mismatch"), report)

  /** Parses a metric row `name p50 p95 p99 records/s` into its four numbers. */
  private def metricRow(report: String, name: String): (Double, Double, Double, Long) =
    val line = report.linesIterator
      .find(_.startsWith(name))
      .getOrElse(fail(s"no metric row '$name' in:\n$report"))
    val toks = line.substring(name.length).trim.split("\\s+")
    assert(toks.length >= 4, s"malformed metric row '$name': $line")
    (toks(0).toDouble, toks(1).toDouble, toks(2).toDouble, toks(3).toLong)

  test("--smoke exits 0 in-process"):
    val buffer = new ByteArrayOutputStream()
    val exit = BenchMain.run(Array("--smoke"), new PrintStream(buffer, true, StandardCharsets.UTF_8))
    assertEquals(exit, 0, buffer.toString(StandardCharsets.UTF_8))

  private def eventCount(jfr: java.nio.file.Path): (Int, Boolean) =
    import scala.jdk.CollectionConverters.*
    val events = jdk.jfr.consumer.RecordingFile.readAllEvents(jfr).asScala.toVector
    (events.length, events.exists(_.getEventType.getName.startsWith("jdk.")))

  test("--jfr uses a named preset and records JVM events; default preset is 'default'"):
    val dir = Files.createTempDirectory("ls-bench-jfr")
    // explicit preset: --jfr-preset profile
    val jfr = dir.resolve("out.jfr")
    val buf = new ByteArrayOutputStream()
    val exit = BenchMain.run(
      Array("--tiny", "--jfr", jfr.toString, "--jfr-preset", "profile"),
      new PrintStream(buf, true, StandardCharsets.UTF_8)
    )
    assertEquals(exit, 0, buf.toString(StandardCharsets.UTF_8))
    assert(Files.isRegularFile(jfr), "jfr recording file should exist")
    val (count, hasJvm) = eventCount(jfr)
    assert(count > 0, s"jfr recording contained no events (an unconfigured recording is empty)")
    assert(hasJvm, "jfr recording contained no jdk.* JVM events")

    // default preset (no --jfr-preset) also produces a configured, non-empty recording
    val jfr2 = dir.resolve("out2.jfr")
    val buf2 = new ByteArrayOutputStream()
    val exit2 = BenchMain.run(
      Array("--tiny", "--jfr", jfr2.toString),
      new PrintStream(buf2, true, StandardCharsets.UTF_8)
    )
    assertEquals(exit2, 0, buf2.toString(StandardCharsets.UTF_8))
    assert(eventCount(jfr2)._1 > 0, "default-preset jfr recording was empty")
