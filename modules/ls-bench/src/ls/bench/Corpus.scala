package ls.bench

import java.nio.file.Path

import ls.index.{RenameProfile, Span, SymKind, SymbolId, TargetId}
import ls.postings.{DocOcc, GroupOcc, SegmentData, SegmentDoc, SegmentSymbol}
import ls.sqlite.{MetaStore, SymbolMetadataRow, WorkspaceSymbolRow}

/** Synthetic corpus parameters. `occurrences` is the reference-occurrence
  * budget; every symbol additionally gets exactly one definition.
  */
final case class CorpusParams(
    docs: Int,
    symbolsPerDoc: Int,
    occurrences: Int,
    targets: Int,
    seed: Long = 42L
):
  def symbolCount: Int = docs * symbolsPerDoc

/** One expected symbol-at-cursor probe. */
final case class CursorProbe(docOrd: Int, line: Int, character: Int, semanticSymbol: String)

/** Generated corpus plus the exact ground truth the consistency checks
  * verify against (expected reference counts, per-target counts, cursor
  * probes, per-doc occurrence counts).
  */
final class CorpusTruth(
    val params: CorpusParams,
    val data: SegmentData,
    val refCountOf: Array[Int],
    val refCountPerTarget: Array[Array[Int]],
    val docOccCount: Array[Int],
    val cursorProbes: Vector[CursorProbe],
    val displayNames: Vector[String]
):
  def totalOccurrences: Long = data.occurrenceCount
  def hotGroups(n: Int): Vector[Int] =
    refCountOf.zipWithIndex.sortBy(-_._1).take(n).map(_._2).toVector
  def rareGroups(n: Int): Vector[Int] =
    refCountOf.zipWithIndex.filter(_._1 > 0).sortBy(_._1).take(n).map(_._2).toVector

/** Synthetic corpus generator working DIRECTLY at the storage layer — no
  * scalac involved (plan 18.3: benchmarks measure the index machinery, not
  * the compiler).
  *
  * Layout: `docs` documents spread round-robin over `targets` build targets;
  * `symbolsPerDoc` symbols defined per document; the reference budget is
  * distributed zipf-ishly (weight 1/rank) so a few groups are hot and the
  * tail is rare — the reference/rename postings, doc postings with interval
  * blocks, symbol metadata and workspace-symbol FTS rows are all populated,
  * with exact ground truth retained for the consistency checks.
  */
