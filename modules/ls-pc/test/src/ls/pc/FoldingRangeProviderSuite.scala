package ls.pc

/** Pins the custom parser-only folding provider: exact ranges + kinds over
  * multi-construct fixtures (indentation syntax, brace syntax, CRLF,
  * unterminated-code recovery, region nesting, comment runs, import runs).
  * Pure — no presentation-compiler instance is involved.
  */
class FoldingRangeProviderSuite extends munit.FunSuite:

  private def folds(text: String): Vector[(Int, Int, Int, Int, Int)] =
    FoldingRangeProvider.foldingRanges("file:///fold/Test.scala", text).map { f =>
      (
        f.range.getStart.getLine,
        f.range.getStart.getCharacter,
        f.range.getEnd.getLine,
        f.range.getEnd.getCharacter,
        f.kind
      )
    }

  private val None_ = 0
  private val Comment = 1
  private val Imports = 2
  private val Region = 3

  test("indentation syntax: object, def body, and block fold; single-line members do not"):
    val text =
      """object A:
        |  def f(x: Int): Int =
        |    val y = x + 1
        |    y
        |  val z = 1
        |""".stripMargin
    assertEquals(
      folds(text),
      Vector(
        (0, 0, 4, 11, None_), // object A body
        (1, 2, 3, 5, None_), // def f body
        (2, 4, 3, 5, None_) // the indented block
      )
    )

  test("brace syntax: class, def, match + case bodies, and multi-line argument list"):
    val text =
      """class B {
        |  def g(n: Int): Int = n match {
        |    case 0 =>
        |      0
        |    case _ =>
        |      n
        |  }
        |  def h: Int = g(
        |    42
        |  )
        |}
        |""".stripMargin
    assertEquals(
      folds(text),
      Vector(
        (0, 0, 10, 1, None_), // class B
        (1, 2, 6, 3, None_), // def g
        (1, 23, 6, 3, None_), // n match { ... }
        (2, 4, 3, 7, None_), // case 0 (whole case)
        (2, 11, 3, 7, None_), // case 0 rhs block
        (4, 4, 5, 7, None_), // case _ (whole case)
        (4, 11, 5, 7, None_), // case _ rhs block
        (7, 2, 9, 3, None_), // def h
        (7, 16, 9, 3, None_) // g( ... ) argument list
      )
    )

  test("CRLF buffers fold to the same line/character ranges as LF ones"):
    val lf =
      """object A:
        |  def f(x: Int): Int =
        |    val y = x + 1
        |    y
        |  val z = 1
        |""".stripMargin
    val crlf = lf.replace("\n", "\r\n")
    assertEquals(folds(crlf), folds(lf))

  test("unterminated code recovers: the parsed prefix still folds"):
    val text =
      """class E {
        |  def m: Int = {
        |    val a = 1
        |    a
        |""".stripMargin
    val got = folds(text)
    assert(got.nonEmpty, s"expected recovered folds, got: $got")
    // the class and the def fold to the recovered end of the buffer
    assert(got.exists { case (sl, _, el, _, k) => sl == 0 && el >= 3 && k == None_ }, got.toString)
    assert(got.exists { case (sl, sc, el, _, k) => sl == 1 && sc == 2 && el >= 3 && k == None_ }, got.toString)

  test("comment blocks and //-runs fold as comments; short runs do not"):
    val text =
      """/* block
        |   comment */
        |// one
        |// two
        |// three
        |val a = 1
        |// short
        |// run
        |val b = "// not a comment /* nor this */"
        |""".stripMargin
    assertEquals(
      folds(text),
      Vector(
        (0, 0, 1, 13, Comment), // the /* */ block
        (2, 0, 4, 8, Comment) // the three-line // run
      )
    )

  test("import runs of >= 2 fold as imports; a lone import does not"):
    val text =
      """package p
        |
        |import a.b
        |import c.d
        |import e.f
        |
        |object C:
        |  import x.y
        |  val v = 1
        |""".stripMargin
    val got = folds(text)
    assertEquals(got.filter(_._5 == Imports), Vector((2, 0, 4, 10, Imports)))

  // A blank line breaks an import run: each side must independently reach the
  // >= 2 threshold (found while wiring the LSP foldingRange probe — real
  // sources often separate import groups with blank lines, and a lone import
  // on either side must not fold).
  test("blank-line-separated import runs fold separately, each needing >= 2"):
    val text =
      """package p
        |
        |import a.b
        |
        |import c.d
        |import e.f
        |
        |val v = 1
        |""".stripMargin
    assertEquals(folds(text).filter(_._5 == Imports), Vector((4, 0, 5, 10, Imports)))

  // Scala 3 fewer-braces colon-lambda argument blocks (`.map: entry =>`) fold
  // like their brace equivalents (the real-project e2e buffer uses them).
  test("fewer-braces colon-lambda argument blocks fold"):
    val text =
      """object D:
        |  val xs = List(1, 2).map: entry =>
        |    val doubled = entry * 2
        |    doubled
        |""".stripMargin
    assertEquals(
      folds(text),
      Vector(
        (0, 0, 3, 11, None_), // object D body
        (1, 25, 3, 11, None_), // the colon-lambda argument block after `.map:`
        (2, 4, 3, 11, None_) // the lambda's indented body
      )
    )

  // Scala 3 declaration bodies: enum, trait, and `given ... with` bodies fold.
  test("scala 3 declaration bodies fold: enum, trait, and given-with"):
    val text =
      """object S:
        |  enum Color:
        |    case Red
        |    case Green
        |  trait Show[T]:
        |    def show(t: T): String
        |  given Show[Int] with
        |    def show(t: Int): String =
        |      t.toString
        |""".stripMargin
    assertEquals(
      folds(text),
      Vector(
        (0, 0, 8, 16, None_), // object S body
        (1, 2, 3, 14, None_), // enum Color body
        (4, 2, 5, 26, None_), // trait Show body
        (6, 2, 8, 16, None_), // given ... with body
        (7, 4, 8, 16, None_) // def show body
      )
    )

  test("region markers pair up and nest via a stack"):
    val text =
      """// region outer
        |val a = 1
        |// region inner
        |val b = 2
        |// endregion
        |val c = 3
        |// endregion
        |// endregion
        |""".stripMargin
    assertEquals(
      folds(text).filter(_._5 == Region),
      Vector(
        (0, 0, 6, 12, Region), // outer (the stray extra endregion is ignored)
        (2, 0, 4, 12, Region) // inner
      )
    )
