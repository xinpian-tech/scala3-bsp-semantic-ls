/*
 * Harness ported from the Scala 3 ("dotty") presentation-compiler test suite,
 * version 3.8.4 (presentation-compiler/test/dotty/tools/pc/base/BasePCSuite.scala,
 * BaseCompletionSuite.scala, BaseHoverSuite.scala, BaseSignatureHelpSuite.scala,
 * BasePcDefinitionSuite.scala and utils/{TestHovers,TestCompletions,TextEdits,
 * RangeReplace,TestExtensions}.scala):
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

import com.google.gson.{Gson, JsonElement}
import org.eclipse.lsp4j.{
  CompletionItem,
  Hover,
  Location,
  Position,
  Range,
  SignatureHelp,
  TextEdit
}
import org.eclipse.lsp4j.jsonrpc.messages.Either as JEither
import scala.meta.internal.pc.{CompletionItemData, HoverMarkup}

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
        .sortBy((_, range) => (range.getStart.getLine, range.getStart.getCharacter))
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

  protected def getItems(original: String, filename: String = "A.scala"): Seq[CompletionItem] =
    val (code, offset) = params(original)
    val uri = CorpusPc.openBuffer(code, filename)
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
    finally CorpusPc.facade.didClose(uri)

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
      enablePackageWrap: Boolean = true
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val withPkg =
        if original.contains("package") || !enablePackageWrap then original
        else s"package test\n$original"
      val baseItems = getItems(withPkg, filename)
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
      filename: String = "A.scala"
  )(implicit loc: munit.Location): Unit =
    test(name) {
      val items = getItems(original, filename).filter(item => filter(item.getLabel))
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
