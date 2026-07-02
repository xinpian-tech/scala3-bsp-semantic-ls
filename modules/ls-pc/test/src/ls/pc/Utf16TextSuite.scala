package ls.pc

class Utf16TextSuite extends munit.FunSuite:

  test("offsetAt walks \\n separated lines"):
    val text = "ab\ncd\nef"
    assertEquals(Utf16Text.offsetAt(text, 0, 0), 0)
    assertEquals(Utf16Text.offsetAt(text, 0, 2), 2)
    assertEquals(Utf16Text.offsetAt(text, 1, 0), 3)
    assertEquals(Utf16Text.offsetAt(text, 1, 1), 4)
    assertEquals(Utf16Text.offsetAt(text, 2, 2), 8)

  test("offsetAt handles \\r\\n and lone \\r line endings"):
    val crlf = "ab\r\ncd"
    assertEquals(Utf16Text.offsetAt(crlf, 1, 0), 4)
    assertEquals(Utf16Text.offsetAt(crlf, 1, 1), 5)
    val cr = "ab\rcd"
    assertEquals(Utf16Text.offsetAt(cr, 1, 0), 3)
    assertEquals(Utf16Text.offsetAt(cr, 1, 2), 5)

  test("offsetAt clamps out-of-range positions"):
    val text = "ab\ncd"
    // character beyond the line end clamps to the line end, not into the next line
    assertEquals(Utf16Text.offsetAt(text, 0, 99), 2)
    // line beyond the last clamps to text end
    assertEquals(Utf16Text.offsetAt(text, 7, 0), 5)
    assertEquals(Utf16Text.offsetAt(text, -1, 5), 0)
    assertEquals(Utf16Text.offsetAt(text, 0, -3), 0)

  test("offsetAt counts UTF-16 code units: surrogate pairs take two columns"):
    val text = "a🚀b" // a🚀b
    assertEquals(Utf16Text.offsetAt(text, 0, 1), 1) // before the emoji
    assertEquals(Utf16Text.offsetAt(text, 0, 3), 3) // after the emoji (2 units)
    assertEquals(Utf16Text.offsetAt(text, 0, 4), 4) // after 'b'

  test("offsetAt counts CJK (BMP) characters as one unit"):
    val text = "名字x\nab"
    assertEquals(Utf16Text.offsetAt(text, 0, 2), 2)
    assertEquals(Utf16Text.offsetAt(text, 0, 3), 3)
    assertEquals(Utf16Text.offsetAt(text, 1, 1), 5)

  test("positionAt inverts offsetAt on multi-line mixed text"):
    val text = "val s = \"🚀🚀\"\nval t = s\r\nx"
    for offset <- 0 to text.length do
      val (line, ch) = Utf16Text.positionAt(text, offset)
      // \r\n interior offset maps back inside the separator; skip that one position
      val isInsideCrLf =
        offset > 0 && offset < text.length && text.charAt(offset - 1) == '\r' && text.charAt(offset) == '\n'
      if !isInsideCrLf then assertEquals(Utf16Text.offsetAt(text, line, ch), offset, s"offset $offset")

  test("positionAt clamps out-of-range offsets"):
    assertEquals(Utf16Text.positionAt("ab\ncd", -1), (0, 0))
    assertEquals(Utf16Text.positionAt("ab\ncd", 99), (1, 2))
