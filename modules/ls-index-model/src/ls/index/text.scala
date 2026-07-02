package ls.index

/** Zero-based line/character positions, matching both SemanticDB Range and
  * LSP Position semantics (end exclusive).
  */
final case class Pos(line: Int, character: Int):
  def <=(other: Pos): Boolean =
    line < other.line || (line == other.line && character <= other.character)

final case class Span(startLine: Int, startChar: Int, endLine: Int, endChar: Int):
  def contains(line: Int, character: Int): Boolean =
    val afterStart =
      line > startLine || (line == startLine && character >= startChar)
    val beforeEnd =
      line < endLine || (line == endLine && character <= endChar)
    afterStart && beforeEnd

object Span:
  /** Pack line:char into one int as (line << 12 | char), the columnar postings
    * encoding. Lines above 2^20-1 or chars above 2^12-1 saturate.
    */
  inline val CharBits = 12
  inline val CharMask = (1 << CharBits) - 1
  def pack(line: Int, character: Int): Int =
    val l = math.min(line, (1 << 20) - 1)
    val c = math.min(character, CharMask)
    (l << CharBits) | c
  def unpackLine(packed: Int): Int = packed >>> CharBits
  def unpackChar(packed: Int): Int = packed & CharMask

/** A resolved location in a workspace source file. */
final case class Loc(uri: String, span: Span)
