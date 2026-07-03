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
        "consistency: all checks passed"
      )
    do assert(report.contains(expected), s"missing '$expected' in:\n$report")

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
