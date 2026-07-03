package ls.core

import java.nio.file.Files

import ls.index.{Role, Span}
import ls.pc.{DefinitionLocation, DefinitionOrigin, DefinitionResult}
import org.eclipse.lsp4j.{Location, Position, Range}

class PcOverlaySuite extends munit.FunSuite:

  private final class StubPc extends PcSymbolQueries:
    var open: Set[String] = Set.empty
    var prepareResult: Option[Range] = None
    var definitionResult: DefinitionResult = DefinitionResult.empty
    var throwOnPrepare: Boolean = false
    def isOpen(fileUri: String): Boolean = open.contains(fileUri)
    def prepareRename(fileUri: String, line: Int, character: Int): Option[Range] =
      if throwOnPrepare then throw new RuntimeException("pc crashed")
      prepareResult
    def definition(fileUri: String, line: Int, character: Int): DefinitionResult =
      definitionResult

  private def rangeOf(span: Span): Range =
    new Range(new Position(span.startLine, span.startChar), new Position(span.endLine, span.endChar))

  private def fixture(
      isIndexedName: String => Boolean = _ => true
  ): (PcOverlay, DocumentStore, StubPc, String, String) =
    val dir = Files.createTempDirectory("ls-core-overlay")
    val file = dir.resolve("A.scala")
    Files.writeString(file, "class A\n")
    val fileUri = Uris.toUri(file)
    val sdbUri = "src/A.scala"
    val docs = new DocumentStore
    val overlay = new PcOverlay(docs)
    val pc = new StubPc
    overlay.install(pc, u => if u == sdbUri then Some(fileUri) else None, isIndexedName)
    (overlay, docs, pc, fileUri, sdbUri)

  test("nothing is dirty before install"):
    val overlay = new PcOverlay(new DocumentStore)
    assert(!overlay.isDirty("src/A.scala"))
    assertEquals(overlay.symbolAt("src/A.scala", 0, 0), None)

  test("dirty tracking: dirty means buffer text differs from disk"):
    val (overlay, docs, _, fileUri, sdbUri) = fixture()
    assert(!overlay.isDirty(sdbUri), "closed file is not dirty")
    docs.open(fileUri, "class A\n")
    assert(!overlay.isDirty(sdbUri), "buffer equal to disk is not dirty")
    docs.change(fileUri, "class AB\n")
    assert(overlay.isDirty(sdbUri), "buffer differing from disk is dirty")
    docs.change(fileUri, "class A\n")
    assert(!overlay.isDirty(sdbUri), "typing back to the disk content clears dirtiness")
    docs.close(fileUri)
    assert(!overlay.isDirty(sdbUri))

  test("symbolAt: workspace definition at cursor span yields a Definition hit, not pcOnly"):
    val (overlay, docs, pc, fileUri, sdbUri) = fixture()
    docs.open(fileUri, "class AB\n")
    pc.open = Set(fileUri)
    val span = Span(0, 6, 0, 8)
    pc.prepareResult = Some(rangeOf(span))
    pc.definitionResult = DefinitionResult(
      "pkg/A#",
      Vector(DefinitionLocation(new Location(fileUri, rangeOf(span)), DefinitionOrigin.Workspace))
    )
    val hit = overlay.symbolAt(sdbUri, 0, 7).get
    assertEquals(hit.semanticSymbol, "pkg/A#")
    assertEquals(hit.span, span)
    assertEquals(hit.role, Role.Definition)
    assert(!hit.pcOnly)

  test("symbolAt: definition elsewhere yields a Reference hit"):
    val (overlay, docs, pc, fileUri, sdbUri) = fixture()
    docs.open(fileUri, "class AB\n")
    pc.open = Set(fileUri)
    pc.prepareResult = Some(rangeOf(Span(0, 6, 0, 8)))
    pc.definitionResult = DefinitionResult(
      "pkg/B#",
      Vector(
        DefinitionLocation(
          new Location("file:///elsewhere/B.scala", rangeOf(Span(3, 0, 3, 1))),
          DefinitionOrigin.Workspace
        )
      )
    )
    assertEquals(overlay.symbolAt(sdbUri, 0, 7).get.role, Role.Reference)

  test("symbolAt: synthetic/plugin-only definitions are pcOnly"):
    val (overlay, docs, pc, fileUri, sdbUri) = fixture()
    docs.open(fileUri, "class AB\n")
    pc.open = Set(fileUri)
    pc.prepareResult = Some(rangeOf(Span(0, 6, 0, 8)))
    pc.definitionResult = DefinitionResult(
      "gen/Ghost#",
      Vector(
        DefinitionLocation(
          new Location("file:///gen/Ghost.scala", rangeOf(Span(0, 0, 0, 5))),
          DefinitionOrigin.Synthetic
        ),
        DefinitionLocation(
          new Location("file:///gen/Other.scala", rangeOf(Span(0, 0, 0, 5))),
          DefinitionOrigin.Plugin
        )
      )
    )
    assert(overlay.symbolAt(sdbUri, 0, 7).get.pcOnly)

  test("symbolAt: mixed workspace + synthetic definitions are NOT pcOnly"):
    val (overlay, docs, pc, fileUri, sdbUri) = fixture()
    docs.open(fileUri, "class AB\n")
    pc.open = Set(fileUri)
    pc.prepareResult = Some(rangeOf(Span(0, 6, 0, 8)))
    pc.definitionResult = DefinitionResult(
      "pkg/A#",
      Vector(
        DefinitionLocation(
          new Location("file:///gen/Ghost.scala", rangeOf(Span(0, 0, 0, 5))),
          DefinitionOrigin.Synthetic
        ),
        DefinitionLocation(
          new Location("file:///real/A.scala", rangeOf(Span(1, 0, 1, 1))),
          DefinitionOrigin.Workspace
        )
      )
    )
    assert(!overlay.symbolAt(sdbUri, 0, 7).get.pcOnly)

  test("symbolAt falls back to the buffer token span when prepareRename declines"):
    val (overlay, docs, pc, fileUri, sdbUri) = fixture()
    docs.open(fileUri, "class AB\n")
    pc.open = Set(fileUri)
    pc.prepareResult = None // dotty PC: rename ranges only for file-local symbols
    pc.definitionResult = DefinitionResult(
      "pkg/AB#",
      Vector(
        DefinitionLocation(new Location(fileUri, rangeOf(Span(0, 6, 0, 8))), DefinitionOrigin.Workspace)
      )
    )
    val hit = overlay.symbolAt(sdbUri, 0, 7).get
    assertEquals(hit.semanticSymbol, "pkg/AB#")
    assertEquals(hit.span, Span(0, 6, 0, 8))
    assertEquals(hit.role, Role.Definition)
    // cursor past the last line: no identifier token, no answer
    assertEquals(overlay.symbolAt(sdbUri, 1, 0), None)

  test("symbolAt degrades to None when the PC cannot answer"):
    val (overlay, docs, pc, fileUri, sdbUri) = fixture()
    docs.open(fileUri, "class AB\n")

    // buffer not open in the PC facade
    pc.open = Set.empty
    pc.prepareResult = Some(rangeOf(Span(0, 6, 0, 8)))
    assertEquals(overlay.symbolAt(sdbUri, 0, 7), None)

    // prepareRename gives nothing
    pc.open = Set(fileUri)
    pc.prepareResult = None
    assertEquals(overlay.symbolAt(sdbUri, 0, 7), None)

    // definition resolves no symbol string
    pc.prepareResult = Some(rangeOf(Span(0, 6, 0, 8)))
    pc.definitionResult = DefinitionResult.empty
    assertEquals(overlay.symbolAt(sdbUri, 0, 7), None)

    // a crashing PC degrades instead of propagating
    pc.throwOnPrepare = true
    assertEquals(overlay.symbolAt(sdbUri, 0, 7), None)

    // unknown sdb uri
    pc.throwOnPrepare = false
    assertEquals(overlay.symbolAt("src/Unknown.scala", 0, 7), None)

  test("occurrencesOf contributes nothing in v1"):
    val (overlay, _, _, _, _) = fixture()
    assertEquals(overlay.occurrencesOf("pkg/A#"), None)

  // --- PC-only workspace-symbol dirty-buffer overlay ---

  test("a top-level symbol only in a dirty buffer surfaces PC-only"):
    // NewThing is in the open buffer but not in the persisted index.
    val (overlay, docs, pc, fileUri, sdbUri) = fixture(isIndexedName = _ => false)
    pc.open = Set(fileUri)
    docs.open(fileUri, "class A\nobject NewThing\n") // dirty: disk is "class A\n"
    // workspace/symbol merge candidate: matched, name not indexed
    val surfaced = overlay.pcOnlySymbols("NewThing")
    assertEquals(surfaced.map(_.name), Vector("NewThing"))
    assertEquals(surfaced.head.fileUri, fileUri)
    assertEquals(surfaced.head.keyword, "object")
    // references/rename cursor on the NewThing token -> PC-only hit (the engines
    // refuse it with LsError.PcOnlySymbol).
    val hit = overlay.symbolAt(sdbUri, 1, "object N".length)
    assert(hit.exists(_.pcOnly), s"expected a PC-only hit, got $hit")

  test("once the name is indexed (saved+ingested) it is no longer PC-only"):
    // Same buffer, but the index now knows NewThing (post save + ingest).
    val (overlay, docs, pc, fileUri, sdbUri) = fixture(isIndexedName = _ == "NewThing")
    pc.open = Set(fileUri)
    docs.open(fileUri, "class A\nobject NewThing\n")
    assertEquals(overlay.pcOnlySymbols("NewThing"), Vector.empty)
    val hit = overlay.symbolAt(sdbUri, 1, "object N".length)
    assert(!hit.exists(_.pcOnly), s"indexed symbol must not be PC-only, got $hit")

  test("a clean (saved) buffer contributes no PC-only symbols"):
    // A symbol present in the persisted index (buffer equals disk) is never
    // PC-only: pcOnlySymbols only scans dirty buffers.
    val (overlay, docs, pc, fileUri, _) = fixture(isIndexedName = _ => false)
    pc.open = Set(fileUri)
    docs.open(fileUri, "class A\n") // equals disk -> not dirty
    assertEquals(overlay.pcOnlySymbols("A"), Vector.empty)
