package ls.bench

import java.io.PrintStream
import java.nio.file.{Files, Path}
import java.util.Comparator

import scala.util.control.NonFatal

import java.io.File

import scala.jdk.CollectionConverters.*

import ls.bsp.{BspCompileOutcome, ProjectModelLoader}
import ls.index.{OccurrenceSink, Span, TargetBitset}
import ls.pc.{
  PcFacade,
  PcPluginInitContext,
  PcPluginManager,
  PcRequest,
  PcServicePlugin,
  PcSettings,
  PcTargetConfig
}
import org.eclipse.lsp4j.CompletionList
import ls.postings.{SegmentReader, SegmentWriter, SnapshotManager}
import ls.rename.{CompileService, QueryOrchestrator, ReferencesEngine, RenameEngine}
import ls.rename.ingest.{IngestPipeline, TargetSpec, WorkspaceTargets}
import ls.sqlite.MetaStore

/** Benchmark + JFR harness over the real storage layer.
  *
  * Modes:
  * {{{
  *   --smoke        small corpus, finishes well under a minute (CI gate)
  *   --full         bigger corpus for real measurements
  *   --tiny         minimal corpus (test harness self-check)
  *   --jfr <path>   record the run with JFR, dumped to <path> on exit
  *   --jfr-preset <name>  named JFR configuration for --jfr (default: default)
  * }}}
  *
  * The corpus is generated directly at the storage layer ([[Corpus]]), the
  * measured operations are the production read paths (SQLite FTS workspace
  * symbol, mmap reference scans with and without exact target pruning,
  * doc-postings symbol-at-cursor, full doc scans), and every measurement is
  * cross-checked against generated ground truth — the process exits non-zero
  * on any inconsistency.
  */
