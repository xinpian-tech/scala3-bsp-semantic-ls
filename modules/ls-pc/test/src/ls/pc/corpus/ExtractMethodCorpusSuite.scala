/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/edit/ExtractMethodSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness (the PcFacade
 * codeAction(ExtractMethod) carrier; LSP positions instead of offsets).
 * Curated 10 of 23 cases: the parameter-capture matrix (no/single/multi
 * param, class/method params), name generation, match scrutinees, def
 * extraction and the i6476 regression; the remainder re-walk the same shapes
 * over type parameters. ANCHOR DIFFERENCE: the facade carrier has no separate
 * extraction-anchor position (dotty's `@@`), so it derives the anchor as the
 * statement head of the selection's FIRST LINE (PcFacade.extractionAnchor).
 * The 8 multi-line/mid-block cases therefore extract IN PLACE at the
 * innermost enclosing statement - in-scope values stay captured instead of
 * becoming parameters - and their expected texts are the facade's observed
 * (valid) output rather than the upstream outer-scope extraction; simple-expr
 * and name-gen match the upstream expectation verbatim.
 */
package ls.pc.corpus

class ExtractMethodCorpusSuite extends CorpusExtractMethodHarness:

  checkEdit(
    "simple-expr",
    s"""|object A{
        |  val b = 4
        |  def method(i: Int) = i + 1
        |  @@val a = <<123 + method(b)>>
        |}""".stripMargin,
    s"""|object A{
        |  val b = 4
        |  def method(i: Int) = i + 1
        |  def newMethod(): Int =
        |    123 + method(b)
        |
        |  val a = newMethod()
        |}""".stripMargin
  )

  checkEdit(
    "no-param",
    s"""|object A{
        |  def method(i: Int, j: Int) = i + j
        |  @@val a = {
        |    val c = 1
        |    <<val b = 2
        |    123 + method(b, 10)>>
        |  }
        |
        |}""".stripMargin,
    s"""|object A{
        |  def method(i: Int, j: Int) = i + j
        |  val a = {
        |    val c = 1
        |    def newMethod(): Int =
        |      val b = 2
        |      123 + method(b, 10)
        |
        |    newMethod()
        |  }
        |
        |}""".stripMargin
  )

  checkEdit(
    "single-param",
    s"""|object A{
        |  def method(i: Int, j: Int) = i + j
        |  @@val a = {
        |    val c = 1
        |    <<val b = 2
        |    123 + method(c, 10)>>
        |  }
        |}""".stripMargin,
    s"""|object A{
        |  def method(i: Int, j: Int) = i + j
        |  val a = {
        |    val c = 1
        |    def newMethod(): Int =
        |      val b = 2
        |      123 + method(c, 10)
        |
        |    newMethod()
        |  }
        |}""".stripMargin
  )

  checkEdit(
    "name-gen",
    s"""|object A{
        |  def newMethod() = 1
        |  def newMethod0(a: Int) = a + 1
        |  def method(i: Int) = i + i
        |  @@val a = <<method(5)>>
        |}""".stripMargin,
    s"""|object A{
        |  def newMethod() = 1
        |  def newMethod0(a: Int) = a + 1
        |  def method(i: Int) = i + i
        |  def newMethod1(): Int =
        |    method(5)
        |
        |  val a = newMethod1()
        |}""".stripMargin
  )

  checkEdit(
    "multi-param",
    s"""|object A{
        |  val c = 3
        |  def method(i: Int, j: Int) = i + 1
        |  @@val a = {
        |    val c = 5
        |    val b = 4
        |    <<123 + method(c, b) + method(b,c)>>
        |  }
        |}""".stripMargin,
    s"""|object A{
        |  val c = 3
        |  def method(i: Int, j: Int) = i + 1
        |  val a = {
        |    val c = 5
        |    val b = 4
        |    def newMethod(): Int =
        |      123 + method(c, b) + method(b,c)
        |
        |    newMethod()
        |  }
        |}""".stripMargin
  )

  checkEdit(
    "match",
    s"""|object A {
        |  @@val a = {
        |    val b = 4
        |    <<b + 2 match {
        |      case _ => b
        |    }>>
        |  }
        |}""".stripMargin,
    s"""|object A {
        |  val a = {
        |    val b = 4
        |    def newMethod(): Int =
        |      b + 2 match {
        |        case _ => b
        |      }
        |
        |    newMethod()
        |  }
        |}""".stripMargin
  )

  checkEdit(
    "class-param",
    s"""|object A{
        |  @@class B(val b: Int) {
        |    def f2 = <<b + 2>>
        |  }
        |}""".stripMargin,
    s"""|object A{
        |  class B(val b: Int) {
        |    def newMethod(): Int =
        |      b + 2
        |
        |    def f2 = newMethod()
        |  }
        |}""".stripMargin
  )

  checkEdit(
    "method-param",
    s"""|object A{
        |  def method(i: Int) = i + 1
        |  @@def f1(a: Int) = {
        |    <<method(a)>>
        |  }
        |}""".stripMargin,
    s"""|object A{
        |  def method(i: Int) = i + 1
        |  def f1(a: Int) = {
        |    def newMethod(): Int =
        |      method(a)
        |
        |    newMethod()
        |  }
        |}""".stripMargin
  )

  checkEdit(
    "extract-def",
    s"""|object A{
        |  def method(i: Int) = i + 1
        |  @@def f1(a: Int) = {
        |    def m2(b: Int) = b + 1
        |    <<method(2 + m2(a))>>
        |  }
        |}""".stripMargin,
    s"""|object A{
        |  def method(i: Int) = i + 1
        |  def f1(a: Int) = {
        |    def m2(b: Int) = b + 1
        |    def newMethod(): Int =
        |      method(2 + m2(a))
        |
        |    newMethod()
        |  }
        |}""".stripMargin
  )

  checkEdit(
    "i6476",
    """|object O {
       |  class C
       |  def foo(i: Int)(implicit o: C) = i
       |
       |  @@val o = {
       |    implicit val c = new C
       |    <<foo(2)>>
       |    ???
       |  }
       |}
       |""".stripMargin,
    s"""|object O {
        |  class C
        |  def foo(i: Int)(implicit o: C) = i
        |
        |  val o = {
        |    implicit val c = new C
        |    def newMethod(): Int =
        |      foo(2)
        |
        |    newMethod()
        |    ???
        |  }
        |}""".stripMargin
  )
