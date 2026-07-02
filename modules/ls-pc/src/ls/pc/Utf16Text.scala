package ls.pc

/** LSP position <-> string offset conversion.
  *
  * LSP positions count lines (separated by `\n`, `\r\n` or lone `\r`) and
  * characters in UTF-16 code units. Java strings are indexed in UTF-16 code
  * units already, so the character column maps directly onto a string index
  * within the line; no code-point arithmetic is needed (a surrogate pair such
  * as an emoji occupies two columns, exactly like two `Char`s).
  */
object Utf16Text:

  /** Offset (UTF-16 code units) of LSP `(line, character)` in `text`.
    *
    * Out-of-range positions clamp, per LSP: a line past the end returns
    * `text.length`; a character past the line end returns the line-end offset.
    */
  def offsetAt(text: String, line: Int, character: Int): Int =
    if line < 0 then return 0
    val n = text.length
    var i = 0
    var curLine = 0
    while curLine < line && i < n do
      val c = text.charAt(i)
      if c == '\n' then curLine += 1
      else if c == '\r' then
        curLine += 1
        if i + 1 < n && text.charAt(i + 1) == '\n' then i += 1
      i += 1
    if curLine < line then n
    else
      var j = i
      var remaining = math.max(0, character)
      while remaining > 0 && j < n && text.charAt(j) != '\n' && text.charAt(j) != '\r' do
        j += 1
        remaining -= 1
      j

  /** LSP `(line, character)` of `offset` in `text` (inverse of [[offsetAt]]).
    * Offsets clamp to `[0, text.length]`.
    */
  def positionAt(text: String, offset: Int): (Int, Int) =
    val bounded = math.max(0, math.min(offset, text.length))
    var i = 0
    var line = 0
    var lineStart = 0
    while i < bounded do
      val c = text.charAt(i)
      if c == '\n' then
        line += 1
        lineStart = i + 1
      else if c == '\r' then
        if i + 1 < text.length && text.charAt(i + 1) == '\n' then
          if i + 1 < bounded then
            line += 1
            i += 1
            lineStart = i + 1
        else
          line += 1
          lineStart = i + 1
      i += 1
    (line, bounded - lineStart)
