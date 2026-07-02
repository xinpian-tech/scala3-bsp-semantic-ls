package ls.postings

import ls.index.*
import TestSupport.*

/** The doc-postings interval block index must confine a symbol-at lookup to
  * the blocks covering the queried line (256 occurrences per block).
  */
class IntervalIndexTest extends munit.FunSuite:

  private val occurrencesPerDoc = 3000
  private val totalBlocks = (occurrencesPerDoc + SegmentFormat.BlockSize - 1) / SegmentFormat.BlockSize

  // doc 0: one single-line occurrence per line 0..2999 at chars [2,12]
  // doc 1: one occurrence spanning lines 0..600 plus small ones on each of lines 0..999
  private val data = SegmentData(
    docs = Vector(
      SegmentDoc("file:///big/Flat.scala", docId = 1, epoch = 1, targetOrd = 0),
      SegmentDoc("file:///big/Deep.scala", docId = 2, epoch = 1, targetOrd = 0)
    ),
    targets = Vector(1L),
    symbols = Vector(
      SegmentSymbol("big/flat.", symbolId = 1),
      SegmentSymbol("big/deep.", symbolId = 2)
    ),
    refOccurrences = Vector.empty,
    defOccurrences = Vector.empty,
    renameOccurrences = Vector.empty,
    renameProfiles = Vector.empty,
    docOccurrences = Vector(
      Vector.tabulate(occurrencesPerDoc)(i => DocOcc(0, Span(i, 2, i, 12), 0)),
      DocOcc(1, Span(0, 0, 600, 5), 0) +:
        Vector.tabulate(1000)(i => DocOcc(1, Span(i, 20, i, 25), 0))
    )
  )

  private var reader: SegmentReader = scala.compiletime.uninitialized

  override def beforeAll(): Unit =
    reader = SegmentReader.open(SegmentWriter.write(tempRoot("interval"), 3, data))

  override def afterAll(): Unit =
    reader.close()

  test("the corpus spans many blocks"):
    assertEquals(totalBlocks, 12)

  test("a targeted line lookup scans exactly one block"):
    for line <- List(0, 100, 255, 256, 1000, 1500, 2999) do
      val (hit, blocksScanned) = reader.symbolAtCounting(0, line, 5)
      assertEquals(hit.map(_.span), Some(Span(line, 2, line, 12)), s"line $line")
      assertEquals(blocksScanned, 1, s"line $line scanned $blocksScanned of $totalBlocks blocks")

  test("a miss on a covered line still scans only that block"):
    val (hit, blocksScanned) = reader.symbolAtCounting(0, 1500, 15)
    assertEquals(hit, None)
    assertEquals(blocksScanned, 1)

  test("a line beyond every block scans nothing"):
    val (hit, blocksScanned) = reader.symbolAtCounting(0, occurrencesPerDoc + 100, 0)
    assertEquals(hit, None)
    assertEquals(blocksScanned, 0)

  test("every line resolves correctly across all blocks"):
    for line <- 0 until occurrencesPerDoc by 97 do
      val hit = reader.symbolAt(0, line, 12) // end boundary, inclusive
      assertEquals(hit.map(_.span), Some(Span(line, 2, line, 12)), s"line $line")

  test("a multi-line occurrence is found from any line it covers"):
    val hit = reader.symbolAt(1, 400, 10).get
    assertEquals(hit.span, Span(0, 0, 600, 5))
    // the small same-line occurrence is smaller, so it wins where it covers
    val small = reader.symbolAt(1, 400, 22).get
    assertEquals(small.span, Span(400, 20, 400, 25))
