/*
 * Harness ported from the Scala 3 ("dotty") presentation-compiler test suite,
 * version 3.8.4 (presentation-compiler/test/dotty/tools/pc/base/BasePCSuite.scala,
 * BaseCompletionSuite.scala, BaseHoverSuite.scala, BaseSignatureHelpSuite.scala,
 * BasePcDefinitionSuite.scala, BaseInlayHintsSuite.scala,
 * BaseSelectionRangeSuite.scala, BaseSemanticTokensSuite.scala,
 * BaseDiagnosticsSuite.scala, BaseCodeActionSuite.scala,
 * BaseAutoImportsSuite.scala, BaseExtractMethodSuite.scala and
 * utils/{TestHovers,TestCompletions,TextEdits,
 * RangeReplace,TestExtensions,TestInlayHints,TestSemanticTokens}.scala):
 *   https://github.com/scala/scala3/tree/3.8.4/presentation-compiler
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed from JUnit 4 onto munit, and from a directly
 * constructed ScalaPresentationCompiler onto the ls.pc.PcFacade LSP surface
 * (LSP line/character positions instead of offsets; no compat maps).
 */
package ls.pc.corpus

import java.util.Collections

import scala.concurrent.duration.*
import scala.jdk.CollectionConverters.*

import com.google.gson.{Gson, JsonElement, JsonParser}
import ls.pc.{
  PcAutoImport,
  PcCodeActionId,
  PcCodeActionResult,
  PcInlayHint,
  PcInlayHintFlags,
  PcSemanticNode,
  Utf16Text
}
import org.eclipse.lsp4j.{
  CompletionItem,
  Diagnostic,
  DiagnosticSeverity,
  Hover,
  Location,
  Position,
  Range,
  SignatureHelp,
  TextEdit
}
import org.eclipse.lsp4j.jsonrpc.messages.Either as JEither
import scala.collection.mutable.ListBuffer
import scala.meta.internal.pc.{CompletionItemData, HoverMarkup, InlayHints, SemanticTokens}

/** Base of all ported corpus suites: cursor-marker parsing, offset/position
  * conversion, text-edit application and shared rendering helpers.
  */
