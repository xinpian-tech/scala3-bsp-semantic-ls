package ls.rename

import scala.concurrent.duration.Duration

import ls.index.*
import ls.rename.ingest.IngestReport

/** End-to-end references / workspace-symbol / highlight coverage over the
  * real-compiler fixture (three targets, B -> A edge, C disconnected).
  */
class ReferencesAndQuerySuite extends munit.FunSuite:

  override def munitTimeout: Duration = Duration(600, "s")

  private lazy val fx = FixtureWorkspace.master
  private lazy val stack = FixtureWorkspace.newStack()
  private lazy val report: IngestReport =
    stack.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
  private lazy val engine = ReferencesEngine(stack.orchestrator)
  private lazy val highlighter = DocumentHighlightService(stack.orchestrator)

  override def beforeAll(): Unit =
    val _ = report

  override def afterAll(): Unit = stack.close()

  private def refs(
      uri: String,
      token: String,
      nth: Int = 0,
      includeDeclaration: Boolean = false
  ): ReferencesResult =
    val (line, ch) = fx.cursor(uri, token, nth)
    engine.references(uri, line, ch, includeDeclaration)

  private def locsIn(result: ReferencesResult, uri: String): Vector[Loc] =
    result.locations.filter(_.uri == uri)

  private def containsToken(result: ReferencesResult, uri: String, token: String, nth: Int): Boolean =
    val span = fx.tokenSpan(uri, token, nth)
    result.locations.contains(Loc(uri, span))

  // ------------------------------------------------------------ ingest

  test("ingest report: all fixture docs indexed, shared doc counted, none stale"):
    assertEquals(report.docsIndexed, FixtureWorkspace.sources.size, report.toString)
    assertEquals(report.docsShared, 1, "shared/src/shared/Shared.scala compiled by A and B")
    assertEquals(report.docsStale, 0)
    assertEquals(report.docsSkipped, 0)
    assert(report.symbolCount > 0)
    assert(report.refGroupCount > 0)
    assertEquals(report.renameGroupCount, report.refGroupCount) // v1 policy

  // ------------------------------------------------------------ class/companion/constructor

  test("class references unify companion object and constructor across files and targets"):
    val r = refs("a/src/pkga/Core.scala", "Core", nth = 0)
    // constructor call `new Core(l)` in Core.scala
    assert(containsToken(r, "a/src/pkga/Core.scala", "Core", 2), r.locations.toString)
    // companion object use `Core.make("a")` and type ref in Impl.scala
    assert(locsIn(r, "a/src/pkga/Impl.scala").nonEmpty, r.locations.toString)
    // cross-target references in B are found
    assert(locsIn(r, "b/src/pkgb/UseB.scala").nonEmpty, r.locations.toString)

  test("target pruning: disconnected target C reusing pkga.Core is excluded"):
    val r = refs("a/src/pkga/Core.scala", "Core", nth = 0, includeDeclaration = true)
    assertEquals(
      locsIn(r, "c/src/pkga/CopyCore.scala"),
      Vector.empty,
      "references (and declarations) in fixture-c must be pruned: no dependency edge to fixture-a"
    )
    // sanity: C's own doc does reference the colliding symbols
    assert(fx.tokenSpans("c/src/pkga/CopyCore.scala", "Core").nonEmpty)

  test("apply-sugar unification: case class references include Item(1), Item.apply(2), new Item(3)"):
    val r = refs("a/src/pkga/Item.scala", "Item", nth = 0)
    assert(containsToken(r, "a/src/pkga/Item.scala", "Item", 1), r.locations.toString) // Item(1)
    assert(containsToken(r, "a/src/pkga/Item.scala", "Item", 2), r.locations.toString) // Item.apply(2)
    assert(containsToken(r, "a/src/pkga/Item.scala", "Item", 3), r.locations.toString) // new Item(3)
    assert(locsIn(r, "b/src/pkgb/UseB.scala").nonEmpty, r.locations.toString)

  // ------------------------------------------------------------ trait/object/enum

  test("trait references (Greeter) reach the extends clause and cross-target signatures"):
    val r = refs("a/src/pkga/Core.scala", "Greeter", nth = 0)
    assert(containsToken(r, "a/src/pkga/Impl.scala", "Greeter", 0), r.locations.toString)
    assert(containsToken(r, "b/src/pkgb/UseB.scala", "Greeter", 0), r.locations.toString)

  test("object references (SharedThing) from the shared source"):
    val r = refs("shared/src/shared/Shared.scala", "SharedThing", nth = 0)
    assert(containsToken(r, "b/src/pkgb/UseB.scala", "SharedThing", 0), r.locations.toString)

  test("enum references (Color) include the cross-target type and case use"):
    val r = refs("a/src/pkga/Core.scala", "Color", nth = 0)
    assert(containsToken(r, "b/src/pkgb/UseB.scala", "Color", 0), r.locations.toString)
    assert(containsToken(r, "b/src/pkgb/UseB.scala", "Color", 1), r.locations.toString)

  // ------------------------------------------------------------ methods

  test("method overloads stay separate"):
    val r = refs("a/src/pkga/Over.scala", "fmt", nth = 0) // def fmt(i: Int)
    val fmtSpans = fx.tokenSpans("a/src/pkga/Over.scala", "fmt")
    val locs = locsIn(r, "a/src/pkga/Over.scala")
    assert(locs.contains(Loc("a/src/pkga/Over.scala", fmtSpans(2))), r.locations.toString) // fmt(1)
    assert(!locs.contains(Loc("a/src/pkga/Over.scala", fmtSpans(1))), "other overload def")
    assert(!locs.contains(Loc("a/src/pkga/Over.scala", fmtSpans(3))), "fmt(\"x\") call")

  test("references through an export forwarder are found"):
    // cursor on the OriginalOwner.exported definition; references must include
    // the ForwarderOwner.exported(3) forwarder call site (nth=2 whole-word
    // "exported": 0=def, 1=export clause, 2=call)
    val r = refs("a/src/pkga/Exported.scala", "exported", nth = 0)
    assert(
      containsToken(r, "a/src/pkga/Exported.scala", "exported", 2),
      r.locations.toString
    )

  test("var getter and setter are unified"):
    val r = refs("a/src/pkga/Vars.scala", "value", nth = 1) // read: c.value + 1
    val spans = fx.tokenSpans("a/src/pkga/Vars.scala", "value")
    val locs = locsIn(r, "a/src/pkga/Vars.scala")
    assert(locs.exists(_.span == spans(1)), r.locations.toString) // read
    assert(locs.exists(_.span.startLine == spans(2).startLine), r.locations.toString) // write via setter

  test("local val references stay inside the document"):
    val r = refs("a/src/pkga/Vars.scala", "tmp", nth = 0, includeDeclaration = true)
    val spans = fx.tokenSpans("a/src/pkga/Vars.scala", "tmp")
    assertEquals(
      locsIn(r, "a/src/pkga/Vars.scala").map(_.span).toSet,
      spans.toSet,
      r.locations.toString
    )
    assertEquals(r.locations.map(_.uri).distinct, Vector("a/src/pkga/Vars.scala"))

  test("extension method references cross targets"):
    val r = refs("a/src/pkga/Core.scala", "shout", nth = 0)
    assert(containsToken(r, "a/src/pkga/Impl.scala", "shout", 0), r.locations.toString)
    assert(containsToken(r, "b/src/pkgb/UseB.scala", "shout", 0), r.locations.toString)

  test("given references by name"):
    val r = refs("a/src/pkga/Core.scala", "defaultCore", nth = 0)
    assert(containsToken(r, "a/src/pkga/Impl.scala", "defaultCore", 0), r.locations.toString)
    assert(containsToken(r, "b/src/pkgb/UseB.scala", "defaultCore", 0), r.locations.toString)

  test("given references are exactly the by-name uses including the using-clause site"):
    val r = refs("a/src/pkga/Core.scala", "defaultCore", nth = 0) // references only
    val useFiles = Set("a/src/pkga/Impl.scala", "b/src/pkgb/UseB.scala", "a/src/pkga/Using.scala")
    for uri <- useFiles do
      assertEquals(
        locsIn(r, uri).map(_.span).toSet,
        fx.tokenSpans(uri, "defaultCore").toSet,
        s"$uri: ${r.locations}"
      )
    assertEquals(r.locations.map(_.uri).toSet, useFiles, r.locations.toString)

  test("inline def references are exactly the definition and both call sites"):
    val r = refs("a/src/pkga/Inline.scala", "twice", nth = 0, includeDeclaration = true)
    assertEquals(
      locsIn(r, "a/src/pkga/Inline.scala").map(_.span).toSet,
      fx.tokenSpans("a/src/pkga/Inline.scala", "twice").toSet,
      r.locations.toString
    )
    assertEquals(
      locsIn(r, "b/src/pkgb/UseB.scala").map(_.span).toSet,
      fx.tokenSpans("b/src/pkgb/UseB.scala", "twice").toSet
    )
    assertEquals(
      r.locations.map(_.uri).toSet,
      Set("a/src/pkga/Inline.scala", "b/src/pkgb/UseB.scala")
    )

  test("top-level def and val references are exactly their definitions and cross-file uses"):
    for token <- Vector("topHelper", "topConst") do
      val r = refs("a/src/pkga/TopLevel.scala", token, nth = 0, includeDeclaration = true)
      assertEquals(
        locsIn(r, "a/src/pkga/TopLevel.scala").map(_.span).toSet,
        fx.tokenSpans("a/src/pkga/TopLevel.scala", token).toSet,
        s"$token: ${r.locations}"
      )
      assertEquals(
        locsIn(r, "b/src/pkgb/UseB.scala").map(_.span).toSet,
        fx.tokenSpans("b/src/pkgb/UseB.scala", token).toSet,
        s"$token cross-file"
      )
      assertEquals(
        r.locations.map(_.uri).toSet,
        Set("a/src/pkga/TopLevel.scala", "b/src/pkgb/UseB.scala"),
        token
      )

  test("cross-file val member references are exactly the definition and cross-file use"):
    val r = refs("a/src/pkga/Named.scala", "title", nth = 0, includeDeclaration = true)
    assertEquals(
      locsIn(r, "a/src/pkga/Named.scala").map(_.span).toSet,
      fx.tokenSpans("a/src/pkga/Named.scala", "title").toSet,
      r.locations.toString
    )
    assertEquals(
      locsIn(r, "b/src/pkgb/UseB.scala").map(_.span).toSet,
      fx.tokenSpans("b/src/pkgb/UseB.scala", "title").toSet
    )
    assertEquals(r.locations.map(_.uri).toSet, Set("a/src/pkga/Named.scala", "b/src/pkgb/UseB.scala"))

  test("private member references are exactly the in-file definition and uses"):
    for token <- Vector("helper", "state") do
      val r = refs("a/src/pkga/Private.scala", token, nth = 0, includeDeclaration = true)
      assertEquals(r.locations.map(_.uri).distinct, Vector("a/src/pkga/Private.scala"), token)
      assertEquals(
        locsIn(r, "a/src/pkga/Private.scala").map(_.span).toSet,
        fx.tokenSpans("a/src/pkga/Private.scala", token).toSet,
        s"$token: ${r.locations}"
      )

  test("nested local def references stay inside the document"):
    val r = refs("a/src/pkga/LocalDef.scala", "loop", nth = 0, includeDeclaration = true)
    assertEquals(r.locations.map(_.uri).distinct, Vector("a/src/pkga/LocalDef.scala"))
    assertEquals(
      locsIn(r, "a/src/pkga/LocalDef.scala").map(_.span).toSet,
      fx.tokenSpans("a/src/pkga/LocalDef.scala", "loop").toSet,
      r.locations.toString
    )

  test("opaque type references are exactly the type, companion, and all in-file uses"):
    val r = refs("a/src/pkga/Opaque.scala", "UserId", nth = 0, includeDeclaration = true)
    assertEquals(r.locations.map(_.uri).distinct, Vector("a/src/pkga/Opaque.scala"))
    assertEquals(
      locsIn(r, "a/src/pkga/Opaque.scala").map(_.span).toSet,
      fx.tokenSpans("a/src/pkga/Opaque.scala", "UserId").toSet,
      r.locations.toString
    )

  // ------------------------------------------------------------ includeDeclaration

  test("includeDeclaration adds the definition site and only then"):
    val defSpan = fx.tokenSpan("a/src/pkga/Item.scala", "Item", 0)
    val without = refs("a/src/pkga/Item.scala", "Item", nth = 1)
    val withDecl = refs("a/src/pkga/Item.scala", "Item", nth = 1, includeDeclaration = true)
    assert(!without.locations.contains(Loc("a/src/pkga/Item.scala", defSpan)))
    assert(withDecl.locations.contains(Loc("a/src/pkga/Item.scala", defSpan)))
    assert(withDecl.hits.exists(h => h.loc.span == defSpan && h.role == Role.Definition))
    assert(withDecl.locations.length > without.locations.length)

  test("results are deduped and sorted by (uri, position)"):
    val r = refs("a/src/pkga/Item.scala", "Item", nth = 0, includeDeclaration = true)
    assertEquals(r.locations, r.locations.distinct)
    val keys = r.locations.map(l =>
      (l.uri, l.span.startLine, l.span.startChar, l.span.endLine, l.span.endChar)
    )
    assertEquals(keys, keys.sorted)

  // ------------------------------------------------------------ workspace symbol

  test("workspace symbol FTS prefix query end-to-end"):
    val hits = stack.orchestrator.workspaceSymbol("Cor")
    assert(hits.exists(_.displayName == "Core"), hits.toString)
    val shared = stack.orchestrator.workspaceSymbol("SharedTh")
    assert(shared.exists(_.displayName == "SharedThing"), shared.toString)
    assertEquals(stack.orchestrator.workspaceSymbol("NoSuchSymbolXyz"), Vector.empty)

  // ------------------------------------------------------------ document highlight

  test("document highlight splits read/write by role"):
    val spans = fx.tokenSpans("a/src/pkga/Vars.scala", "value")
    val (line, ch) = fx.cursor("a/src/pkga/Vars.scala", "value", 1)
    val hs = highlighter.highlights("a/src/pkga/Vars.scala", line, ch)
    assert(hs.exists(h => h.span == spans(0) && h.kind == HighlightKind.Write), hs.toString)
    assert(hs.exists(h => h.span == spans(1) && h.kind == HighlightKind.Read), hs.toString)
    assert(hs.count(_.kind == HighlightKind.Write) >= 1)
    assert(hs.count(_.kind == HighlightKind.Read) >= 2, hs.toString) // read + setter site
    // same-document only by construction; sorted
    val keys = hs.map(h => (h.span.startLine, h.span.startChar))
    assertEquals(keys, keys.sorted)

  // ------------------------------------------------------------ overlay hooks

  test("overlay contributes distinctly-marked dirty-buffer references"):
    val extra = Loc("virtual/Dirty.scala", Span(0, 0, 0, 4))
    val overlay = new DirtyBufferOverlay:
      override def isDirty(uri: String): Boolean = false
      override def symbolAt(uri: String, line: Int, character: Int): Option[OverlayHit] = None
      override def contributesOccurrences: Boolean = true
      override def occurrencesOf(semanticSymbol: String): Option[Vector[Loc]] =
        if semanticSymbol == "pkga/Item#" then Some(Vector(extra)) else None
    val stack2 = FixtureWorkspace.newStack(overlay)
    try
      stack2.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      val engine2 = ReferencesEngine(stack2.orchestrator)
      val (line, ch) = fx.cursor("a/src/pkga/Item.scala", "Item", 0)
      val r = engine2.references("a/src/pkga/Item.scala", line, ch, includeDeclaration = false)
      val overlayHits = r.hits.filter(_.fromOverlay)
      assertEquals(overlayHits.map(_.loc), Vector(extra))
      assert(r.hits.exists(!_.fromOverlay))
    finally stack2.close()

  test("overlay hits keyed to any alias-group member are merged, not just the cursor symbol"):
    // The cursor is on the class `pkga/Item#`, but the overlay only knows an
    // occurrence keyed to a companion-side member (`pkga/Item.` / `.apply`).
    // A group-keyed fan-out must still surface it; a cursor-symbol-only query
    // (the pre-fix behaviour) would miss it entirely.
    val extra = Loc("virtual/Dirty.scala", Span(1, 2, 1, 6))
    val queried = scala.collection.mutable.Set.empty[String]
    val overlay = new DirtyBufferOverlay:
      override def isDirty(uri: String): Boolean = false
      override def symbolAt(uri: String, line: Int, character: Int): Option[OverlayHit] = None
      override def contributesOccurrences: Boolean = true
      override def occurrencesOf(semanticSymbol: String): Option[Vector[Loc]] =
        queried += semanticSymbol
        // companion object and its members start with "pkga/Item."; the class
        // itself is "pkga/Item#" and must NOT match.
        if semanticSymbol.startsWith("pkga/Item.") then Some(Vector(extra)) else None
    val stack2 = FixtureWorkspace.newStack(overlay)
    try
      stack2.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      val engine2 = ReferencesEngine(stack2.orchestrator)
      val (line, ch) = fx.cursor("a/src/pkga/Item.scala", "Item", 0)
      val r = engine2.references("a/src/pkga/Item.scala", line, ch, includeDeclaration = false)
      val overlayHits = r.hits.filter(_.fromOverlay)
      assertEquals(overlayHits.map(_.loc), Vector(extra), s"queried=$queried")
      // The fan-out queried the cursor symbol AND at least one companion member.
      assert(queried.contains("pkga/Item#"), s"queried=$queried")
      assert(
        queried.exists(s => s != "pkga/Item#" && s.startsWith("pkga/Item.")),
        s"group fan-out must query companion members; queried=$queried"
      )
    finally stack2.close()

  test("dirty buffer: overlay answers symbolAtCursor; unanswerable dirty query degrades"):
    val dirtyUri = "a/src/pkga/Item.scala"
    var answer: Option[OverlayHit] = None
    val overlay = new DirtyBufferOverlay:
      override def isDirty(uri: String): Boolean = uri == dirtyUri
      override def symbolAt(uri: String, line: Int, character: Int): Option[OverlayHit] = answer
      override def occurrencesOf(semanticSymbol: String): Option[Vector[Loc]] = None
    val stack2 = FixtureWorkspace.newStack(overlay)
    try
      stack2.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      answer = Some(OverlayHit("pkga/Item#", Span(2, 11, 2, 15), Role.Definition))
      val cursor = stack2.orchestrator.symbolAtCursor(dirtyUri, 2, 12)
      assertEquals(cursor.source, ResolutionSource.Overlay)
      assertEquals(cursor.semanticSymbol, "pkga/Item#")
      answer = None
      val err = intercept[LsException](stack2.orchestrator.symbolAtCursor(dirtyUri, 2, 12))
      assert(err.error.isInstanceOf[LsError.StaleIndex], err.error.toString)
    finally stack2.close()

  // ------------------------------------------------------------ errors

  test("no symbol at cursor throws NoSymbolAtCursor"):
    val err = intercept[LsException](
      engine.references("a/src/pkga/Item.scala", 1, 0, includeDeclaration = false)
    )
    assert(err.error.isInstanceOf[LsError.NoSymbolAtCursor], err.error.toString)

  test("unknown uri throws NotIndexed"):
    val err = intercept[LsException](
      engine.references("nope/Missing.scala", 0, 0, includeDeclaration = false)
    )
    assert(err.error.isInstanceOf[LsError.NotIndexed], err.error.toString)

  // ------------------------------------------------------------ re-ingest supersede

  test("re-running ingest supersedes cleanly: new snapshot, same answers, old segment reclaimable"):
    val before = stack.manager.current().get
    val beforeId = before.snapshotId
    before.release()
    val r1 = refs("a/src/pkga/Item.scala", "Item", nth = 0).locations

    val report2 = stack.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
    assert(report2.segmentId > beforeId)
    val after = stack.manager.current().get
    assert(after.snapshotId > beforeId)
    after.release()

    val r2 = refs("a/src/pkga/Item.scala", "Item", nth = 0).locations
    assertEquals(r2, r1, "identical corpus must produce identical references")

    // The publish tail already reclaimed the drained old segment, so a manual
    // pass finds nothing left.
    assertEquals(stack.manager.deleteSuperseded(), Nil, "old segment auto-reclaimed at publish tail")
