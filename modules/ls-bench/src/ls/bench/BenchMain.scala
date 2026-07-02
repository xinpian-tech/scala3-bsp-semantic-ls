package ls.bench

import java.io.PrintStream
import java.nio.file.{Files, Path}
import java.util.Comparator

import scala.util.control.NonFatal

import ls.index.{OccurrenceSink, TargetBitset}
import ls.postings.{PostingsSnapshot, SegmentReader, SegmentWriter, SnapshotManager}
import ls.sqlite.MetaStore

/** Benchmark + JFR harness over the real storage layer (plan 18.3 / Phase 10).
  *
  * Modes:
  * {{{
  *   --smoke        small corpus, finishes well under a minute (CI gate)
  *   --full         bigger corpus for real measurements
  *   --tiny         minimal corpus (test harness self-check)
  *   --jfr <path>   record the run with JFR, dumped to <path> on exit
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
    val (params, queries) = mode match
      case "full" => (CorpusParams(docs = 2000, symbolsPerDoc = 5, occurrences = 500000, targets = 8), 1000)
      case "tiny" => (CorpusParams(docs = 40, symbolsPerDoc = 5, occurrences = 2000, targets = 4), 50)
      case _ => (CorpusParams(docs = 200, symbolsPerDoc = 5, occurrences = 20000, targets = 4), 200)

    val jfrPath = args.toVector.sliding(2).collectFirst {
      case Vector("--jfr", path) => Path.of(path)
    }
    val recording = jfrPath.map { _ =>
      val r = new jdk.jfr.Recording()
      r.start()
      r
    }

    val dir = Files.createTempDirectory("ls-bench-")
    try
      val exit = runBench(mode, params, queries, dir, out)
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

  private def runBench(
      mode: String,
      params: CorpusParams,
      queries: Int,
      dir: Path,
      out: PrintStream
  ): Int =
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

        val hotAll = scanRefs(hot, allTargets, corpus.refCountOf(_), "references hot (all targets)")
        val hotPruned =
          scanRefs(hot, onlyTarget0, corpus.refCountPerTarget(_)(0), "references hot (pruned)")
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
        out.println(Metrics.row("workspace/symbol fts", fts))
        out.println(Metrics.row("references hot (all targets)", hotAll))
        out.println(Metrics.row("references hot (pruned)", hotPruned))
        out.println(Metrics.row("references rare (all targets)", rareAll))
        out.println(Metrics.row("references rare (pruned)", rarePruned))
        out.println(Metrics.row("symbolAt (doc postings)", symbolAt))
        out.println(Metrics.row("doc scan (full)", docScan))

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