abstract class CorpusSuiteBase extends munit.FunSuite:

  override def munitTimeout: Duration = 5.minutes

  /** Dotty `PcAssertions` normalization: drop leading blank lines and trim the
    * whole string, so blank-line artifacts of `stripMargin` expected strings
    * never fail a comparison.
    */
  private def unifyNewlines(str: String): String =
    str.linesIterator.dropWhile(_.trim.isEmpty).mkString("\n").trim

  def assertCorpusNoDiff(obtained: String, expected: String)(implicit loc: munit.Location): Unit =
    assertNoDiff(unifyNewlines(obtained), unifyNewlines(expected))

  // --- cursor markers (dotty BasePCSuite.params / hoverParams) ---------------

  /** Strip the `@@` cursor marker and return `(code, offset)`. */
  def params(code: String): (String, Int) =
    val code2 = code.replace("@@", "")
    val offset = code.indexOf("@@")
    if offset < 0 then fail("missing @@")
    (code2, offset)

  /** Strip `@@` / `%<%` / `%>%` and return `(code, startOffset, endOffset)`;
    * a point query has `start == end`.
    */
  def hoverParams(code: String): (String, Int, Int) =
    val code2 = code.replace("@@", "").replace("%<%", "").replace("%>%", "")
    val positionOffset = code.replace("%<%", "").replace("%>%", "").indexOf("@@")
    val startOffset = code.replace("@@", "").indexOf("%<%")
    val endOffset = code.replace("@@", "").replace("%<%", "").indexOf("%>%")
    (positionOffset, startOffset, endOffset) match
      case (po, so, eo) if po < 0 && so < 0 && eo < 0 =>
        fail("missing @@ and (%<% and %>%)")
      case (_, so, eo) if so >= 0 && eo >= 0 => (code2, so, eo)
      case (po, _, _) => (code2, po, po)

  extension (s: String)
    def triplequoted: String = s.replace("'''", "\"\"\"")

    def removeRanges: String =
      s.replace("<<", "")
        .replace(">>", "")
        .replaceAll("/\\*.+\\*/", "")

    def removePos: String = s.replace("@@", "")

  // --- LSP position conversion ----------------------------------------------

  /** Java string index -> zero-based LSP (line, UTF-16 character). */
  def offsetToLsp(text: String, offset: Int): (Int, Int) =
    var line = 0
    var col = 0
    var i = 0
    while i < offset do
      if text.charAt(i) == '\n' then
        line += 1
        col = 0
      else col += 1
      i += 1
    (line, col)

  /** LSP position -> Java string index (dotty TestExtensions.getOffset). */
  def positionOffset(pos: Position, text: String): Int =
    val lines = text.linesWithSeparators.toList
    lines.take(pos.getLine).foldRight(0)(_.length + _) + pos.getCharacter

  // --- text edits (dotty utils/TextEdits.scala) ------------------------------

  def applyEdits(text: String, edits: List[TextEdit]): String =
    if edits.isEmpty then text
    else
      val positions = edits
        .flatMap(edit => Option(edit.getRange).map(edit -> _))
        // The end position joins the sort key (the one departure from the
        // dotty original, which sorts by start only): at a TIED start the
        // zero-width insert applies before the replacement — the LSP
        // ordering rule for an insert and a replace at the same position.
        // The extract-method carrier produces exactly that pair when the
        // extraction anchors at the selection's own statement.
        .sortBy((_, range) =>
          (
            range.getStart.getLine,
            range.getStart.getCharacter,
            range.getEnd.getLine,
            range.getEnd.getCharacter
          )
        )
      var curr = 0
      val out = new java.lang.StringBuilder()
      positions.foreach { case (edit, pos) =>
        out.append(text, curr, positionOffset(pos.getStart, text))
        out.append(edit.getNewText)
        curr = positionOffset(pos.getEnd, text)
      }
      out.append(text, curr, text.length)
      out.toString

  def applyEdits(text: String, item: CompletionItem): String =
    val edits = getLeftTextEdit(item).toList ++
      Option(item.getAdditionalTextEdits).toList.flatMap(_.asScala)
    applyEdits(text, edits)

  def getLeftTextEdit(item: CompletionItem): Option[TextEdit] =
    for
      either <- Option(item.getTextEdit)
      textEdit <- Option(either.getLeft)
    yield textEdit

  // --- range markers (dotty utils/RangeReplace.scala) ------------------------

  protected def replaceInRange(
      base: String,
      range: Range,
      prefix: String = "<<",
      suffix: String = ">>"
  ): String =
    val posStart = positionOffset(range.getStart, base)
    val posEnd = positionOffset(range.getEnd, base)
    new java.lang.StringBuilder()
      .append(base, 0, posStart)
      .append(prefix)
      .append(base, posStart, posEnd)
      .append(suffix)
      .append(base, posEnd, base.length)
      .toString

  // --- shared rendering ------------------------------------------------------

  def doc(e: JEither[String, org.eclipse.lsp4j.MarkupContent]): String = {
    if e == null then ""
    else if e.isLeft then " " + e.getLeft
    else " " + e.getRight.getValue
  }.trim

  def sortLines(stableOrder: Boolean, string: String): String =
    val stripped = string.linesIterator.toList.filter(_.nonEmpty)
    if stableOrder then stripped.mkString("\n")
    else stripped.sorted.mkString("\n")

  protected def trimTrailingSpace(string: String): String =
    string.linesIterator
      .map(_.replaceFirst("\\s++$", ""))
      .mkString("\n")

  // --- completion item data (mtags CompletionItemData vendored in the PC jar)

  private val gson = new Gson()

  /** The SemanticDB symbol the PC attached to a completion item, used to
    * resolve the item exactly like dotty's `resolvedCompletions`.
    */
  def dataSymbol(item: CompletionItem): Option[String] =
    item.getData match
      case null => None
      case data: CompletionItemData => Option(data.symbol)
      case json: JsonElement =>
        try Option(gson.fromJson(json, classOf[CompletionItemData])).map(_.symbol)
        catch case _: Exception => None
      case _ => None

/** Ported `BaseCompletionSuite`: `check` renders resolved completions one per
  * line (label + detail), `checkEdit` applies the selected item's text edit.
  */
