package ls.pc

import scala.collection.mutable

import org.eclipse.lsp4j.{Position, Range}

import dotty.tools.dotc.ast.untpd
import dotty.tools.dotc.core.Contexts.{Context, ContextBase}
import dotty.tools.dotc.parsing.Parsers
import dotty.tools.dotc.reporting.StoreReporter
import dotty.tools.dotc.util.SourceFile

/** Folding ranges for the current buffer text — the one ABI v2 op with no
  * dotty presentation-compiler provider, so it is computed island-side by a
  * parser-only walk (plus a small lexical pass), never the typer.
  *
  * Parsing mirrors how the interactive driver parses a buffer: a virtual
  * [[SourceFile]] fed to the dotty [[Parsers.Parser]] under a store reporter,
  * so syntax errors are recovered (collected, never thrown) and a broken
  * buffer still folds whatever parsed.
  *
  * Folded constructs (kind `none` unless said otherwise), each dropped when it
  * does not span at least two lines:
  *   - template bodies (`class`/`object`/`trait`/`enum`, brace or indentation
  *     syntax), `def` bodies, `match` expressions plus their multi-line
  *     `case` bodies, block expressions, and multi-line argument lists (the
  *     region from the callee's end to the call's closing token);
  *   - comment blocks: a multi-line `/* ... */` and any run of >= 3
  *     consecutive full-line `//` comments (kind `comment`);
  *   - import runs: >= 2 consecutive `import` statements (kind `imports`);
  *   - `// region` / `// endregion` marker pairs, nested via a stack (kind
  *     `region`).
  *
  * Offsets are UTF-16 code units against the buffer string (the parser and
  * the lexical pass agree by construction); positions are derived from one
  * line index, so CRLF buffers fold identically to LF ones. Results are
  * deduplicated and sorted, so the answer is deterministic for a given text.
  */
