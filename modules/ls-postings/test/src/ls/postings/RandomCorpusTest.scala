package ls.postings

import scala.util.Random

import ls.index.*
import TestSupport.*

/** Seeded-random corpus (200 docs, 2000 symbols, 20k occurrences): every scan
  * must equal the brute-force reference result, with and without target
  * pruning; symbol-at probes must match the brute-force smallest-covering
  * rule.
  */
class RandomCorpusTest extends munit.FunSuite:
  override val munitTimeout = scala.concurrent.duration.Duration(120, "s")

  private val seed = 0xc0ffee42L
  private val data = TestSupport.randomCorpus(seed)
  private var snapshot: PostingsSnapshot = scala.compiletime.uninitialized

  override def beforeAll(): Unit =
    val dir = SegmentWriter.write(tempRoot("random"), 42, data)
    snapshot = new PostingsSnapshot(SegmentReader.open(dir))

  override def afterAll(): Unit =
    snapshot.markSuperseded()
    snapshot.release()

  test("corpus has the required shape"):
    assertEquals(data.docs.length, 200)
    assertEquals(data.symbols.length, 2000)
    assertEquals(data.occurrenceCount, 20000L)
    assertEquals(snapshot.reader.occurrenceCount, 20000L)

  test("every ref group scan equals brute force without pruning"):
    val all = TargetBitset.all(data.targets.length)
    for g <- data.refOccurrences.indices do
      val sink = CollectSink()
      snapshot.scanReferences(RefGroupOrd(g), all, sink)
      assertEquals(
        sink.out.toVector,
        expectedGroupScan(data, data.refOccurrences(g), Some(all), false),
        s"ref group $g"
      )

  test("every ref group scan equals brute force with random target pruning"):
    val rnd = new Random(seed + 1)
    for g <- data.refOccurrences.indices do
      val allowedOrds = (0 until data.targets.length).filter(_ => rnd.nextBoolean())
      val allowed = TargetBitset.of(data.targets.length, allowedOrds)
      val sink = CollectSink()
      snapshot.scanReferences(RefGroupOrd(g), allowed, sink)
      assertEquals(
        sink.out.toVector,
        expectedGroupScan(data, data.refOccurrences(g), Some(allowed), false),
        s"ref group $g allowed=$allowedOrds"
      )

  test("every definition group scan equals brute force"):
    for g <- data.defOccurrences.indices do
      val sink = CollectSink()
      snapshot.scanDefinitions(RefGroupOrd(g), sink)
      assertEquals(
        sink.out.toVector,
        expectedGroupScan(data, data.defOccurrences(g), None, false),
        s"definition group $g"
      )

  test("every rename group scan equals brute force (editable + fresh only)"):
    for g <- data.renameOccurrences.indices do
      val sink = CollectSink()
      snapshot.scanRenameEdits(RenameGroupOrd(g), sink)
      assertEquals(
        sink.out.toVector,
        expectedGroupScan(data, data.renameOccurrences(g), None, true),
        s"rename group $g"
      )

  test("epoch-stale occurrences are dropped from scans"):
    val all = TargetBitset.all(data.targets.length)
    val totalStale = data.refOccurrences.flatten
      .count(o => o.docEpoch != data.docs(o.docOrd).epoch)
    assert(totalStale > 100, s"corpus should contain stale records, had $totalStale")
    var surfaced = 0
    for g <- data.refOccurrences.indices do
      val sink = CollectSink()
      snapshot.scanReferences(RefGroupOrd(g), all, sink)
      surfaced += sink.out.length
    assertEquals(surfaced, data.refOccurrences.flatten.length - totalStale)

  test("every doc scan equals brute force"):
    for d <- data.docs.indices do
      val sink = CollectSink()
      snapshot.scanDocOccurrences(DocOrd(d), sink)
      assertEquals(sink.out.toVector, expectedDocScan(data, d), s"doc $d")

  test("all rename profiles round-trip"):
    for g <- data.renameProfiles.indices do
      assertEquals(snapshot.renameProfileOf(RenameGroupOrd(g)), data.renameProfiles(g), s"group $g")

  test("symbol dictionary is complete and consistent"):
    for callerOrd <- data.symbols.indices do
      val sym = data.symbols(callerOrd)
      val ord = snapshot.symbolOrdOf(sym.semanticSymbol)
      assert(ord.isDefined, s"missing ${sym.semanticSymbol}")
      assertEquals(snapshot.semanticSymbolOf(ord.get), sym.semanticSymbol)
      assertEquals(snapshot.reader.symbolIdOf(ord.get.ord), sym.symbolId)
      assertEquals(snapshot.refGroupOf(ord.get).map(_.ord).getOrElse(-1), sym.refGroupOrd)
      assertEquals(snapshot.renameGroupOf(ord.get).map(_.ord).getOrElse(-1), sym.renameGroupOrd)
      assertEquals(snapshot.definitionTargetOf(ord.get).map(_.ord).getOrElse(-1), sym.defTargetOrd)
    assertEquals(snapshot.symbolOrdOf("ws/pkg0/DoesNotExist#"), None)

  test("doc dictionary is complete"):
    for d <- data.docs.indices do
      val doc = data.docs(d)
      assertEquals(snapshot.uriOf(DocOrd(d)), doc.uri)
      assertEquals(snapshot.docOrdOf(doc.uri).map(_.ord), Some(d))
      assertEquals(snapshot.epochOf(DocOrd(d)), doc.epoch)
      assertEquals(snapshot.targetOrdOf(DocOrd(d)).ord, doc.targetOrd)
      assertEquals(snapshot.isGenerated(DocOrd(d)), doc.generated)
      assertEquals(snapshot.isReadonly(DocOrd(d)), doc.readonly)

  test("random symbol-at probes match brute force"):
    val rnd = new Random(seed + 2)
    // sorted symbol ordinal per caller ordinal, resolved through the contract
    val sortedOrd: Map[Int, Int] =
      data.symbols.indices
        .map(c => c -> snapshot.symbolOrdOf(data.symbols(c).semanticSymbol).get.ord)
        .toMap
    var hits = 0
    def probe(d: Int, line: Int, char: Int): Unit =
      val actual = snapshot.symbolAt(DocOrd(d), line, char)
      val expected = expectedSymbolAt(data, d, line, char)
      assertEquals(actual.isDefined, expected.isDefined, s"doc $d @$line:$char")
      for (a, (callerSym, span, flags)) <- actual.zip(expected) do
        hits += 1
        assertEquals(a.span, span, s"doc $d @$line:$char")
        assertEquals(a.symbolOrd.ord, sortedOrd(callerSym), s"doc $d @$line:$char")
        assertEquals(a.flags, flags, s"doc $d @$line:$char")
        assertEquals(
          a.role,
          if OccFlags.has(flags, OccFlags.Definition) then Role.Definition else Role.Reference
        )
    // uniform probes: mostly misses, must agree with brute force
    for _ <- 0 until 1000 do
      probe(rnd.nextInt(data.docs.length), rnd.nextInt(1005), rnd.nextInt(160))
    // targeted probes at occurrence boundaries: mostly hits
    for _ <- 0 until 1000 do
      val d = Iterator
        .continually(rnd.nextInt(data.docs.length))
        .find(data.docOccurrences(_).nonEmpty)
        .get
      val occs = data.docOccurrences(d)
      val o = occs(rnd.nextInt(occs.length))
      rnd.nextInt(5) match
        case 0 => probe(d, o.span.startLine, o.span.startChar) // start boundary
        case 1 => probe(d, o.span.endLine, o.span.endChar) // end boundary (inclusive)
        case 2 => probe(d, o.span.startLine, o.span.startChar + 1)
        case 3 => probe(d, o.span.endLine, o.span.endChar + 1) // just past the end
        case _ => probe(d, o.span.startLine, math.max(0, o.span.startChar - 1))
    assert(hits > 300, s"probe set too weak: only $hits hits")