abstract class CorpusCompletionHarness extends CorpusSuiteBase:

  protected def getItems(
      original: String,
      filename: String = "A.scala",
      workspaceMethods: Seq[CorpusPc.WorkspaceMethod] = Nil,
      mockToplevels: Map[String, Vector[String]] = Map.empty
  ): Seq[CompletionItem] =
    val (code, offset) = params(original)
    val uri = CorpusPc.openBuffer(code, filename, workspaceMethods, mockToplevels)
    try
      val (line, col) = offsetToLsp(code, offset)
      val result = CorpusPc.facade.completion(uri, line, col)
      result.getItems.asScala.toSeq
        .map { item =>
          dataSymbol(item) match
            case Some(symbol) =>
              CorpusPc.facade.completionItemResolve(CorpusPc.targetId, item, symbol)
            case None => item
        }
        .sortBy(item => Option(item.getSortText).getOrElse(item.getLabel))
    finally CorpusPc.closeBuffer(uri)

  /** Dotty `TestCompletions.getFullyQualifiedLabel`. */
  def fullyQualifiedLabel(item: CompletionItem): String =
    if item.getInsertText == null then item.getLabel
    else
      val idx = item.getInsertText.indexOf(item.getLabel)
      if idx < 0 then item.getLabel
      else item.getInsertText.substring(0, idx) + item.getLabel

  def check(
      name: String,
      original: String,
      expected: String,
      includeCommitCharacter: Boolean = false,
      stableOrder: Boolean = true,
      topLines: Option[Int] = None,
      filterText: String = "",
      includeDetail: Boolean = true,
      filename: String = "A.scala",
      filter: String => Boolean = _ => true,
      enablePackageWrap: Boolean = true,
      workspaceMethods: Seq[CorpusPc.WorkspaceMethod] = Nil,
      mockToplevels: Map[String, Vector[String]] = Map.empty
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val withPkg =
        if original.contains("package") || !enablePackageWrap then original
        else s"package test\n$original"
      val baseItems = getItems(withPkg, filename, workspaceMethods, mockToplevels)
      val items = topLines match
        case Some(top) => baseItems.take(top)
        case None => baseItems
      val filteredItems = items.filter(item => filter(item.getLabel))
      val nonEmptyExpected =
        filteredItems.isEmpty && expected.linesIterator.exists(_.trim.nonEmpty)
      val result = if nonEmptyExpected then items else filteredItems

      val out = new StringBuilder()
      result.foreach { item =>
        val label = fullyQualifiedLabel(item)
        val commitCharacter =
          if includeCommitCharacter then
            Option(item.getCommitCharacters)
              .getOrElse(Collections.emptyList())
              .asScala
              .mkString(" (commit: '", " ", "')")
          else ""
        out
          .append(label)
          .append {
            val detailIsDefined = Option(item.getDetail).isDefined
            if includeDetail && detailIsDefined
              && !item.getLabel.contains(item.getDetail)
            then item.getDetail
            else ""
          }
          .append(commitCharacter)
          .append(if nonEmptyExpected then " [FILTERED OUT]" else "")
          .append("\n")
      }

      val expectedResult = sortLines(stableOrder, expected)
      val actualResult = sortLines(stableOrder, trimTrailingSpace(out.toString))
      assertCorpusNoDiff(actualResult, expectedResult)

      if filterText.nonEmpty then
        filteredItems.foreach { item =>
          assertEquals(
            Option(item.getFilterText).getOrElse(""),
            filterText,
            s"Invalid filter text for item:\n$item"
          )
        }
    }

  def checkEdit(
      name: String,
      original: String,
      expected: String,
      filterText: String = "",
      assertSingleItem: Boolean = true,
      filter: String => Boolean = _ => true,
      command: Option[String] = None,
      itemIndex: Int = 0,
      filename: String = "A.scala",
      workspaceMethods: Seq[CorpusPc.WorkspaceMethod] = Nil,
      mockToplevels: Map[String, Vector[String]] = Map.empty
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val items = getItems(original, filename, workspaceMethods, mockToplevels)
        .filter(item => filter(item.getLabel))
      assert(items.nonEmpty, "Obtained empty completions, can't check for edits.")
      if assertSingleItem && items.length != 1 then
        fail(
          s"expected single completion item, obtained ${items.length} items.\n${items.map(_.getLabel + "\n")}"
        )
      if items.size <= itemIndex then fail(s"Not enough completion items: $items")
      val item = items(itemIndex)
      val (code, _) = params(original)
      val obtained = applyEdits(code, item)
      assertCorpusNoDiff(obtained, expected)
      if filterText.nonEmpty then
        assertEquals(Option(item.getFilterText).getOrElse(""), filterText, "Invalid filter text")
      assertEquals(
        Option(item.getCommand).fold("")(_.getCommand),
        command.getOrElse(""),
        "Invalid command"
      )
    }

/** Ported `BaseHoverSuite` + `TestHovers`: terse expected strings expand
  * through the vendored `HoverMarkup`; `<<...>>` markers in the original
  * assert the hover's reported range.
  */
