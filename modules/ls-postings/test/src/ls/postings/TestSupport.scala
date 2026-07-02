package ls.postings

import java.nio.file.{Files, Path}
import scala.collection.mutable.ArrayBuffer
import scala.util.Random

import ls.index.{OccFlags, OccurrenceSink, RenameProfile, Span, TargetBitset}

/** One occurrence as delivered by an [[OccurrenceSink]], for comparisons. */
final case class SunkOcc(
    docOrd: Int,
    targetOrd: Int,
    docEpoch: Int,
    packedStart: Int,
    packedEnd: Int,
    flags: Int
)

final class CollectSink extends OccurrenceSink:
  val out = ArrayBuffer.empty[SunkOcc]
  override def accept(
      docOrd: Int,
      targetOrd: Int,
      docEpoch: Int,
      packedStart: Int,
      packedEnd: Int,
      flags: Int
  ): Unit =
    out += SunkOcc(docOrd, targetOrd, docEpoch, packedStart, packedEnd, flags)

object TestSupport:

  def tempRoot(prefix: String): Path =
    val p = Files.createTempDirectory(s"ls-postings-$prefix-")
    p.toFile.deleteOnExit()
    p

  def packStart(o: GroupOcc): Int = Span.pack(o.span.startLine, o.span.startChar)
  def packEnd(o: GroupOcc): Int = Span.pack(o.span.endLine, o.span.endChar)

  /** Brute-force reference result of a group scan, mirroring the reader
    * contract: sort by (doc_ord, packed_start, packed_end), then target
    * filter, epoch filter and (for rename) editable filter.
    */
  def expectedGroupScan(
      data: SegmentData,
      occs: Vector[GroupOcc],
      allowed: Option[TargetBitset],
      requireEditable: Boolean
  ): Vector[SunkOcc] =
    occs
      .sortBy(o => (o.docOrd, packStart(o), packEnd(o)))
      .filter(o => allowed.forall(_.contains(o.targetOrd)))
      .filter(o => o.docEpoch == data.docs(o.docOrd).epoch)
      .filter(o => !requireEditable || OccFlags.has(o.flags, OccFlags.Editable))
      .map(o => SunkOcc(o.docOrd, o.targetOrd, o.docEpoch, packStart(o), packEnd(o), o.flags))

  /** Brute-force reference result of a doc scan. */
  def expectedDocScan(data: SegmentData, docOrd: Int): Vector[SunkOcc] =
    val doc = data.docs(docOrd)
    data
      .docOccurrences(docOrd)
      .sortBy(o => (Span.pack(o.span.startLine, o.span.startChar), Span.pack(o.span.endLine, o.span.endChar)))
      .map(o =>
        SunkOcc(
          docOrd,
          doc.targetOrd,
          doc.epoch,
          Span.pack(o.span.startLine, o.span.startChar),
          Span.pack(o.span.endLine, o.span.endChar),
          o.flags
        )
      )

  /** Brute-force symbol-at: smallest covering occurrence (packed size), then
    * earliest start, then first in (packed_start, packed_end) sort order.
    * Returns (callerSymbolOrd, span, flags).
    */
  def expectedSymbolAt(
      data: SegmentData,
      docOrd: Int,
      line: Int,
      character: Int
  ): Option[(Int, Span, Int)] =
    val q = Span.pack(line, character)
    val sorted = data
      .docOccurrences(docOrd)
      .sortBy(o => (Span.pack(o.span.startLine, o.span.startChar), Span.pack(o.span.endLine, o.span.endChar)))
    var best: Option[(Int, Span, Int)] = None
    var bestSize = Int.MaxValue
    for o <- sorted do
      val ps = Span.pack(o.span.startLine, o.span.startChar)
      val pe = Span.pack(o.span.endLine, o.span.endChar)
      if ps <= q && q <= pe && (pe - ps) < bestSize then
        bestSize = pe - ps
        best = Some((o.symbolOrd, o.span, o.flags))
    best

  /** Deterministic random corpus: 200 docs, 2000 symbols, 20_000 occurrences
    * (8k ref + 2k definition + 2k rename + 8k doc postings).
    */
  def randomCorpus(seed: Long): SegmentData =
    val rnd = new Random(seed)
    val targetCount = 16
    val docCount = 200
    val symbolCount = 2000
    val refGroups = 400
    val renameGroups = 200

    val targets = Vector.tabulate(targetCount)(t => 100L + t)
    val docs = Vector.tabulate(docCount) { d =>
      SegmentDoc(
        uri = s"file:///ws/src/pkg${d % 10}/Doc$d.scala",
        docId = 5000L + d,
        epoch = 1 + rnd.nextInt(3),
        targetOrd = rnd.nextInt(targetCount),
        generated = rnd.nextInt(20) == 0,
        readonly = rnd.nextInt(20) == 0
      )
    }
    val symbols = Vector.tabulate(symbolCount) { i =>
      SegmentSymbol(
        semanticSymbol = s"ws/pkg${i % 10}/Sym$i${if i % 3 == 0 then "#" else "."}",
        symbolId = 10000L + i,
        refGroupOrd = if rnd.nextInt(10) == 0 then -1 else rnd.nextInt(refGroups),
        renameGroupOrd = if rnd.nextInt(4) == 0 then -1 else rnd.nextInt(renameGroups),
        defTargetOrd = rnd.nextInt(targetCount + 1) - 1
      )
    }

    def randomSpan(): Span =
      val startLine = rnd.nextInt(1000)
      val startChar = rnd.nextInt(120)
      if rnd.nextInt(20) == 0 then
        Span(startLine, startChar, startLine + 1 + rnd.nextInt(3), rnd.nextInt(120))
      else Span(startLine, startChar, startLine, startChar + 1 + rnd.nextInt(30))

    def randomGroupOcc(editableBias: Boolean, definition: Boolean): GroupOcc =
      val docOrd = rnd.nextInt(docCount)
      val doc = docs(docOrd)
      val staleEpoch = rnd.nextInt(10) == 0
      val targetOrd =
        if rnd.nextInt(10) == 0 then rnd.nextInt(targetCount) else doc.targetOrd
      var flags = 0
      if definition then flags |= OccFlags.Definition
      if editableBias then (if rnd.nextInt(10) != 0 then flags |= OccFlags.Editable)
      else if rnd.nextInt(3) == 0 then flags |= OccFlags.Editable
      if rnd.nextInt(15) == 0 then flags |= OccFlags.Synthetic
      GroupOcc(
        docOrd = docOrd,
        docEpoch = if staleEpoch then doc.epoch + 1 + rnd.nextInt(2) else doc.epoch,
        targetOrd = targetOrd,
        span = randomSpan(),
        flags = flags
      )

    def spread(total: Int, buckets: Int)(mk: () => GroupOcc): Vector[Vector[GroupOcc]] =
      val builders = Vector.fill(buckets)(Vector.newBuilder[GroupOcc])
      var i = 0
      while i < total do
        builders(rnd.nextInt(buckets)) += mk()
        i += 1
      builders.map(_.result())

    val refOccs = spread(8000, refGroups)(() => randomGroupOcc(editableBias = false, definition = false))
    val defOccs = spread(2000, refGroups)(() => randomGroupOcc(editableBias = false, definition = true))
    val renOccs = spread(2000, renameGroups)(() => randomGroupOcc(editableBias = true, definition = false))

    val profiles = Vector.tabulate(renameGroups) { g =>
      RenameProfile(
        isLocal = g % 7 == 0,
        isExternal = g % 11 == 0,
        hasGeneratedOccurrences = g % 5 == 0,
        hasReadonlyOccurrences = g % 13 == 0,
        hasOverrideFamily = g % 17 == 0,
        hasCompanion = g % 3 == 0,
        editableOccurrenceCount = renOccs(g).count(o => OccFlags.has(o.flags, OccFlags.Editable)),
        unsafeReasonMask = if g % 9 == 0 then ls.index.UnsafeReason.External else 0L
      )
    }

    // 8k doc occurrences with unique (start,end) per doc so symbol-at
    // expectations are unambiguous.
    val docOccBuilders = Vector.fill(docCount)(Vector.newBuilder[DocOcc])
    val seen = Array.fill(docCount)(scala.collection.mutable.Set.empty[(Int, Int)])
    var made = 0
    while made < 8000 do
      val d = rnd.nextInt(docCount)
      val span = randomSpan()
      val key = (Span.pack(span.startLine, span.startChar), Span.pack(span.endLine, span.endChar))
      if seen(d).add(key) then
        var flags = 0
        if rnd.nextInt(6) == 0 then flags |= OccFlags.Definition
        if rnd.nextInt(3) != 0 then flags |= OccFlags.Editable
        docOccBuilders(d) += DocOcc(rnd.nextInt(symbolCount), span, flags)
        made += 1

    SegmentData(
      docs = docs,
      targets = targets,
      symbols = symbols,
      refOccurrences = refOccs,
      defOccurrences = defOccs,
      renameOccurrences = renOccs,
      renameProfiles = profiles,
      docOccurrences = docOccBuilders.map(_.result())
    )
