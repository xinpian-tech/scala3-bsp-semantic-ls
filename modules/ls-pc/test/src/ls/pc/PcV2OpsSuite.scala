package ls.pc

import scala.concurrent.duration.*

import org.eclipse.lsp4j.{Position, Range, TextEdit}

/** End-to-end coverage of the ABI v2 payload-query providers at the facade
  * level, against the real Scala 3 presentation compiler (shared [[SharedPc]]
  * instance, scala-library classpath): per op a happy case with the exact
  * expected shape, an empty/None case, and for codeAction the
  * `DisplayableException` refusal case (data, not an error).
  */
class PcV2OpsSuite extends munit.FunSuite:
  override def munitTimeout: Duration = 5.minutes

  import SharedPc.{facade, openBuffer}

  private def applyEdits(text: String, edits: Vector[TextEdit]): String =
    // apply in reverse document order so earlier offsets stay valid; at a tied
    // start a zero-width insert sorts before the replacement (LSP edit order),
    // so reversed application replaces first and inserts second
    edits
      .sortBy(e =>
        (
          e.getRange.getStart.getLine,
          e.getRange.getStart.getCharacter,
          e.getRange.getEnd.getLine,
          e.getRange.getEnd.getCharacter
        )
      )
      .reverse
      .foldLeft(text) { (acc, e) =>
        val start = Utf16Text.offsetAt(acc, e.getRange.getStart.getLine, e.getRange.getStart.getCharacter)
        val end = Utf16Text.offsetAt(acc, e.getRange.getEnd.getLine, e.getRange.getEnd.getCharacter)
        acc.substring(0, start) + e.getNewText + acc.substring(end)
      }

  private def fullRange(text: String): Range =
    val (line, character) = Utf16Text.positionAt(text, text.length)
    new Range(new Position(0, 0), new Position(line, character))

  // --- inlayHints -----------------------------------------------------------

  test("inlayHints: inferred-type hint with exact position, parts, and flag gating"):
    val text = "object InlayHappy:\n  val xs = List(1)\n"
    val uri = openBuffer(text)
    val hints = facade.inlayHints(uri, fullRange(text), PcInlayHintFlags.InferredTypes)
    assertEquals(hints.size, 1)
    val hint = hints.head
    assertEquals(hint.position, new Position(1, "  val xs".length))
    assertEquals(hint.labelParts.map(_.text).mkString, ": List[Int]")
    assertEquals(hint.kind, 1) // InlayHintKind.Type
    assert(hint.data.nonEmpty, "expected opaque data bytes to round-trip")

    // the same buffer with the inferred-types bit unset answers empty
    assertEquals(facade.inlayHints(uri, fullRange(text), 0), Vector.empty[PcInlayHint])

  // --- semanticTokens -------------------------------------------------------

  test("semanticTokens: offset nodes cover the declared names; empty buffer answers empty"):
    val text = "object SemTok:\n  val alpha = 1\n"
    val uri = openBuffer(text)
    val nodes = facade.semanticTokens(uri)
    assert(nodes.nonEmpty, "expected semantic tokens")
    val texts = nodes.map(n => text.substring(n.start, n.end))
    assert(texts.contains("SemTok"), s"missing SemTok token in: $texts")
    assert(texts.contains("alpha"), s"missing alpha token in: $texts")
    // offsets are ordered and in range
    nodes.foreach(n => assert(0 <= n.start && n.start < n.end && n.end <= text.length))

    val empty = openBuffer("\n")
    assertEquals(facade.semanticTokens(empty), Vector.empty[PcSemanticNode])

  // --- selectionRanges ------------------------------------------------------

  test("selectionRanges: innermost-first widening chain per position; no positions, no chains"):
    val text = "object Sel:\n  val x = 1 + 2\n"
    val uri = openBuffer(text)
    val onOne = new Position(1, "  val x = 1".length - 1) // on the literal `1`
    val chains = facade.selectionRanges(uri, Vector(onOne, onOne))
    assertEquals(chains.size, 2)
    val chain = chains.head
    assert(chain.nonEmpty, "expected a selection chain")
    // innermost first: the literal, then a widening chain of enclosing ranges
    assertEquals(chain.head.getStart, new Position(1, "  val x = ".length))
    assertEquals(chain.head.getEnd, new Position(1, "  val x = 1".length))
    chain.sliding(2).foreach {
      case Vector(inner, outer) =>
        val startsBeforeOrAt =
          outer.getStart.getLine < inner.getStart.getLine ||
            (outer.getStart.getLine == inner.getStart.getLine &&
              outer.getStart.getCharacter <= inner.getStart.getCharacter)
        val endsAfterOrAt =
          outer.getEnd.getLine > inner.getEnd.getLine ||
            (outer.getEnd.getLine == inner.getEnd.getLine &&
              outer.getEnd.getCharacter >= inner.getEnd.getCharacter)
        assert(startsBeforeOrAt && endsAfterOrAt, s"chain not widening: $inner then $outer")
      case _ => ()
    }
    assertEquals(facade.selectionRanges(uri, Vector.empty), Vector.empty[Vector[Range]])

  // --- codeAction -----------------------------------------------------------

  test("codeAction ConvertToNamedArguments: the call gains its parameter names"):
    val text = "object CA:\n  def m(a: Int, b: Int): Int = a\n  val r = m(1, 2)\n"
    val uri = openBuffer(text)
    val result = facade.codeAction(
      uri,
      PcCodeActionId.ConvertToNamedArguments,
      new Position(2, "  val r = m(1, 2)".length), // at the end of the call
      None,
      Some(Vector(0, 1))
    )
    assertEquals(result.refusal, None)
    assertEquals(
      applyEdits(text, result.edits),
      "object CA:\n  def m(a: Int, b: Int): Int = a\n  val r = m(a = 1, b = 2)\n"
    )

  test("codeAction ImplementAbstractMembers: the stub member is inserted"):
    val text = "object IAM:\n  trait T:\n    def f: Int\n  class C extends T\n"
    val uri = openBuffer(text)
    val result = facade.codeAction(
      uri,
      PcCodeActionId.ImplementAbstractMembers,
      new Position(3, "  class C".length - 1), // on `C`
      None,
      None
    )
    assertEquals(result.refusal, None)
    assert(result.edits.nonEmpty, "expected implement-abstract-members edits")
    val applied = applyEdits(text, result.edits)
    assert(applied.contains("def f: Int = ???"), applied)

  test("codeAction ExtractMethod: the selection becomes a new method"):
    val text = "object EM:\n  def host: Int =\n    val a = 1\n    a + 2\n"
    val uri = openBuffer(text)
    val result = facade.codeAction(
      uri,
      PcCodeActionId.ExtractMethod,
      new Position(3, "    ".length), // selection start: `a + 2`
      Some(new Position(3, "    a + 2".length)), // selection end
      None
    )
    assertEquals(result.refusal, None)
    assert(result.edits.nonEmpty, "expected extract-method edits")
    val applied = applyEdits(text, result.edits)
    assert(applied.contains("def newMethod"), applied)

  test("codeAction InlineValue: a local value inlines into its use"):
    val text = "object InlOk:\n  def m: Int =\n    val local = 1\n    local + 2\n"
    val uri = openBuffer(text)
    val result = facade.codeAction(
      uri,
      PcCodeActionId.InlineValue,
      new Position(2, "    val lo".length), // on the local definition
      None,
      None
    )
    assertEquals(result.refusal, None)
    assertEquals(
      applyEdits(text, result.edits),
      "object InlOk:\n  def m: Int =\n    1 + 2\n"
    )

  test("codeAction InlineValue refusal: DisplayableException comes back as data"):
    val text = "object InlRef:\n  val x = 1\n  val y = x + x\n"
    val uri = openBuffer(text)
    val result = facade.codeAction(
      uri,
      PcCodeActionId.InlineValue,
      new Position(1, "  val ".length), // on the non-local definition
      None,
      None
    )
    assertEquals(result.edits, Vector.empty[TextEdit])
    assertEquals(result.refusal, Some("Non-local value cannot be inlined."))

  test("codeAction InsertInferredType: the val gains its type; a non-binding position answers empty"):
    val text = "object IIT:\n  val x = 1\n"
    val uri = openBuffer(text)
    val result = facade.codeAction(
      uri,
      PcCodeActionId.InsertInferredType,
      new Position(1, "  val ".length), // on `x`
      None,
      None
    )
    assertEquals(result.refusal, None)
    assertEquals(
      applyEdits(text, result.edits),
      "object IIT:\n  val x: Int = 1\n"
    )
    // the empty case: the object name binds no inferrable type
    val none = facade.codeAction(
      uri,
      PcCodeActionId.InsertInferredType,
      new Position(0, "object I".length),
      None,
      None
    )
    assertEquals(none, PcCodeActionResult(Vector.empty))

  test("codeAction InsertInferredMethod: the missing method is scaffolded"):
    val text = "object IIM:\n  def run: Int = missingMethod(1)\n"
    val uri = openBuffer(text)
    val result = facade.codeAction(
      uri,
      PcCodeActionId.InsertInferredMethod,
      new Position(1, "  def run: Int = mi".length),
      None,
      None
    )
    assertEquals(result.refusal, None)
    assert(result.edits.nonEmpty, "expected insert-inferred-method edits")
    val applied = applyEdits(text, result.edits)
    assert(applied.contains("def missingMethod"), applied)

  test("codeAction ConvertToNamedLambdaParameters: the placeholder lambda gains a name"):
    val text = "object CNL:\n  val f = List(1).map(_ + 1)\n"
    val uri = openBuffer(text)
    val result = facade.codeAction(
      uri,
      PcCodeActionId.ConvertToNamedLambdaParameters,
      new Position(1, "  val f = List(1).map(_".length), // on the placeholder
      None,
      None
    )
    assertEquals(result.refusal, None)
    assert(result.edits.nonEmpty, "expected convert-to-named-lambda-parameters edits")
    val applied = applyEdits(text, result.edits)
    assert(applied.contains("=>"), applied)

  test("codeAction with an unknown action id fails fast"):
    val uri = openBuffer("object BadAction\n")
    intercept[IllegalArgumentException](
      facade.codeAction(uri, 99, new Position(0, 0), None, None)
    )

  // --- autoImports ----------------------------------------------------------

  test("autoImports: a known class offers its import; an unknown name offers none"):
    val text = "object AI:\n  val b = new ArrayBuffer[Int]()\n"
    val uri = openBuffer(text)
    val position = new Position(1, "  val b = new Ar".length)
    val imports = facade.autoImports(uri, position, "ArrayBuffer", isExtension = false)
    assert(imports.nonEmpty, "expected auto-import candidates")
    val mutablePkg = imports.find(_.packageName == "scala.collection.mutable")
    assert(mutablePkg.isDefined, s"missing scala.collection.mutable in: ${imports.map(_.packageName)}")
    val applied = applyEdits(text, mutablePkg.get.edits)
    assert(applied.contains("import scala.collection.mutable.ArrayBuffer"), applied)

    assertEquals(
      facade.autoImports(uri, position, "NoSuchClazz123456", isExtension = false),
      Vector.empty[PcAutoImport]
    )

  // --- pcDiagnostics (the facade diagnostics path the op routes through) ----

  test("pcDiagnostics: a type error surfaces; a clean buffer answers empty"):
    val bad = openBuffer("object DiagBad:\n  val x: Int = \"nope\"\n")
    val diags = facade.diagnostics(bad)
    assert(diags.nonEmpty, "expected a type-error diagnostic")
    assert(diags.exists(_.getSeverity == org.eclipse.lsp4j.DiagnosticSeverity.Error), diags.toString)

    val clean = openBuffer("object DiagClean:\n  val x: Int = 1\n")
    assertEquals(facade.diagnostics(clean), Vector.empty)

  // --- foldingRanges (facade wiring; the provider is pinned in its own suite) --

  test("foldingRanges: the facade folds the open buffer text without a PC round-trip"):
    val uri = openBuffer("object Fold:\n  def f: Int =\n    val x = 1\n    x\n")
    val ranges = facade.foldingRanges(uri)
    assert(ranges.nonEmpty, "expected folding ranges")
    assertEquals(ranges.head.range.getStart.getLine, 0)
    assertEquals(ranges.head.kind, 0)