abstract class CorpusHoverHarness extends CorpusSuiteBase:

  extension (string: String)
    def hover: String =
      string.trim.linesIterator.toList match
        case List(symbolSignature) =>
          HoverMarkup("", Some(symbolSignature), "")
        case List(expressionType, symbolSignature) =>
          HoverMarkup(expressionType, Some(symbolSignature), "", forceExpressionType = true)
        case _ => string

  def renderAsString(hover: Option[Hover]): String =
    hover match
      case Some(value) =>
        val contents = value.getContents
        if contents.isRight then contents.getRight.getValue
        else
          contents.getLeft.asScala
            .map(e => if e.isLeft then e.getLeft else e.getRight.getValue)
            .mkString("\n")
      case None => ""

  def check(
      name: String,
      original: String,
      expected: String
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val filename = "Hover.scala"
      val codeOriginal = original.replace("<<", "").replace(">>", "")
      val (code, so, eo) = hoverParams(codeOriginal)
      if so != eo then
        fail("range hovers (%<% ... %>%) are not supported by the PcFacade point-hover surface")
      val uri = CorpusPc.openBuffer(code, filename)
      try
        val (line, col) = offsetToLsp(code, so)
        val hover = CorpusPc.facade.hover(uri, line, col)
        val obtained = renderAsString(hover)
        assertCorpusNoDiff(obtained, expected)

        for
          h <- hover
          range <- Option(h.getRange)
        do
          val base = codeOriginal.removePos
          val withRange = replaceInRange(base, range)
          assertCorpusNoDiff(withRange, original.removePos)
      finally CorpusPc.facade.didClose(uri)
    }

/** Ported `BaseSignatureHelpSuite`: renders every signature label and a caret
  * line under the active parameter of the active signature.
  */
abstract class CorpusSignatureHelpHarness extends CorpusSuiteBase:

  def check(
      name: String,
      original: String,
      expected: String,
      stableOrder: Boolean = true
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val (code, offset) = params(original)
      val uri = CorpusPc.openBuffer(code, "A.scala")
      try
        val (line, col) = offsetToLsp(code, offset)
        val result = CorpusPc.facade.signatureHelp(uri, line, col)
        val out = new StringBuilder()
        // default SignatureHelp is the PC's crash sentinel
        assert(result != new SignatureHelp())
        if result != null then
          result.getSignatures.asScala.zipWithIndex.foreach { case (signature, i) =>
            out.append(signature.getLabel).append("\n")
            if result.getActiveSignature == i && result.getActiveParameter != null
              && result.getActiveParameter >= 0
              && result.getActiveParameter < signature.getParameters.size()
            then
              val param = signature.getParameters.get(result.getActiveParameter)
              val label = param.getLabel.getLeft
              val sameLabelsBeforeActive = signature.getParameters.asScala
                .take(result.getActiveParameter + 1)
                .count(_.getLabel.getLeft == label) - 1
              def seekColumn(atIndex: Int, labels: Int): Int =
                val ch = signature.getLabel.indexOf(label, atIndex)
                if labels == 0 then ch
                else seekColumn(ch + 1, labels - 1)
              val column = seekColumn(0, sameLabelsBeforeActive)
              if column < 0 then
                fail(s"""invalid parameter label
                        |  param.label    : ${param.getLabel}
                        |  signature.label: ${signature.getLabel}
                        |""".stripMargin)
              out
                .append(" " * column)
                .append("^" * param.getLabel.getLeft.length)
                .append("\n")
          }
        val obtainedSorted = sortLines(stableOrder, out.toString)
        val expectedSorted = sortLines(stableOrder, expected)
        assertCorpusNoDiff(obtainedSorted, expectedSorted)
      finally CorpusPc.facade.didClose(uri)
    }