object BenchMain:

  def main(args: Array[String]): Unit = sys.exit(run(args))

  def run(args: Array[String], out: PrintStream = System.out): Int =
    val mode =
      if args.contains("--full") then "full"
      else if args.contains("--tiny") then "tiny"
      else "smoke"
    val config = configFor(mode, args)

    val jfrPath = args.toVector.sliding(2).collectFirst {
      case Vector("--jfr", path) => Path.of(path)
    }
    // Named JFR configuration (default: "default"); "profile" adds method
    // sampling. Without a configuration a Recording captures no events, so the
    // preset is what makes --jfr produce a useful recording.
    val jfrPreset = args.toVector.sliding(2).collectFirst {
      case Vector("--jfr-preset", name) => name
    }.getOrElse("default")
    val recording = jfrPath.map { _ =>
      val jfrConfig = jdk.jfr.Configuration.getConfiguration(jfrPreset)
      val r = new jdk.jfr.Recording(jfrConfig)
      r.start()
      r
    }

    val dir = Files.createTempDirectory("ls-bench-")
    try
      val exit = runBench(config, dir, out)
      recording.zip(jfrPath).foreach { (r, path) =>
        if path.getParent != null then Files.createDirectories(path.getParent)
        r.dump(path)
        r.close()
        out.println(s"jfr recording dumped to $path")
      }
      exit
    catch
      case NonFatal(t) =>
        out.println(s"bench failed: $t")
        recording.foreach(r => try r.close() catch case NonFatal(_) => ())
        1
    finally deleteRecursively(dir)

  /** Everything [[runBench]] needs. Kept as an explicit value (not derived from
    * `mode` inside the run) so tests can drive the full-mode row wiring with a
    * tiny corpus and inject a real ground-truth mismatch. The document counts
    * for the SemanticDB ingest tiers live here: 1000 (smoke) / 10000 / 100000.
    */
  private[bench] final case class BenchConfig(
      mode: String,
      corpus: CorpusParams,
      queries: Int,
      ingestTiers: Vector[(String, SdbCorpusParams)],
      bspSizes: Vector[Int],
      renameSamples: Int,
      pcSamples: Int,
      injectFailure: Boolean = false,
      corruptOccset: Boolean = false
  )

  /** Builds the run configuration for a mode. `--tier-docs-cap <n>` caps every
    * SemanticDB ingest tier's document count so a real `--full` run (all three
    * `1k`/`10k`/`100k` tier labels) can be exercised cheaply from a test.
    */
  private[bench] def configFor(mode: String, args: Array[String]): BenchConfig =
    val tierCap = args.toVector.sliding(2).collectFirst {
      case Vector("--tier-docs-cap", n) => n.toInt
    }
    // first field is the DOCUMENT count; symbols track docs (one owned symbol
    // per doc). One hot symbol (>=1000 refs), five rare (<=3), the rest single.
    def sdbTier(docs: Int): SdbCorpusParams =
      val d = tierCap.fold(docs)(c => math.min(docs, c))
      SdbCorpusParams(docs = d, symbols = d, hotRefs = 1000, rareCount = 5, rareRefs = 3, fillerRefs = 1)
    val (corpus, queries) = mode match
      case "full" => (CorpusParams(docs = 2000, symbolsPerDoc = 5, occurrences = 500000, targets = 8), 1000)
      case "tiny" => (CorpusParams(docs = 40, symbolsPerDoc = 5, occurrences = 2000, targets = 4), 50)
      case _ => (CorpusParams(docs = 200, symbolsPerDoc = 5, occurrences = 20000, targets = 4), 200)
    val ingestTiers = mode match
      case "full" =>
        Vector(
          "semanticdb-ingest-1k" -> sdbTier(1000),
          "semanticdb-ingest-10k" -> sdbTier(10000),
          "semanticdb-ingest-100k" -> sdbTier(100000)
        )
      case "tiny" => Vector("semanticdb-ingest-1k" -> SdbCorpusParams(20, 20, 30, 3, 3, 1))
      case _ => Vector("semanticdb-ingest-1k" -> sdbTier(1000))
    BenchConfig(
      mode = mode,
      corpus = corpus,
      queries = queries,
      ingestTiers = ingestTiers,
      bspSizes = if mode == "tiny" then Vector(2, 4) else Vector(5, 50, 200),
      renameSamples = if mode == "full" then 10 else if mode == "tiny" then 2 else 4,
      pcSamples = if mode == "full" then 200 else if mode == "tiny" then 20 else 100,
      injectFailure = args.contains("--inject-failure"),
      corruptOccset = args.contains("--corrupt-occset")
    )

  /** Runs a fully-specified config against a throwaway store (test entry). */
  private[bench] def runWith(config: BenchConfig, out: PrintStream): Int =
    val dir = Files.createTempDirectory("ls-bench-")
    try runBench(config, dir, out)
    finally deleteRecursively(dir)

  private def runBench(cfg: BenchConfig, dir: Path, out: PrintStream): Int =
    val mode = cfg.mode
    val params = cfg.corpus
    val queries = cfg.queries
    val injectFailure = cfg.injectFailure
    val errors = Vector.newBuilder[String]
    def check(cond: Boolean, msg: => String): Unit = if !cond then errors += msg

    val meta = MetaStore.open(dir.resolve("meta.sqlite"))
    val manager = SnapshotManager(dir.resolve("postings"))
    try
      // --- corpus + segment ---
      val genStart = System.nanoTime()
      val corpus = Corpus.generate(params, meta)
      val genMs = (System.nanoTime() - genStart) / 1e6

      val writeStart = System.nanoTime()
      val segmentId = manager.nextSegmentId()
      val segmentDir = SegmentWriter.write(manager.root, segmentId, corpus.data, 1L)
      val writeMs = (System.nanoTime() - writeStart) / 1e6

      val openStart = System.nanoTime()
      val reader = SegmentReader.open(segmentDir)
      val openMs = (System.nanoTime() - openStart) / 1e6
      manager.publish(reader)

      val segmentBytes = sizeOf(segmentDir)

      val snap = manager
        .current()
        .getOrElse(throw IllegalStateException("no snapshot published"))
      try
        val s = params.symbolCount
        val t = params.targets

        // --- workspace/symbol FTS ---
        val ftsQueries = Vector.tabulate(queries) { q =>
          // zipf-ish rank sampling: quadratic bias toward hot symbols
          val r = (q.toLong * q.toLong % s).toInt
          corpus.displayNames(r)
        }
        val fts = Metrics.measure(ftsQueries) { query =>
          val hits = meta.workspaceSymbolSearch(query, 50)
          check(hits.nonEmpty, s"fts query '$query' returned no hits")
          check(
            hits.exists(_.displayName.startsWith(query)),
            s"fts query '$query' returned no prefix match"
          )
          hits.length.toLong
        }

        // --- workspace/symbol fuzzy fallback: camel-hump forms the FTS prefix
        // misses (e.g. "Sym5" -> "s5"), forcing the sidecar subsequence search ---
        def fuzzyForm(name: String): String =
          name.take(1).toLowerCase + name.dropWhile(!_.isDigit)
        // (fuzzy-form query, expected exact displayName): expected symbols are
        // constrained to the candidate-pull-visible range (the fuzzy pull is
        // capped at 5000) so the ground-truth check holds even in --full.
        val fuzzyVisible = math.min(s, 4000)
        val fuzzyPlan = Vector.tabulate(queries) { q =>
          val r = (q.toLong * q.toLong % fuzzyVisible).toInt
          (fuzzyForm(corpus.displayNames(r)), corpus.displayNames(r))
        }
        val fuzzy = Metrics.measure(fuzzyPlan) { (query, expected) =>
          val hits = meta.workspaceSymbolSearch(query, 50)
          check(
            hits.exists(_.displayName == expected),
            s"fuzzy '$query' missed expected '$expected' (got ${hits.take(5).map(_.displayName)})"
          )
          hits.length.toLong
        }

        // --- reference scans (hot + rare, with and without pruning) ---
        val hot = corpus.hotGroups(math.min(16, s))
        val rare = corpus.rareGroups(math.min(16, s))
        val allTargets = TargetBitset.all(t)
        val onlyTarget0 = TargetBitset.of(t, Seq(0))

        def scanRefs(groups: Vector[Int], allowed: TargetBitset, expected: Int => Int, what: String) =
          val plan = Vector.tabulate(queries)(i => groups(i % groups.length))
          Metrics.measure(plan) { g =>
            val sink = new CountingSink
            snap.scanReferences(ls.index.RefGroupOrd(g), allowed, sink)
            check(
              sink.count == expected(g),
              s"$what group $g: scanned ${sink.count} records, expected ${expected(g)}"
            )
            sink.count
          }

        val mid = corpus.midGroups(math.min(16, s))
        check(mid.nonEmpty, "corpus produced no mid-rank groups")
        val hotAll = scanRefs(hot, allTargets, corpus.refCountOf(_), "references hot (all targets)")
        val hotPruned =
          scanRefs(hot, onlyTarget0, corpus.refCountPerTarget(_)(0), "references hot (pruned)")
        val midAll = scanRefs(mid, allTargets, corpus.refCountOf(_), "references medium (all targets)")
        val rareAll = scanRefs(rare, allTargets, corpus.refCountOf(_), "references rare (all targets)")
        val rarePruned =
          scanRefs(rare, onlyTarget0, corpus.refCountPerTarget(_)(0), "references rare (pruned)")

        // --- symbol-at-cursor over doc postings ---
        val probes = corpus.cursorProbes
        check(probes.nonEmpty, "corpus generated no cursor probes")
        val probePlan = Vector.tabulate(queries)(i => probes(i % math.max(1, probes.length)))
        val symbolAt = Metrics.measure(probePlan) { probe =>
          snap.symbolAt(ls.index.DocOrd(probe.docOrd), probe.line, probe.character) match
            case None =>
              check(false, s"symbolAt missed at doc ${probe.docOrd}:${probe.line}:${probe.character}")
              0L
            case Some(hit) =>
              val sym = snap.semanticSymbolOf(hit.symbolOrd)
              check(
                sym == probe.semanticSymbol,
                s"symbolAt at doc ${probe.docOrd}:${probe.line} resolved '$sym', expected '${probe.semanticSymbol}'"
              )
              1L
        }

        // --- full doc scan throughput ---
        val docPlan = Vector.tabulate(params.docs)(identity)
        val docScan = Metrics.measure(docPlan) { d =>
          val sink = new CountingSink
          snap.scanDocOccurrences(ls.index.DocOrd(d), sink)
          check(
            sink.count == corpus.docOccCount(d),
            s"doc scan $d: ${sink.count} records, expected ${corpus.docOccCount(d)}"
          )
          sink.count
        }

        // --- SQLite FFM call-overhead microbench (ns/call) ---
        // A tight prepared-statement loop; the per-call cost is dominated by the
        // FFM boundary crossing, not query complexity. symbol_metadata has one
        // row per symbol, so the returned count is the ground-truth invariant.
        val ffmCalls = math.max(queries * 4, 2000)
        val ffmStart = System.nanoTime()
        var ffmSum = 0L
        var fc = 0
        while fc < ffmCalls do
          ffmSum += meta.db
            .prepare("SELECT count(*) FROM symbol_metadata")
            .queryOne(_.columnLong(0))
            .getOrElse(-1L)
          fc += 1
        val ffmNsPerCall = (System.nanoTime() - ffmStart) / ffmCalls
        check(
          ffmSum == s.toLong * ffmCalls,
          s"ffm microbench: symbol_metadata count drifted (sum=$ffmSum, expected ${s.toLong * ffmCalls})"
        )

        // --- occurrence-set preservation gate: the published segment must
        // reproduce the generated ground truth exactly (write -> read invariant).
        // Compare the (doc, packedStart, packedEnd, flags) multiset. ---
        val expectedOccs = scala.collection.mutable.HashMap.empty[(Int, Int, Int, Int), Int]
        for (occs, doc) <- corpus.data.docOccurrences.zipWithIndex; o <- occs do
          val key = (
            doc,
            ls.index.Span.pack(o.span.startLine, o.span.startChar),
            ls.index.Span.pack(o.span.endLine, o.span.endChar),
            o.flags
          )
          expectedOccs.update(key, expectedOccs.getOrElse(key, 0) + 1)
        val scannedOccs = scala.collection.mutable.HashMap.empty[(Int, Int, Int, Int), Int]
        for d <- 0 until params.docs do
          val sink = new CollectingOccSink
          snap.scanDocOccurrences(ls.index.DocOrd(d), sink)
          sink.keys.foreach(k => scannedOccs.update(k, scannedOccs.getOrElse(k, 0) + 1))
        check(
          scannedOccs == expectedOccs,
          s"occurrence-set mismatch: scanned ${scannedOccs.valuesIterator.sum} occurrences " +
            s"(${scannedOccs.size} distinct), expected ${expectedOccs.valuesIterator.sum} (${expectedOccs.size} distinct)"
        )
        val occVerified = expectedOccs.valuesIterator.sum

        // ================= ingest-path rows (real IngestPipeline) =================
        // Ingest-path rows: real IngestPipeline timing, cold/warm start,
        // rename, and the occurrence-set
        // gate re-run against the IngestPipeline-produced segment.
        val ingestLines = Vector.newBuilder[String]
        val tiers: Vector[(String, SdbCorpusParams)] = cfg.ingestTiers

        // Generate + ingest each tier; keep the first (smoke) store open for the
        // cold/warm/rename rows, close the rest. Every tier is consistency-checked
        // immediately after its ingest, including the occurrence-set gate below.
        var keepMeta: MetaStore = null
        var keepMgr: SnapshotManager = null
        var keepOrch: QueryOrchestrator = null
        var keepCorpus: SdbCorpusTruth = null
        var keepIngestMs: Double = 0.0
        for (((label, cp), idx) <- tiers.zipWithIndex) do
          val cdir = Files.createTempDirectory(dir, s"$label-")
          val corpus = SemanticdbCorpus.generate(cp, cdir.resolve("corpus"))
          val imeta = MetaStore.open(cdir.resolve("meta.sqlite"))
          val imgr = SnapshotManager(cdir.resolve("postings"))
          val ipipe = IngestPipeline(imeta, imgr)
          val iorch = QueryOrchestrator(imeta, imgr, ipipe)
          val ws = WorkspaceTargets(
            Vector(TargetSpec(bspId = "bench", semanticdbRoot = corpus.semanticdbRoot, sourceroot = corpus.sourceroot))
          )
          val t0 = System.nanoTime()
          val rep = iorch.ingest(ws)
          val ingestMs = (System.nanoTime() - t0) / 1e6
          check(rep.docsIndexed == cp.docs, s"$label: docsIndexed ${rep.docsIndexed} != ${cp.docs}")
          check(rep.symbolCount == cp.symbols, s"$label: symbolCount ${rep.symbolCount} != ${cp.symbols}")
          check(rep.docsStale == 0, s"$label: ${rep.docsStale} stale docs (md5 mismatch)")
          check(rep.docsSkipped == 0, s"$label: ${rep.docsSkipped} skipped docs (missing source)")
          check(rep.refGroupCount == rep.renameGroupCount, s"$label: ref/rename group counts differ")
          check(rep.refGroupCount == cp.symbols, s"$label: refGroupCount ${rep.refGroupCount} != ${cp.symbols}")
          val wsRows = imeta.db
            .prepare("SELECT count(*) FROM workspace_symbol_rows")
            .queryOne(_.columnLong(0))
            .getOrElse(-1L)
          check(wsRows == cp.symbols.toLong, s"$label: workspace_symbol_rows $wsRows != ${cp.symbols}")
          val docsPerSec = if ingestMs <= 0 then 0L else (cp.docs / (ingestMs / 1000.0)).toLong
          ingestLines += f"$label%-32s ${ingestMs}%10.1f ms  $docsPerSec%10d docs/s"

          // Per-tier occurrence-set preservation gate: this tier's published
          // segment must reproduce the generated per-doc occurrence multiset
          // exactly. Each occurrence is keyed by its owning document's stable uri
          // (not just span+flags) so an occurrence migrating between docs is
          // caught, and the gate runs for every tier — including 10k/100k.
          val expectedOccs = scala.collection.mutable.HashMap.empty[(String, Int, Int, Int), Int]
          for (occs, gd) <- corpus.docOccKeys.zipWithIndex; k <- occs do
            val key = (corpus.docUris(gd), k._2, k._3, k._4)
            expectedOccs.update(key, expectedOccs.getOrElse(key, 0) + 1)
          val scannedOccs = scala.collection.mutable.HashMap.empty[(String, Int, Int, Int), Int]
          val gateSnap = imgr.current().getOrElse(throw IllegalStateException(s"$label: no ingest snapshot"))
          try
            var d = 0
            while d < gateSnap.docCount do
              val doc = ls.index.DocOrd(d)
              val uri = gateSnap.uriOf(doc)
              val sink = new CollectingOccSink
              gateSnap.scanDocOccurrences(doc, sink)
              sink.keys.foreach { k =>
                val key = (uri, k._2, k._3, k._4)
                scannedOccs.update(key, scannedOccs.getOrElse(key, 0) + 1)
              }
              d += 1
          finally gateSnap.release()
          // negative-test hook: inject one bogus scanned occurrence so the real
          // gate below reports a genuine occurrence-set mismatch (not a bare fail).
          if cfg.corruptOccset && idx == 0 then
            scannedOccs.update(("__corrupt__", -1, -1, -1), 1)
          check(
            scannedOccs == expectedOccs,
            s"$label: ingest occurrence-set mismatch: scanned ${scannedOccs.valuesIterator.sum} " +
              s"(${scannedOccs.size} distinct), expected ${expectedOccs.valuesIterator.sum} (${expectedOccs.size} distinct)"
          )

          if idx == 0 then
            keepMeta = imeta; keepMgr = imgr; keepOrch = iorch; keepCorpus = corpus; keepIngestMs = ingestMs
          else { imgr.close(); imeta.close() }

        // cold/warm start. Cold = the fresh ingest plus the first successful
        // references query; warm = a process-restart simulation.
        val refEngine = ReferencesEngine(keepOrch)
        val coldQueryStart = System.nanoTime()
        val coldHits = refEngine.references(keepCorpus.hot.cursorUri, keepCorpus.hot.cursorLine, keepCorpus.hot.cursorChar, includeDeclaration = true)
        val coldMs = keepIngestMs + (System.nanoTime() - coldQueryStart) / 1e6
        check(
          coldHits.locations.length == keepCorpus.probeSymbolOccurrences,
          s"cold-start: references ${coldHits.locations.length} != ${keepCorpus.probeSymbolOccurrences}"
        )
        val warmMs = warmRestart(keepCorpus, check)

        // rename small (rare) / large (hot)
        val renameEngine = RenameEngine(keepOrch, NoopCompiler)
        def renameRow(t: RenameTruth): Metrics.Result =
          Metrics.measure(Vector.fill(cfg.renameSamples)(0)) { _ =>
            val plan = renameEngine.rename(t.cursorUri, t.cursorLine, t.cursorChar, "Renamed")
            check(
              plan.occurrenceCount == t.occurrenceCount,
              s"rename '${t.displayName}': occurrenceCount ${plan.occurrenceCount} != ${t.occurrenceCount}"
            )
            check(
              plan.edits.map((u, e) => u -> e.map(_.span).toSet).toMap == t.editsByUri,
              s"rename '${t.displayName}': edit spans differ from ground truth"
            )
            plan.occurrenceCount.toLong
          }
        val renameSmall = renameRow(keepCorpus.rare)
        val renameLarge = renameRow(keepCorpus.hot)

        // BSP import: time ProjectModelLoader.load against a fake N-target server.
        val bspLines = Vector.newBuilder[String]
        for size <- cfg.bspSizes do
          val bdir = Files.createTempDirectory(dir, s"bsp-$size-")
          val (session, closeBsp) = BenchBspServer.connect(bdir, size)
          try
            val bt0 = System.nanoTime()
            val model = ProjectModelLoader.load(session)
            val bms = (System.nanoTime() - bt0) / 1e6
            check(model.targets.length == size, s"bsp-import-$size: ${model.targets.length} targets != $size")
            check(model.uriToTarget.size == size, s"bsp-import-$size: ${model.uriToTarget.size} source uris != $size")
            check(model.targets.forall(_.semanticdbRoot.isDefined), s"bsp-import-$size: a target lacks a semanticdb root")
            check(model.targets.forall(_.sourceroot.isDefined), s"bsp-import-$size: a target lacks a sourceroot")
            val depEdges = model.targets.map(_.directDeps.length).sum
            check(depEdges == size - 1, s"bsp-import-$size: $depEdges dep edges != ${size - 1}")
            bspLines += f"bsp-import-$size%-32s ${bms}%10.1f ms"
          finally closeBsp()

        // PC completion percentiles + plugin overhead.
        val libClasspath = System
          .getProperty("java.class.path", "")
          .split(File.pathSeparatorChar)
          .toVector
          .filter { e =>
            val nm = Path.of(e).getFileName.toString
            nm.endsWith(".jar") && (nm.startsWith("scala-library") || nm.startsWith("scala3-library"))
          }
          .map(Path.of(_))
        check(libClasspath.nonEmpty, "pc: no scala library jar on the classpath")
        val pcText = "object B:\n  val xs = List(1)\n  val ys = xs.\n"
        val pcCol = "  val ys = xs.".length
        def measurePc(withPlugin: Boolean): Metrics.Result =
          val gen = Files.createTempDirectory(dir, "pc-gen-")
          val pm = new PcPluginManager(PcPluginInitContext(None, gen))
          val plugin = Option.when(withPlugin) {
            val p = new BenchPassthroughPlugin
            pm.register(p)
            p
          }
          val facade = new PcFacade(pm, PcSettings(None, gen, 4, 90000L))
          val what = if withPlugin then "pc completion (plugin)" else "pc completion"
          try
            facade.registerTarget(PcTargetConfig("bench-pc", libClasspath, Vector.empty))
            val uri = "file:///bench/PcBuffer.scala"
            facade.didOpen("bench-pc", uri, pcText)
            val warm = facade.completion(uri, 2, pcCol) // first completion compiles
            check(warm.getItems.asScala.exists(_.getLabel.startsWith("map")), s"$what: warmup has no 'map' item")
            val result = Metrics.measure(Vector.fill(cfg.pcSamples)(0)) { _ =>
              val items = facade.completion(uri, 2, pcCol).getItems
              // every timed sample must still resolve the same completion (proves
              // the plugin path preserves results, not just that it was invoked).
              check(items.asScala.exists(_.getLabel.startsWith("map")), s"$what: timed sample has no 'map' item")
              items.size.toLong
            }
            // the pass-through plugin must have been dispatched once per completion
            // (warmup + every timed sample), proving the plugin hook actually ran.
            plugin.foreach { p =>
              val expected = cfg.pcSamples + 1
              check(
                p.completions == expected,
                s"pc plugin dispatch count ${p.completions} != $expected (warmup + ${cfg.pcSamples} samples)"
              )
            }
            result
          finally facade.shutdown()
        val pcCompletion = measurePc(withPlugin = false)
        val pcWithPlugin = measurePc(withPlugin = true)
        val pcPluginOverheadMs = pcWithPlugin.p50Ms - pcCompletion.p50Ms

        // --- report ---
        out.println(s"scala3-bsp-semantic-ls bench ($mode)")
        out.println(
          f"corpus: docs=${params.docs} targets=$t symbols=$s occurrences=${corpus.totalOccurrences} " +
            f"(generated in $genMs%.1f ms)"
        )
        out.println(
          f"segment: write=$writeMs%.1f ms  open=$openMs%.1f ms  size=${segmentBytes / 1024}%d KiB  " +
            f"queries=$queries"
        )
        out.println()
        out.println(Metrics.header)
        out.println(Metrics.row("workspace/symbol fts (prefix)", fts))
        out.println(Metrics.row("workspace/symbol fuzzy", fuzzy))
        out.println(Metrics.row("references hot (all targets)", hotAll))
        out.println(Metrics.row("references hot (pruned)", hotPruned))
        out.println(Metrics.row("references medium (all targets)", midAll))
        out.println(Metrics.row("references rare (all targets)", rareAll))
        out.println(Metrics.row("references rare (pruned)", rarePruned))
        out.println(Metrics.row("symbolAt (doc postings)", symbolAt))
        out.println(Metrics.row("doc scan (full)", docScan))
        out.println(f"sqlite-ffm-call-overhead: $ffmNsPerCall%d ns/call (${ffmCalls}%d calls)")
        out.println(f"occurrence-set: $occVerified%d occurrences verified against ground truth")
        out.println()
        ingestLines.result().foreach(out.println)
        out.println(f"${"cold-start"}%-32s ${coldMs}%10.1f ms  (ingest + first references query)")
        out.println(f"${"warm-start"}%-32s ${warmMs}%10.1f ms  (reopen + publish + first references query)")
        out.println(Metrics.row("rename small (rare)", renameSmall))
        out.println(Metrics.row("rename large (hot)", renameLarge))
        bspLines.result().foreach(out.println)
        out.println(Metrics.row("pc completion", pcCompletion))
        out.println(f"pc plugin overhead: $pcPluginOverheadMs%.3f ms (p50 delta)")
        keepMgr.close()
        keepMeta.close()

        // Negative-path hook: prove a ground-truth mismatch forces a non-zero exit.
        if injectFailure then check(false, "injected ground-truth mismatch (negative test)")
        val failures = errors.result()
        if failures.isEmpty then
          out.println()
          out.println("consistency: all checks passed")
          0
        else
          out.println()
          out.println(s"consistency: ${failures.length} FAILED checks")
          failures.take(20).foreach(f => out.println(s"  - $f"))
          1
      finally snap.release()
    finally
      manager.close()
      meta.close()

  private final class CountingSink extends OccurrenceSink:
    var count: Int = 0
    override def accept(
        docOrd: Int,
        targetOrd: Int,
        docEpoch: Int,
        packedStart: Int,
        packedEnd: Int,
        flags: Int
    ): Unit = count += 1

  /** Collects (doc, packedStart, packedEnd, flags) keys for the occurrence-set
    * preservation gate.
    */
  private final class CollectingOccSink extends OccurrenceSink:
    val keys: scala.collection.mutable.ArrayBuffer[(Int, Int, Int, Int)] =
      scala.collection.mutable.ArrayBuffer.empty
    override def accept(
        docOrd: Int,
        targetOrd: Int,
        docEpoch: Int,
        packedStart: Int,
        packedEnd: Int,
        flags: Int
    ): Unit = keys += ((docOrd, packedStart, packedEnd, flags))

  /** No-op compile service for the rename bench (rename is FreshRequired and
    * compiles the affected domain before re-ingesting; the compile is a no-op
    * because the SemanticDB corpus is already on disk).
    */
  private object NoopCompiler extends CompileService:
    override def compile(targets: Seq[String]): BspCompileOutcome = BspCompileOutcome.Ok(None)

  /** Pass-through PC service plugin: its `afterCompletion` hook is invoked but
    * returns the completion list unchanged, so labels are preserved and the
    * measured delta is the plugin-dispatch overhead. The invocation count is
    * asserted against warmup+samples to prove the hook actually dispatched.
    */
  private final class BenchPassthroughPlugin extends PcServicePlugin:
    private val calls = new java.util.concurrent.atomic.AtomicInteger(0)
    def completions: Int = calls.get()
    def id: String = "bench-passthrough"
    override def afterCompletion(req: PcRequest, result: CompletionList): CompletionList =
      calls.incrementAndGet()
      result

  /** Warm-start (process-restart) simulation: open a fresh store over the same
    * on-disk directory, recover the manifest-active segment, publish it, and time
    * to the first successful references query.
    */
  private def warmRestart(corpus: SdbCorpusTruth, check: (Boolean, => String) => Unit): Double =
    val storeDir = corpus.root.getParent
    val start = System.nanoTime()
    val wmeta = MetaStore.open(storeDir.resolve("meta.sqlite"))
    val wmgr = SnapshotManager(storeDir.resolve("postings"))
    try
      wmeta.activeSegment() match
        case Some(seg) =>
          val reader = SegmentReader.open(java.nio.file.Path.of(seg.path))
          wmgr.publish(reader, recordCurrentFile = false)
        case None => check(false, "warm-start: no active segment in manifest")
      val engine = ReferencesEngine(QueryOrchestrator(wmeta, wmgr, IngestPipeline(wmeta, wmgr)))
      val hits = engine.references(
        corpus.hot.cursorUri,
        corpus.hot.cursorLine,
        corpus.hot.cursorChar,
        includeDeclaration = true
      )
      check(
        hits.locations.length == corpus.probeSymbolOccurrences,
        s"warm-start: references ${hits.locations.length} != ${corpus.probeSymbolOccurrences}"
      )
      (System.nanoTime() - start) / 1e6
    finally
      wmgr.close()
      wmeta.close()

  private def sizeOf(dir: Path): Long =
    val stream = Files.walk(dir)
    try
      stream
        .filter(Files.isRegularFile(_))
        .mapToLong(p => Files.size(p))
        .sum()
    finally stream.close()

  private def deleteRecursively(root: Path): Unit =
    if Files.exists(root) then
      val stream = Files.walk(root)
      try stream.sorted(Comparator.reverseOrder()).forEach(p => Files.deleteIfExists(p))
      finally stream.close()