object Corpus:

  def generate(params: CorpusParams, meta: MetaStore): CorpusTruth =
    val n = params.docs
    val spd = params.symbolsPerDoc
    val s = params.symbolCount
    val t = params.targets
    require(n > 0 && spd > 0 && t > 0, "corpus dimensions must be positive")

    val docUris = Vector.tabulate(n)(d => f"src/pkg${d % 50}/Doc$d%04d.scala")
    val displayNames = Vector.tabulate(s)(i => s"Sym$i")
    val semanticSymbols = Vector.tabulate(s)(i => s"pkg${(i / spd) % 50}/Sym$i#")

    // --- MetaStore rows: targets, documents, metadata + FTS (one tx) ---
    val targetIds = new Array[Long](t)
    val docIds = new Array[Long](n)
    meta.db.withWriteTransaction {
      for ord <- 0 until t do
        targetIds(ord) = meta
          .upsertTarget(
            bspId = s"bench://target/$ord",
            scalaVersion = "3",
            classpathHash = "bench",
            optionsHash = "bench",
            semanticdbRoot = s"/bench/out/$ord",
            sourceroot = "/bench/src",
            active = true
          )
          .value
      for d <- 0 until n do
        val (docId, _) = meta.upsertDocument(
          targetId = TargetId(targetIds(d % t)),
          uri = docUris(d),
          semanticdbPath = s"/bench/out/${d % t}/META-INF/semanticdb/${docUris(d)}.semanticdb",
          semanticdbMtimeMs = 1L,
          md5 = f"$d%032x",
          generated = false,
          readonly = false
        )
        docIds(d) = docId.value

      // per-doc symbol metadata + workspace-symbol FTS rows
      for d <- 0 until n do
        val defined = (d * spd) until ((d + 1) * spd)
        val targetId = TargetId(targetIds(d % t))
        val metadataRows = defined.toVector.map { i =>
          SymbolMetadataRow(
            symbolId = SymbolId(i + 1L),
            targetId = targetId,
            displayName = displayNames(i),
            ownerName = Some(s"pkg${(i / spd) % 50}"),
            packageName = Some(s"pkg${(i / spd) % 50}"),
            kind = SymKind.Class,
            properties = 0,
            signatureHash = None,
            span = Some(defSpan(i, spd))
          )
        }
        val wsRows = defined.toVector.map { i =>
          WorkspaceSymbolRow(
            displayName = displayNames(i),
            ownerName = Some(s"pkg${(i / spd) % 50}"),
            packageName = Some(s"pkg${(i / spd) % 50}"),
            kind = SymKind.Class,
            targetId = targetId,
            symbolId = SymbolId(i + 1L)
          )
        }
        meta.replaceSymbolMetadata(ls.index.DocId(docIds(d)), metadataRows)
        meta.replaceWorkspaceSymbols(ls.index.DocId(docIds(d)), wsRows)
    }

    // --- zipf-ish reference budget: weight 1/(rank+1) ---
    val weights = Array.tabulate(s)(i => 1.0 / (i + 1).toDouble)
    val weightSum = weights.sum
    val refCounts = new Array[Int](s)
    var assigned = 0
    for i <- 0 until s do
      val c = math.max(0, math.round(params.occurrences * weights(i) / weightSum).toInt)
      refCounts(i) = c
      assigned += c
    // distribute the rounding remainder over the hottest symbols
    var leftover = params.occurrences - assigned
    var cursor = 0
    while leftover > 0 do
      refCounts(cursor % s) += 1
      leftover -= 1
      cursor += 1

    // --- occurrence materialization with per-doc line allocation ---
    // Lines: definitions first (line = local index), then references append.
    val docLineCounters = Array.tabulate(n)(_ => spd)
    val docOccs = Vector.fill(n)(Vector.newBuilder[DocOcc])
    val refPostings = Vector.fill(s)(Vector.newBuilder[GroupOcc])
    val defPostings = Vector.fill(s)(Vector.newBuilder[GroupOcc])
    val renamePostings = Vector.fill(s)(Vector.newBuilder[GroupOcc])
    val refCountPerTarget = Array.fill(s, t)(0)
    val docOccCount = new Array[Int](n)
    val probes = Vector.newBuilder[CursorProbe]
    val rng = new java.util.Random(params.seed)

    import ls.index.OccFlags

    def addOcc(group: Int, doc: Int, span: Span, definition: Boolean): Unit =
      val targetOrd = doc % t
      var flags = OccFlags.Editable
      if definition then flags |= OccFlags.Definition
      val occ = GroupOcc(doc, 1, targetOrd, span, flags)
      if definition then defPostings(group) += occ
      else
        refPostings(group) += occ
        refCountPerTarget(group)(targetOrd) += 1
      renamePostings(group) += occ
      docOccs(doc) += DocOcc(group, span, flags)
      docOccCount(doc) += 1
      if rng.nextInt(37) == 0 then
        probes += CursorProbe(doc, span.startLine, span.startChar + 1, semanticSymbols(group))

    for i <- 0 until s do
      addOcc(i, i / spd, defSpan(i, spd), definition = true)
    for i <- 0 until s do
      var k = 0
      while k < refCounts(i) do
        val doc = math.floorMod(i + k * 7 + 1, n)
        val line = docLineCounters(doc)
        docLineCounters(doc) += 1
        addOcc(i, doc, Span(line, 10, line, 10 + displayNames(i).length), definition = false)
        k += 1

    val data = SegmentData(
      docs = Vector.tabulate(n)(d =>
        SegmentDoc(
          uri = docUris(d),
          docId = docIds(d),
          epoch = 1,
          targetOrd = d % t
        )
      ),
      targets = targetIds.toVector,
      symbols = Vector.tabulate(s)(i =>
        SegmentSymbol(
          semanticSymbol = semanticSymbols(i),
          symbolId = i + 1L,
          refGroupOrd = i,
          renameGroupOrd = i,
          defTargetOrd = (i / spd) % t
        )
      ),
      refOccurrences = refPostings.map(_.result()),
      defOccurrences = defPostings.map(_.result()),
      renameOccurrences = renamePostings.map(_.result()),
      renameProfiles = Vector.tabulate(s)(i =>
        RenameProfile(
          isLocal = false,
          isExternal = false,
          hasGeneratedOccurrences = false,
          hasReadonlyOccurrences = false,
          hasOverrideFamily = false,
          hasCompanion = false,
          editableOccurrenceCount = refCounts(i) + 1,
          unsafeReasonMask = 0L
        )
      ),
      docOccurrences = docOccs.map(_.result())
    )

    CorpusTruth(params, data, refCounts, refCountPerTarget, docOccCount, probes.result(), displayNames)

  private def defSpan(symbol: Int, spd: Int): Span =
    val line = symbol % spd
    Span(line, 6, line, 6 + s"Sym$symbol".length)