/** Ported `BaseInlayHintsSuite` + `TestInlayHints`: hints for the whole
  * buffer render as inline `/*...*/` decorations at their positions; each
  * label part's data — a SemanticDB symbol for global symbols or a `(line:
  * char)` definition position for local ones, carried in the hint's opaque
  * `data` JSON — renders as `<<...>>` after the label, exactly like dotty's
  * `TestInlayHints.decorationString`.
  */
abstract class CorpusInlayHintsHarness extends CorpusSuiteBase:

  /** The dotty base suite enables EVERY hint category (`inferredTypes`,
    * `typeParameters`, `implicitParameters`, `hintsXRayMode`,
    * `byNameParameters`, `implicitConversions`, `namedParameters`); the two
    * per-case booleans add their bits on top. The matching facade bitset.
    */
  private def flags(hintsInPatternMatch: Boolean, closingLabels: Boolean): Int =
    var bits = PcInlayHintFlags.InferredTypes | PcInlayHintFlags.TypeParameters |
      PcInlayHintFlags.ImplicitParameters | PcInlayHintFlags.ByNameParameters |
      PcInlayHintFlags.ImplicitConversions | PcInlayHintFlags.NamedParameters |
      PcInlayHintFlags.HintsXRayMode
    if hintsInPatternMatch then bits |= PcInlayHintFlags.HintsInPatternMatch
    if closingLabels then bits |= PcInlayHintFlags.ClosingLabels
    bits

  /** Dotty `TestInlayHints.readData`: an empty entry renders nothing, a
    * symbol renders `<<symbol>>`, a local definition position renders
    * `<<(line:char)>>`.
    */
  private def readData(data: Either[String, Position]): List[String] =
    data match
      case Left("") => Nil
      case Left(symbol) => List("<<", symbol, ">>")
      case Right(pos) => List("<<", s"(${pos.getLine}:${pos.getCharacter})", ">>")

  /** Dotty `TestInlayHints.decorationString` over the facade carrier: the
    * label-part texts zipped with the per-part data decoded from the hint's
    * opaque JSON bytes (the exact gson JSON the island carried verbatim).
    */
  private def decoration(hint: PcInlayHint): String =
    val labels = hint.labelParts.map(_.text).toList
    val data = hint.data match
      case Some(bytes) =>
        val json = JsonParser.parseString(
          new String(bytes.toArray, java.nio.charset.StandardCharsets.UTF_8)
        )
        InlayHints.fromData(json)._2
      case None => Nil
    val out = new StringBuilder("/*")
    labels.zip(data).foreach { (label, d) =>
      out.append(label)
      readData(d).foreach(out.append)
    }
    out.append("*/").toString

  def check(
      name: String,
      base: String,
      expected: String,
      hintsInPatternMatch: Boolean = false,
      closingLabels: Boolean = false
  )(implicit loc: munit.Location): Unit =
    test(name) {
      def pkgWrap(text: String) =
        if text.contains("package") then text else s"package test\n$text"
      val withPkg = pkgWrap(base)
      val uri = CorpusPc.openBuffer(withPkg, "InlayHints.scala")
      try
        val (endLine, endChar) = offsetToLsp(withPkg, withPkg.length)
        val range = new Range(new Position(0, 0), new Position(endLine, endChar))
        val hints =
          CorpusPc.facade.inlayHints(uri, range, flags(hintsInPatternMatch, closingLabels))
        val edits = hints.toList.map { hint =>
          new TextEdit(new Range(hint.position, hint.position), decoration(hint))
        }
        assertCorpusNoDiff(applyEdits(withPkg, edits), pkgWrap(expected))
      finally CorpusPc.closeBuffer(uri)
    }

/** Ported `BaseSelectionRangeSuite`: the cursor's innermost-first chain of
  * enclosing selection ranges renders each expected step with
  * `>>region>>...<<region<<` markers. Like the dotty base (mimicking how VS
  * Code walks the parents client-side), only as many chain steps as the
  * expectation lists are compared — the chain may extend wider.
  */
abstract class CorpusSelectionRangeHarness extends CorpusSuiteBase:

  def check(
      name: String,
      original: String,
      expectedRanges: List[String]
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val (code, offset) = params(original)
      val uri = CorpusPc.openBuffer(code, "SelectionRange.scala")
      try
        val (line, col) = offsetToLsp(code, offset)
        val chains = CorpusPc.facade.selectionRanges(uri, Vector(new Position(line, col)))
        val chain = chains.headOption.getOrElse(Vector.empty)
        assert(chain.nonEmpty, "no selection chain at the cursor")
        expectedRanges.zipWithIndex.foreach { (expected, i) =>
          assert(
            i < chain.size,
            s"selection chain ended after ${chain.size} ranges; expected at least ${expectedRanges.size}"
          )
          val obtained = replaceInRange(code, chain(i), ">>region>>", "<<region<<")
          assertCorpusNoDiff(obtained, expected)
        }
      finally CorpusPc.closeBuffer(uri)
    }

/** Ported `BaseSemanticTokensSuite` + `TestSemanticTokens.pcSemanticString`:
  * the expected string carries `<<name>>/*type,modifiers*/` markers over the
  * symbol tokens the COMPILER contributes (keyword/literal tokens are added
  * outside the compiler and are not asserted, exactly like the dotty base
  * suite). `check` strips the markers, runs the facade's whole-buffer
  * `semanticTokens` (offset nodes), and re-renders the markers with the same
  * node-selection walk the dotty test util applies — including the
  * `SemanticTokens.getTypePriority` tie-break for same-start candidates.
  */
abstract class CorpusSemanticTokensHarness extends CorpusSuiteBase:

  /** Dotty `TestSemanticTokens.decorationString`: the type name (when
    * classified) followed by every set modifier bit's name, comma-joined.
    */
  private def decorationString(typeInd: Int, modInd: Int): String =
    val buffer = ListBuffer.empty[String]
    if typeInd != -1 then buffer += SemanticTokens.TokenTypes(typeInd)
    val wkList = modInd.toBinaryString.toCharArray().toList.reverse
    for i <- 0 to wkList.size - 1 do
      if wkList(i).toString == "1" then buffer += SemanticTokens.TokenModifiers(i)
    buffer.toList.mkString(",")

  /** Dotty `TestSemanticTokens.pcSemanticString` over the facade's
    * [[PcSemanticNode]] carrier: walk the (provider-ordered) nodes, keep the
    * identifier-shaped, non-overlapped ones, pick the highest-priority node
    * among same-start candidates, and wrap each pick in its markers.
    */
  private def pcSemanticString(fileContent: String, nodes: List[PcSemanticNode]): String =
    val wkStr = new StringBuilder

    def isIdentifier(start: Int, end: Int) =
      fileContent.slice(start, end).matches("^[\\d\\w`+-_!@]+$")

    def iter(nodes: List[PcSemanticNode], curr: Int): Int =
      nodes match
        case head :: rest
            if (curr <= head.start && head.start != head.end) && isIdentifier(head.start, head.end) =>
          val isValid = rest
            .takeWhile(node =>
              node.end < head.end || (node.end == head.end && node.start > head.start)
            )
            .isEmpty
          if isValid then
            val candidates = head :: rest.takeWhile(nxt =>
              nxt.start == head.start && isIdentifier(nxt.start, nxt.end)
            )
            val node = candidates.maxBy(n => SemanticTokens.getTypePriority(n.tokenType))
            wkStr ++= fileContent.slice(curr, node.start)
            wkStr ++= "<<"
            wkStr ++= fileContent.slice(node.start, node.end)
            wkStr ++= ">>/*"
            wkStr ++= decorationString(node.tokenType, node.tokenModifier)
            wkStr ++= "*/"
            iter(rest, node.end)
          else iter(rest, curr)
        case _ :: rest => iter(rest, curr)
        case Nil => curr
    val curr = iter(nodes, 0)
    wkStr ++= fileContent.slice(curr, fileContent.size)
    wkStr.mkString

  def check(name: String, expected: String)(implicit loc: munit.Location): Unit =
    test(name) {
      val base = expected
        .replaceAll(raw"/\*[\w,]+\*/", "")
        .replaceAll(raw"\<\<|\>\>", "")
      val uri = CorpusPc.openBuffer(base, "Tokens.scala")
      try
        val nodes = CorpusPc.facade.semanticTokens(uri)
        val obtained = pcSemanticString(base, nodes.toList)
        assertCorpusNoDiff(obtained, expected)
      finally CorpusPc.closeBuffer(uri)
    }