/** Latency/throughput sampling and the aligned plain-text report rows. */
object Metrics:

  final case class Result(
      samples: Int,
      p50Ms: Double,
      p95Ms: Double,
      p99Ms: Double,
      records: Long,
      elapsedSeconds: Double
  ):
    def recordsPerSecond: Long =
      if elapsedSeconds <= 0 then 0L else (records / elapsedSeconds).toLong

  /** Runs `body` once per plan entry, timing each invocation. `body` returns
    * the record count it visited (throughput accounting).
    */
  def measure[A](plan: Vector[A])(body: A => Long): Result =
    val nanos = new Array[Long](plan.length)
    var records = 0L
    var total = 0L
    var i = 0
    while i < plan.length do
      val start = System.nanoTime()
      records += body(plan(i))
      val took = System.nanoTime() - start
      nanos(i) = took
      total += took
      i += 1
    java.util.Arrays.sort(nanos)
    Result(
      samples = plan.length,
      p50Ms = percentile(nanos, 0.50),
      p95Ms = percentile(nanos, 0.95),
      p99Ms = percentile(nanos, 0.99),
      records = records,
      elapsedSeconds = total / 1e9
    )

  private def percentile(sorted: Array[Long], p: Double): Double =
    if sorted.isEmpty then 0.0
    else
      val idx = math.min(sorted.length - 1, math.round((sorted.length - 1) * p).toInt)
      sorted(idx) / 1e6

  val header: String =
    f"${"metric"}%-32s ${"p50(ms)"}%10s ${"p95(ms)"}%10s ${"p99(ms)"}%10s ${"records/s"}%12s"

  def row(name: String, r: Result): String =
    f"$name%-32s ${r.p50Ms}%10.3f ${r.p95Ms}%10.3f ${r.p99Ms}%10.3f ${r.recordsPerSecond}%12d"