object FoldingRangeProvider:

  /** Kind ordinals of [[PcFoldingRange.kind]] (the boundary `folding_kind`
    * contract: 0 none, 1 comment, 2 imports, 3 region).
    */
  private object Kind:
    val None = 0
    val Comment = 1
    val Imports = 2
    val Region = 3

  /** The dotty parser shares mutable name/context state; folding requests are
    * serialized here so a concurrent caller can never corrupt a parse.
    */
  private val lock = new Object

  def foldingRanges(uri: String, text: String): Vector[PcFoldingRange] = lock.synchronized {
    val lines = new LineIndex(text)
    val folds = mutable.ArrayBuffer.empty[(Int, Int, Int)] // (startOffset, endOffset, kind)
    folds ++= syntaxFolds(uri, text)
    folds ++= lexicalFolds(text, lines)
    folds.iterator
      .flatMap { case (start, end, kind) =>
        val (sl, sc) = lines.positionOf(start)
        val (el, ec) = lines.positionOf(end)
        // single-line ranges fold nothing: drop them
        if el > sl then Some(PcFoldingRange(new Range(new Position(sl, sc), new Position(el, ec)), kind))
        else None
      }
      .toVector
      .distinct
      .sortBy(f =>
        (
          f.range.getStart.getLine,
          f.range.getStart.getCharacter,
          f.range.getEnd.getLine,
          f.range.getEnd.getCharacter,
          f.kind
        )
      )
  }

  // --- the parser walk ------------------------------------------------------

  /** Parse `text` with error recovery and collect the foldable construct spans
    * plus import runs. Errors land in the store reporter and are discarded —
    * exactly the interactive-driver posture of serving from whatever parsed.
    */
  private def syntaxFolds(uri: String, text: String): Vector[(Int, Int, Int)] =
    val source = SourceFile.virtual(uri, text)
    given Context = ContextBase().initialCtx.fresh
      .setReporter(new StoreReporter(null, fromTyperState = false))
      .setSource(source)
    val tree = new Parsers.Parser(source).parse()
    val collector = new Collector
    collector.traverse(tree)
    collector.folds.toVector ++ importRuns(collector.imports.toVector)

  private final class Collector(using Context) extends untpd.UntypedTreeTraverser:
    val folds = mutable.ArrayBuffer.empty[(Int, Int, Int)]
    val imports = mutable.ArrayBuffer.empty[(Int, Int)]

    private def add(span: dotty.tools.dotc.util.Spans.Span): Unit =
      if span.exists && span.start < span.end then folds += ((span.start, span.end, Kind.None))

    def traverse(tree: untpd.Tree)(using Context): Unit =
      tree match
        case t: untpd.ModuleDef =>
          add(t.span) // object body (brace or indentation syntax)
        case t: untpd.TypeDef if t.rhs.isInstanceOf[untpd.Template] =>
          add(t.span) // class / trait / enum body
        case t: untpd.DefDef =>
          if !t.rhs.isEmpty then add(t.span) // def body
        case t: untpd.Match =>
          add(t.span) // whole match; multi-line case bodies fold below
          t.cases.foreach(c => add(c.span))
        case t: untpd.Block =>
          add(t.span)
        case t: untpd.Apply if t.args.nonEmpty && t.fun.span.exists && t.span.exists =>
          // multi-line argument list: the region from the callee's end to the
          // call's closing token
          if t.fun.span.end < t.span.end then folds += ((t.fun.span.end, t.span.end, Kind.None))
        case t: untpd.Import =>
          if t.span.exists then imports += ((t.span.start, t.span.end))
        case _ => ()
      traverseChildren(tree)

  /** Group consecutive `import` statements (adjacent or same-line starts) and
    * fold every run of >= 2, kind `imports`.
    */
  private def importRuns(imports: Vector[(Int, Int)]): Vector[(Int, Int, Int)] =
    if imports.isEmpty then return Vector.empty
    val sorted = imports.sortBy(_._1)
    val runs = mutable.ArrayBuffer.empty[(Int, Int, Int)]
    var runStart = sorted.head._1
    var runEnd = sorted.head._2
    var runCount = 1
    def flush(): Unit =
      if runCount >= 2 then runs += ((runStart, runEnd, Kind.Imports))
    for (start, end) <- sorted.tail do
      if start <= runEnd + 1 then
        runEnd = math.max(runEnd, end)
        runCount += 1
      else
        flush()
        runStart = start
        runEnd = end
        runCount = 1
    flush()
    runs.toVector

  // --- the lexical pass -----------------------------------------------------

  /** One full-line `//` comment: its token offsets, line, and marker class. */
  private final case class LineComment(start: Int, end: Int, line: Int, marker: Int)
  private val NoMarker = 0
  private val RegionStart = 1
  private val RegionEnd = 2

  /** Scan the raw text once (string/char-literal aware, nested block
    * comments) and fold multi-line block comments, `//` runs of >= 3 lines,
    * and `// region`/`// endregion` pairs (nested via a stack).
    */
  private def lexicalFolds(text: String, lines: LineIndex): Vector[(Int, Int, Int)] =
    val out = mutable.ArrayBuffer.empty[(Int, Int, Int)]
    val lineComments = mutable.ArrayBuffer.empty[LineComment]
    val n = text.length
    var i = 0
    while i < n do
      val c = text.charAt(i)
      if c == '/' && i + 1 < n && text.charAt(i + 1) == '/' then
        val start = i
        while i < n && text.charAt(i) != '\n' do i += 1
        val end = if i > start && text.charAt(i - 1) == '\r' then i - 1 else i
        val (line, _) = lines.positionOf(start)
        if onlyWhitespaceBefore(text, lines, start, line) then
          lineComments += LineComment(start, end, line, markerOf(text, start + 2, end))
      else if c == '/' && i + 1 < n && text.charAt(i + 1) == '*' then
        // block comment; Scala block comments nest
        val start = i
        var depth = 1
        i += 2
        while i < n && depth > 0 do
          if text.charAt(i) == '/' && i + 1 < n && text.charAt(i + 1) == '*' then
            depth += 1
            i += 2
          else if text.charAt(i) == '*' && i + 1 < n && text.charAt(i + 1) == '/' then
            depth -= 1
            i += 2
          else i += 1
        out += ((start, i, Kind.Comment)) // an unterminated comment folds to EOF
      else if c == '"' then i = skipString(text, i)
      else if c == '\'' && i + 2 < n
        && (text.charAt(i + 2) == '\'' || (text.charAt(i + 1) == '\\' && i + 3 < n && text.charAt(i + 3) == '\''))
      then
        // a char literal ('x' or '\x'); quote/symbol syntax falls through as code
        i += (if text.charAt(i + 1) == '\\' then 4 else 3)
      else i += 1
    out ++= commentRuns(lineComments.toVector)
    out ++= regionPairs(lineComments.toVector)
    out.toVector

  /** Skip a string literal starting at the opening quote `i`; handles triple
    * quotes (no escapes) and single-line strings (backslash escapes).
    */
  private def skipString(text: String, i: Int): Int =
    val n = text.length
    if i + 2 < n && text.charAt(i + 1) == '"' && text.charAt(i + 2) == '"' then
      var j = i + 3
      while j < n && !text.startsWith("\"\"\"", j) do j += 1
      if j < n then j + 3 else n
    else
      var j = i + 1
      while j < n && text.charAt(j) != '"' && text.charAt(j) != '\n' do
        if text.charAt(j) == '\\' && j + 1 < n then j += 2 else j += 1
      if j < n && text.charAt(j) == '"' then j + 1 else j

  private def onlyWhitespaceBefore(text: String, lines: LineIndex, offset: Int, line: Int): Boolean =
    val lineStart = lines.startOf(line)
    (lineStart until offset).forall(k => text.charAt(k).isWhitespace)

  /** Marker class of a `//` comment body `[from, until)`: `// region ...` /
    * `// endregion ...` (leading whitespace ignored) or a plain comment.
    */
  private def markerOf(text: String, from: Int, until: Int): Int =
    val body = text.substring(from, until).trim
    if body == "endregion" || body.startsWith("endregion ") then RegionEnd
    else if body == "region" || body.startsWith("region ") then RegionStart
    else NoMarker

  /** Runs of >= 3 consecutive full-line plain `//` comments, kind `comment`.
    * Region markers never join a run (they fold as regions instead).
    */
  private def commentRuns(comments: Vector[LineComment]): Vector[(Int, Int, Int)] =
    val runs = mutable.ArrayBuffer.empty[(Int, Int, Int)]
    val plain = comments.filter(_.marker == NoMarker)
    var idx = 0
    while idx < plain.length do
      var last = idx
      while last + 1 < plain.length && plain(last + 1).line == plain(last).line + 1 do last += 1
      if last - idx + 1 >= 3 then runs += ((plain(idx).start, plain(last).end, Kind.Comment))
      idx = last + 1
    runs.toVector

  /** `// region` / `// endregion` pairs matched with a stack (nesting), kind
    * `region`; unmatched markers are ignored.
    */
  private def regionPairs(comments: Vector[LineComment]): Vector[(Int, Int, Int)] =
    val out = mutable.ArrayBuffer.empty[(Int, Int, Int)]
    val stack = mutable.Stack.empty[LineComment]
    for c <- comments do
      c.marker match
        case RegionStart => stack.push(c)
        case RegionEnd => if stack.nonEmpty then
            val open = stack.pop()
            out += ((open.start, c.end, Kind.Region))
        case _ => ()
    out.toVector

  // --- line index -----------------------------------------------------------

  /** UTF-16 line-start table over `\n`-terminated lines (a `\r` before the
    * `\n` stays inside the line, so CRLF and LF buffers agree on positions).
    */
  private final class LineIndex(text: String):
    private val starts: Array[Int] =
      val buf = mutable.ArrayBuffer(0)
      var i = 0
      while i < text.length do
        if text.charAt(i) == '\n' then buf += i + 1
        i += 1
      buf.toArray

    def startOf(line: Int): Int = starts(line)

    /** `(line, character)` of a UTF-16 `offset` (clamped to the text). */
    def positionOf(offset: Int): (Int, Int) =
      val bounded = math.max(0, math.min(offset, text.length))
      var lo = 0
      var hi = starts.length - 1
      while lo < hi do
        val mid = (lo + hi + 1) >>> 1
        if starts(mid) <= bounded then lo = mid else hi = mid - 1
      (lo, bounded - starts(lo))