/** Ported `BaseDiagnosticsSuite`: `check` pushes the buffer through the
  * facade's `diagnostics` path (the PC `didChange` with diagnostics on — the
  * island's `pc_diagnostics` op) and compares the `(startOffset, endOffset,
  * message, severity)` tuples exactly; `additionalChecks` receives the raw
  * lsp4j diagnostics for data assertions (the attached quick-fix actions).
  */
abstract class CorpusDiagnosticsHarness extends CorpusSuiteBase:

  case class TestDiagnostic(
      startIndex: Int,
      endIndex: Int,
      msg: String,
      severity: DiagnosticSeverity
  )

  /** Open the buffer under this target id — the plain corpus target by
    * default; the explain suite overrides with the `-explain` target.
    */
  def diagnosticsTargetId: String = CorpusPc.targetId

  private def diagnosticMessageAsString(d: Diagnostic): String =
    val msg = d.getMessage()
    if msg == null then ""
    else if msg.isLeft then msg.getLeft
    else msg.getRight.getValue

  private def offsetOf(text: String, position: Position): Int =
    Utf16Text.offsetAt(text, position.getLine, position.getCharacter)

  def check(
      name: String,
      text: String,
      expected: List[TestDiagnostic],
      additionalChecks: List[Diagnostic] => Unit = _ => ()
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val uri = CorpusPc.openBufferFor(diagnosticsTargetId, text, "Diagnostic.scala")
      try
        val diagnostics = CorpusPc.facade.diagnostics(uri).toList
        val actual = diagnostics.map(d =>
          TestDiagnostic(
            offsetOf(text, d.getRange().getStart()),
            offsetOf(text, d.getRange().getEnd()),
            diagnosticMessageAsString(d),
            d.getSeverity()
          )
        )
        assertEquals(
          actual,
          expected,
          s"Expected [${expected.mkString(", ")}] but got [${actual.mkString(", ")}]"
        )
        additionalChecks(diagnostics)
      finally CorpusPc.closeBuffer(uri)
    }

