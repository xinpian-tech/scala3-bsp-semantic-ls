package ls.postings

import ls.index.*
import TestSupport.*

/** Full write/read round-trip over a small hand-built corpus with known
  * expectations: dictionaries, group scans (with and without target pruning),
  * epoch filtering, rename editable filtering, rename profiles, doc scans and
  * symbol-at boundary semantics.
  */
class HandBuiltCorpusTest extends munit.FunSuite:

  // caller symbol ordinals (deliberately not in sorted order)
  private val SymB = 0 // "a/B."
  private val SymA = 1 // "a/A#"
  private val SymFoo = 2 // "a/A#foo()."

  private val data = SegmentData(
    docs = Vector(
      SegmentDoc("file:///a/A.scala", docId = 1, epoch = 3, targetOrd = 0),
      SegmentDoc("file:///a/B.scala", docId = 2, epoch = 1, targetOrd = 1, generated = true),
      SegmentDoc("file:///a/C.scala", docId = 3, epoch = 2, targetOrd = 2, readonly = true)
    ),
    targets = Vector(11L, 22L, 33L),
    symbols = Vector(
      SegmentSymbol("a/B.", symbolId = 200, refGroupOrd = 1, renameGroupOrd = 0, defTargetOrd = 1),
      SegmentSymbol("a/A#", symbolId = 100, refGroupOrd = 0, renameGroupOrd = -1, defTargetOrd = 0),
      SegmentSymbol("a/A#foo().", symbolId = 101, refGroupOrd = 0, renameGroupOrd = 0, defTargetOrd = -1)
    ),
    refOccurrences = Vector(
      Vector(
        GroupOcc(0, 3, 0, Span(1, 4, 1, 7), 0),
        GroupOcc(1, 1, 1, Span(2, 0, 2, 3), OccFlags.Editable),
        GroupOcc(0, 2, 0, Span(9, 0, 9, 3), 0), // stale epoch: doc 0 is at epoch 3
        GroupOcc(2, 2, 2, Span(5, 5, 5, 8), 0)
      ),
      Vector(
        GroupOcc(0, 3, 0, Span(7, 1, 7, 2), 0)
      )
    ),
    defOccurrences = Vector(
      Vector(GroupOcc(0, 3, 0, Span(0, 6, 0, 9), OccFlags.Definition)),
      Vector.empty
    ),
    renameOccurrences = Vector(
      Vector(
        GroupOcc(0, 3, 0, Span(1, 4, 1, 7), OccFlags.Editable),
        GroupOcc(1, 1, 1, Span(2, 0, 2, 3), 0), // not editable
        GroupOcc(2, 5, 2, Span(5, 5, 5, 8), OccFlags.Editable) // stale epoch
      )
    ),
    renameProfiles = Vector(
      RenameProfile(
        isLocal = false,
        isExternal = true,
        hasGeneratedOccurrences = true,
        hasReadonlyOccurrences = false,
        hasOverrideFamily = false,
        hasCompanion = true,
        editableOccurrenceCount = 1,
        unsafeReasonMask = UnsafeReason.External | UnsafeReason.GeneratedOccurrence
      )
    ),
    docOccurrences = Vector(
      Vector(
        DocOcc(SymA, Span(0, 6, 0, 9), OccFlags.Definition),
        DocOcc(SymFoo, Span(1, 4, 1, 7), 0),
        DocOcc(SymB, Span(3, 0, 3, 20), 0), // outer
        DocOcc(SymFoo, Span(3, 5, 3, 10), 0), // nested inner
        DocOcc(SymB, Span(4, 5, 4, 10), 0), // adjacent left
        DocOcc(SymFoo, Span(4, 10, 4, 15), 0) // adjacent right
      ),
      Vector(DocOcc(SymB, Span(2, 0, 2, 3), OccFlags.Generated)),
      Vector.empty
    )
  )

  private var snapshot: PostingsSnapshot = scala.compiletime.uninitialized

  override def beforeAll(): Unit =
    val root = tempRoot("hand")
    val dir = SegmentWriter.write(root, segmentId = 7, data, createdAtMs = 123456789L)
    assertEquals(dir.getFileName.toString, "segment-000007")
    snapshot = new PostingsSnapshot(SegmentReader.open(dir))

  override def afterAll(): Unit =
    snapshot.markSuperseded()
    snapshot.release()

  private def sortedOrdOf(callerOrd: Int): Int =
    snapshot.symbolOrdOf(data.symbols(callerOrd).semanticSymbol).get.ord

  /** Collects (packedStart, flags) for a doc, via either the full or the
    * editable scan.
    */
  private def collectDoc(snap: PostingsSnapshot, doc: Int, editableOnly: Boolean): Vector[(Int, Int)] =
    val out = Vector.newBuilder[(Int, Int)]
    val sink = new OccurrenceSink:
      def accept(docOrd: Int, targetOrd: Int, docEpoch: Int, packedStart: Int, packedEnd: Int, flags: Int): Unit =
        out += ((packedStart, flags))
    if editableOnly then snap.scanDocEditable(DocOrd(doc), sink)
    else snap.scanDocOccurrences(DocOrd(doc), sink)
    out.result()

  test("scanDocEditable yields exactly the editable subset of scanDocOccurrences"):
    val corpus = SegmentData(
      docs = Vector(SegmentDoc("file:///e/E.scala", docId = 1, epoch = 1, targetOrd = 0)),
      targets = Vector(1L),
      symbols = Vector(SegmentSymbol("e/E#", symbolId = 1, refGroupOrd = 0, renameGroupOrd = -1, defTargetOrd = 0)),
      refOccurrences = Vector(Vector(GroupOcc(0, 1, 0, Span(0, 0, 0, 1), 0))),
      defOccurrences = Vector(Vector.empty),
      renameOccurrences = Vector.empty,
      renameProfiles = Vector.empty,
      docOccurrences = Vector(
        Vector(
          DocOcc(0, Span(0, 0, 0, 1), OccFlags.Definition), // not editable
          DocOcc(0, Span(1, 0, 1, 1), OccFlags.Editable), // editable
          DocOcc(0, Span(2, 0, 2, 1), OccFlags.Generated), // not editable
          DocOcc(0, Span(3, 0, 3, 1), OccFlags.Editable | OccFlags.Definition), // editable
          DocOcc(0, Span(4, 0, 4, 1), OccFlags.Readonly), // not editable
          DocOcc(0, Span(5, 0, 5, 1), 0) // not editable
        )
      )
    )
    val dir = SegmentWriter.write(tempRoot("editable"), segmentId = 1, corpus, createdAtMs = 1L)
    val snap = new PostingsSnapshot(SegmentReader.open(dir))
    try
      val all = collectDoc(snap, 0, editableOnly = false)
      val editable = collectDoc(snap, 0, editableOnly = true)
      val bruteForce = all.filter((_, flags) => OccFlags.has(flags, OccFlags.Editable))
      assertEquals(editable, bruteForce, "scanDocEditable must equal the editable filter of scanDocOccurrences")
      // Editable is a STRICT subset here (non-editable occurrences exist and are excluded),
      // so the test fails if scanDocEditable ever emits the full doc set.
      assertEquals(all.length, 6)
      assertEquals(editable.length, 2)
      assert(editable.forall((_, flags) => OccFlags.has(flags, OccFlags.Editable)))
      val editableStarts = editable.map(_._1).toSet
      assertEquals(editableStarts, Set(Span.pack(1, 0), Span.pack(3, 0)))
      // Negative: definition-only, generated, readonly and plain occurrences are excluded.
      assert(!editableStarts.contains(Span.pack(0, 0)), "definition-only excluded")
      assert(!editableStarts.contains(Span.pack(2, 0)), "generated excluded")
      assert(!editableStarts.contains(Span.pack(4, 0)), "readonly excluded")
      assert(!editableStarts.contains(Span.pack(5, 0)), "plain excluded")
    finally
      snap.markSuperseded()
      snap.release()

  test("header fields round-trip"):
    assertEquals(snapshot.snapshotId, 7L)
    assertEquals(snapshot.reader.createdAtMs, 123456789L)
    assertEquals(snapshot.reader.refGroupCount, 2)
    assertEquals(snapshot.reader.renameGroupCount, 1)
    assertEquals(snapshot.docCount, 3)
    assertEquals(snapshot.reader.occurrenceCount, 16L)

  test("doc dictionary round-trips"):
    assertEquals(snapshot.uriOf(DocOrd(0)), "file:///a/A.scala")
    assertEquals(snapshot.uriOf(DocOrd(2)), "file:///a/C.scala")
    assertEquals(snapshot.docOrdOf("file:///a/B.scala").map(_.ord), Some(1))
    assertEquals(snapshot.docOrdOf("file:///nope.scala"), None)
    assertEquals(snapshot.epochOf(DocOrd(0)), 3)
    assertEquals(snapshot.epochOf(DocOrd(1)), 1)
    assertEquals(snapshot.targetOrdOf(DocOrd(2)).ord, 2)
    assertEquals(snapshot.isGenerated(DocOrd(1)), true)
    assertEquals(snapshot.isGenerated(DocOrd(0)), false)
    assertEquals(snapshot.isReadonly(DocOrd(2)), true)
    assertEquals(snapshot.isReadonly(DocOrd(1)), false)

  test("target dictionary round-trips"):
    assertEquals(snapshot.targetCount, 3)
    assertEquals(snapshot.targetIdOf(TargetOrd(1)).value, 22L)
    assertEquals(snapshot.targetOrdOfId(TargetId(33L)).map(_.ord), Some(2))
    assertEquals(snapshot.targetOrdOfId(TargetId(99L)), None)

  test("symbol dictionary is sorted, searchable and complete"):
    for callerOrd <- data.symbols.indices do
      val sym = data.symbols(callerOrd)
      val ord = snapshot.symbolOrdOf(sym.semanticSymbol)
      assert(ord.isDefined, s"symbol ${sym.semanticSymbol} not found")
      assertEquals(snapshot.semanticSymbolOf(ord.get), sym.semanticSymbol)
      assertEquals(snapshot.reader.symbolIdOf(ord.get.ord), sym.symbolId)
    assertEquals(snapshot.symbolOrdOf("a/Zzz."), None)
    assertEquals(snapshot.symbolOrdOf(""), None)
    // sorted UTF-8 order: a/A# < a/A#foo(). < a/B.
    assertEquals(sortedOrdOf(SymA), 0)
    assertEquals(sortedOrdOf(SymFoo), 1)
    assertEquals(sortedOrdOf(SymB), 2)

  test("group and target lookups from the symbol dictionary"):
    val a = SymbolOrd(sortedOrdOf(SymA))
    val foo = SymbolOrd(sortedOrdOf(SymFoo))
    val b = SymbolOrd(sortedOrdOf(SymB))
    assertEquals(snapshot.refGroupOf(a).map(_.ord), Some(0))
    assertEquals(snapshot.refGroupOf(b).map(_.ord), Some(1))
    assertEquals(snapshot.renameGroupOf(a), None)
    assertEquals(snapshot.renameGroupOf(foo).map(_.ord), Some(0))
    assertEquals(snapshot.definitionTargetOf(a).map(_.ord), Some(0))
    assertEquals(snapshot.definitionTargetOf(foo), None)

  test("scanReferences: all targets allowed, epoch-stale record dropped"):
    val sink = CollectSink()
    snapshot.scanReferences(RefGroupOrd(0), TargetBitset.all(3), sink)
    val expected = expectedGroupScan(data, data.refOccurrences(0), Some(TargetBitset.all(3)), false)
    assertEquals(sink.out.toVector, expected)
    assertEquals(sink.out.length, 3) // 4 raw minus 1 stale-epoch
    assert(!sink.out.exists(o => o.packedStart == Span.pack(9, 0)))

  test("scanReferences: target pruning"):
    val allowed = TargetBitset.of(3, Seq(0, 1))
    val sink = CollectSink()
    snapshot.scanReferences(RefGroupOrd(0), allowed, sink)
    assertEquals(sink.out.toVector, expectedGroupScan(data, data.refOccurrences(0), Some(allowed), false))
    assert(sink.out.forall(o => o.targetOrd != 2))

    val only2 = TargetBitset.of(3, Seq(2))
    val sink2 = CollectSink()
    snapshot.scanReferences(RefGroupOrd(0), only2, sink2)
    assertEquals(sink2.out.toVector, expectedGroupScan(data, data.refOccurrences(0), Some(only2), false))
    assertEquals(sink2.out.length, 1)

    val none = TargetBitset.empty(3)
    val sink3 = CollectSink()
    snapshot.scanReferences(RefGroupOrd(0), none, sink3)
    assertEquals(sink3.out.length, 0)

  test("scanDefinitions"):
    val sink = CollectSink()
    snapshot.scanDefinitions(RefGroupOrd(0), sink)
    assertEquals(sink.out.toVector, expectedGroupScan(data, data.defOccurrences(0), None, false))
    assertEquals(sink.out.length, 1)
    assert(OccFlags.has(sink.out(0).flags, OccFlags.Definition))
    val empty = CollectSink()
    snapshot.scanDefinitions(RefGroupOrd(1), empty)
    assertEquals(empty.out.length, 0)

  test("scanRenameEdits: only editable, epoch-fresh occurrences"):
    val sink = CollectSink()
    snapshot.scanRenameEdits(RenameGroupOrd(0), sink)
    assertEquals(sink.out.toVector, expectedGroupScan(data, data.renameOccurrences(0), None, true))
    assertEquals(sink.out.length, 1)
    assertEquals(sink.out(0).docOrd, 0)

  test("renameProfileOf round-trips all 8 fields"):
    assertEquals(snapshot.renameProfileOf(RenameGroupOrd(0)), data.renameProfiles(0))

  test("scanDocOccurrences delivers doc postings in position order"):
    for d <- data.docs.indices do
      val sink = CollectSink()
      snapshot.scanDocOccurrences(DocOrd(d), sink)
      assertEquals(sink.out.toVector, expectedDocScan(data, d), s"doc $d")

  test("symbolAt: start and end boundaries are inclusive"):
    val hit = snapshot.symbolAt(DocOrd(0), 1, 4).get
    assertEquals(hit.symbolOrd.ord, sortedOrdOf(SymFoo))
    assertEquals(hit.span, Span(1, 4, 1, 7))
    assertEquals(hit.role, Role.Reference)
    assertEquals(snapshot.symbolAt(DocOrd(0), 1, 7).map(_.span), Some(Span(1, 4, 1, 7)))
    assertEquals(snapshot.symbolAt(DocOrd(0), 1, 3), None)
    assertEquals(snapshot.symbolAt(DocOrd(0), 1, 8), None)

  test("symbolAt: between occurrences returns None"):
    assertEquals(snapshot.symbolAt(DocOrd(0), 2, 5), None)
    assertEquals(snapshot.symbolAt(DocOrd(0), 100, 0), None)
    assertEquals(snapshot.symbolAt(DocOrd(2), 0, 0), None) // doc without postings

  test("symbolAt: smallest covering occurrence wins"):
    val inner = snapshot.symbolAt(DocOrd(0), 3, 7).get
    assertEquals(inner.span, Span(3, 5, 3, 10))
    assertEquals(inner.symbolOrd.ord, sortedOrdOf(SymFoo))
    val outer = snapshot.symbolAt(DocOrd(0), 3, 2).get
    assertEquals(outer.span, Span(3, 0, 3, 20))
    assertEquals(outer.symbolOrd.ord, sortedOrdOf(SymB))

  test("symbolAt: equal-size tie goes to the earliest start"):
    val hit = snapshot.symbolAt(DocOrd(0), 4, 10).get
    assertEquals(hit.span, Span(4, 5, 4, 10))
    assertEquals(hit.symbolOrd.ord, sortedOrdOf(SymB))

  test("symbolAt reports the Definition role from flags"):
    val hit = snapshot.symbolAt(DocOrd(0), 0, 7).get
    assertEquals(hit.role, Role.Definition)
    assert(OccFlags.has(hit.flags, OccFlags.Definition))

  test("IndexSnapshot.using loans a retained snapshot"):
    val n = IndexSnapshot.using(snapshot)(_.docCount)
    assertEquals(n, 3)

class EmptySegmentTest extends munit.FunSuite:
  test("an empty segment round-trips"):
    val data = SegmentData(
      docs = Vector.empty,
      targets = Vector.empty,
      symbols = Vector.empty,
      refOccurrences = Vector.empty,
      defOccurrences = Vector.empty,
      renameOccurrences = Vector.empty,
      renameProfiles = Vector.empty,
      docOccurrences = Vector.empty
    )
    val dir = SegmentWriter.write(tempRoot("empty"), 1, data)
    val snapshot = new PostingsSnapshot(SegmentReader.open(dir))
    try
      assertEquals(snapshot.docCount, 0)
      assertEquals(snapshot.targetCount, 0)
      assertEquals(snapshot.symbolOrdOf("a/A#"), None)
      assertEquals(snapshot.docOrdOf("file:///x.scala"), None)
      assertEquals(snapshot.reader.occurrenceCount, 0L)
    finally
      snapshot.markSuperseded()
      snapshot.release()