/** Ported `BaseCodeActionSuite`: the single `<<target>>` marker parse — the
  * cursor sits at the END of the target, the dotty edit-suite convention —
  * shared by every code-action-family corpus harness below. Drives the facade
  * `codeAction`/`autoImports` carriers (LSP positions instead of offsets) and
  * applies the returned edits with the shared [[CorpusSuiteBase.applyEdits]]
  * renderer.
  */
abstract class CorpusCodeActionBase extends CorpusSuiteBase:

  /** Dotty `BaseCodeActionSuite.params`: strip the one `<<target>>` and
    * return `(code, target, offset)` with the offset at the target's end.
    */
  def codeActionParams(code: String): (String, String, Int) =
    val targetRegex = "<<(.+)>>".r
    val target = targetRegex.findAllMatchIn(code).toList match
      case Nil => fail("Missing <<target>>")
      case t :: Nil => t.group(1)
      case _ => fail("Multiple <<targets>> found")
    val code2 = code.replace("<<", "").replace(">>", "")
    val offset = code.indexOf("<<") + target.length
    (code2, target, offset)

  /** Run `actionId` at the `<<target>>` cursor; a dotty `DisplayableException`
    * comes back as the result's refusal (data, not an error).
    */
  def runCodeAction(
      actionId: Int,
      original: String,
      argIndices: Option[Vector[Int]] = None
  ): (String, PcCodeActionResult) =
    val (code, _, offset) = codeActionParams(original)
    val uri = CorpusPc.openBuffer(code)
    try
      val (line, col) = offsetToLsp(code, offset)
      val result =
        CorpusPc.facade.codeAction(uri, actionId, new Position(line, col), None, argIndices)
      (code, result)
    finally CorpusPc.closeBuffer(uri)

/** The ported edit-suite check DSL for the single-position code actions
  * (InsertInferredType / AutoImplementAbstractMembers / InlineValue /
  * InsertInferredMethod / ConvertToNamedLambdaParameters): `checkEdit` runs
  * the op at the `<<target>>` cursor, applies the returned edits and asserts
  * the edited text; `checkRefusal` ports the dotty `checkError` shape onto
  * the facade's refusal-as-data carrier.
  */
abstract class CorpusCodeActionEditHarness(actionId: Int) extends CorpusCodeActionBase:

  def checkEdit(
      name: String,
      original: String,
      expected: String
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val (code, result) = runCodeAction(actionId, original)
      assertEquals(result.refusal, None)
      assert(result.edits.nonEmpty, "expected code-action edits")
      assertCorpusNoDiff(applyEdits(code, result.edits.toList), expected)
    }

  def checkRefusal(
      name: String,
      original: String,
      expectedRefusal: String
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val (_, result) = runCodeAction(actionId, original)
      assertEquals(result.edits, Vector.empty[TextEdit])
      assertEquals(result.refusal, Some(expectedRefusal))
    }

/** Ported `ConvertToNamedArgumentsSuite` check DSL: the op additionally
  * carries the explicit argument-index list (the dotty tests name them per
  * case; the LSP assembly layer instead passes every index — that policy is
  * pinned Rust-side, not here).
  */
abstract class CorpusConvertToNamedArgumentsHarness extends CorpusCodeActionBase:

  def checkEdit(
      name: String,
      original: String,
      argIndices: List[Int],
      expected: String
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val (code, result) = runCodeAction(
        PcCodeActionId.ConvertToNamedArguments,
        original,
        Some(argIndices.toVector)
      )
      assertEquals(result.refusal, None)
      assert(result.edits.nonEmpty, "expected convert-to-named-arguments edits")
      assertCorpusNoDiff(applyEdits(code, result.edits.toList), expected)
    }

  def checkRefusal(
      name: String,
      original: String,
      argIndices: List[Int],
      expectedRefusal: String
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val (_, result) = runCodeAction(
        PcCodeActionId.ConvertToNamedArguments,
        original,
        Some(argIndices.toVector)
      )
      assertEquals(result.edits, Vector.empty[TextEdit])
      assertEquals(result.refusal, Some(expectedRefusal))
    }

/** Ported `BaseExtractMethodSuite`: `<<...>>` marks the extraction selection.
  * The dotty harness carries a separate `@@` extraction-anchor position; the
  * facade carrier anchors the extraction at the SELECTION START (`position` =
  * selection start, `extractionEnd` = selection end), so the `@@` marker is
  * stripped and ignored — the ported cases all anchor within the selection's
  * own enclosing statement, where the two conventions agree.
  */
abstract class CorpusExtractMethodHarness extends CorpusSuiteBase:

  def checkEdit(
      name: String,
      original: String,
      expected: String
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val noAnchor = original.replace("@@", "")
      val onlyClose = noAnchor.replace("<<", "")
      val code = onlyClose.replace(">>", "")
      val start = noAnchor.indexOf("<<")
      val end = onlyClose.indexOf(">>")
      assert(start >= 0 && end >= 0, "missing <<selection>>")
      val uri = CorpusPc.openBuffer(code)
      try
        val (sl, sc) = offsetToLsp(code, start)
        val (el, ec) = offsetToLsp(code, end)
        val result = CorpusPc.facade.codeAction(
          uri,
          PcCodeActionId.ExtractMethod,
          new Position(sl, sc),
          Some(new Position(el, ec)),
          None
        )
        assertEquals(result.refusal, None)
        assert(result.edits.nonEmpty, "expected extract-method edits")
        assertCorpusNoDiff(applyEdits(code, result.edits.toList), expected)
      finally CorpusPc.closeBuffer(uri)
    }

/** Ported `BaseAutoImportsSuite`: `check` renders the candidate package list
  * one per line; `checkEdit` applies the selected candidate's edits. The
  * candidates come from the island's classpath search over the corpus
  * target's classpath (scala-library + scala3-library) plus the JDK index —
  * the same `ClasspathSearch.fromClasspath` seam the dotty base suite mocks.
  */
abstract class CorpusAutoImportsHarness extends CorpusCodeActionBase:

  def getAutoImports(original: String): (String, Vector[PcAutoImport]) =
    val (code, symbol, offset) = codeActionParams(original)
    val uri = CorpusPc.openBuffer(code)
    try
      val (line, col) = offsetToLsp(code, offset)
      (
        code,
        CorpusPc.facade.autoImports(uri, new Position(line, col), symbol, isExtension = false)
      )
    finally CorpusPc.closeBuffer(uri)

  def check(
      name: String,
      original: String,
      expected: String
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val (_, imports) = getAutoImports(original)
      assertCorpusNoDiff(imports.map(_.packageName).mkString("\n"), expected)
    }

  def checkEdit(
      name: String,
      original: String,
      expected: String,
      selection: Int = 0
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val (code, imports) = getAutoImports(original)
      assert(imports.size > selection, "Obtained no expected imports")
      assertCorpusNoDiff(applyEdits(code, imports(selection).edits.toList), expected)
    }

/** Ported `BasePcDefinitionSuite`: same-file definition ranges render as
  * `<<...>>`, cross-file (mock-resolved) locations render as an inline
  * `/*uri*/` comment at the cursor.
  */
abstract class CorpusDefinitionHarness extends CorpusSuiteBase:

  def definitions(uri: String, line: Int, character: Int): List[Location]

  def check(name: String, original: String)(implicit loc: munit.Location): Unit =
    test(name) {
      val (cleanedCode, offset) = params(original.removeRanges)
      val uri = CorpusPc.openBuffer(cleanedCode, "A.scala")
      try
        val (line, col) = offsetToLsp(cleanedCode, offset)
        val cursor = new Position(line, col)
        val offsetRange = new Range(cursor, cursor)
        val locs = definitions(uri, line, col)
        val edits = locs.flatMap { location =>
          if location.getUri == uri then
            List(
              new TextEdit(
                new Range(location.getRange.getStart, location.getRange.getStart),
                "<<"
              ),
              new TextEdit(
                new Range(location.getRange.getEnd, location.getRange.getEnd),
                ">>"
              )
            )
          else List(new TextEdit(offsetRange, s"/*${location.getUri}*/"))
        }
        val obtained = applyEdits(cleanedCode, edits)
        assertCorpusNoDiff(obtained, original.removePos)
      finally CorpusPc.facade.didClose(uri)
    }
