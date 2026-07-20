/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/SelectionRangeSuite.scala
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/SelectionRangeCommentSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness (both dotty
 * suites in one file, the comment suite's case prefixed `comment-`); full
 * port, no compat maps.
 */
package ls.pc.corpus

class SelectionRangeCorpusSuite extends CorpusSelectionRangeHarness:

  check(
    "match",
    """|object Main extends App {
       |  Option("chris") match {
       |    case Some(n@@ame) => println("Hello! " + name)
       |    case None =>
       |  }
       |}""".stripMargin,
    List(
      """|object Main extends App {
         |  Option("chris") match {
         |    case Some(>>region>>name<<region<<) => println("Hello! " + name)
         |    case None =>
         |  }
         |}""".stripMargin,
      """|object Main extends App {
         |  Option("chris") match {
         |    case >>region>>Some(name)<<region<< => println("Hello! " + name)
         |    case None =>
         |  }
         |}""".stripMargin,
      """|object Main extends App {
         |  Option("chris") match {
         |    >>region>>case Some(name) => println("Hello! " + name)<<region<<
         |    case None =>
         |  }
         |}""".stripMargin,
      """|object Main extends App {
         |  >>region>>Option("chris") match {
         |    case Some(name) => println("Hello! " + name)
         |    case None =>
         |  }<<region<<
         |}""".stripMargin,
      """|object Main extends >>region>>App {
         |  Option("chris") match {
         |    case Some(name) => println("Hello! " + name)
         |    case None =>
         |  }<<region<<
         |}""".stripMargin,
      """|>>region>>object Main extends App {
         |  Option("chris") match {
         |    case Some(name) => println("Hello! " + name)
         |    case None =>
         |  }
         |}<<region<<""".stripMargin
    )
  )

  check(
    "for",
    """|object Main extends App {
       |  val total = for {
       |    a <- S@@ome(1)
       |    b <- Some(2)
       |  } yield a + b
       |}""".stripMargin,
    List(
      """|object Main extends App {
         |  val total = for {
         |    a <- >>region>>Some<<region<<(1)
         |    b <- Some(2)
         |  } yield a + b
         |}""".stripMargin,
      """|object Main extends App {
         |  val total = for {
         |    a <- >>region>>Some(1)<<region<<
         |    b <- Some(2)
         |  } yield a + b
         |}""".stripMargin,
      """|object Main extends App {
         |  val total = for {
         |    >>region>>a <- Some(1)<<region<<
         |    b <- Some(2)
         |  } yield a + b
         |}""".stripMargin,
      """|object Main extends App {
         |  val total = >>region>>for {
         |    a <- Some(1)
         |    b <- Some(2)
         |  } yield a + b<<region<<
         |}""".stripMargin,
      """|object Main extends App {
         |  >>region>>val total = for {
         |    a <- Some(1)
         |    b <- Some(2)
         |  } yield a + b<<region<<
         |}""".stripMargin,
      """|object Main extends >>region>>App {
         |  val total = for {
         |    a <- Some(1)
         |    b <- Some(2)
         |  } yield a + b<<region<<
         |}""".stripMargin,
      """|>>region>>object Main extends App {
         |  val total = for {
         |    a <- Some(1)
         |    b <- Some(2)
         |  } yield a + b
         |}<<region<<""".stripMargin
    )
  )

  check(
    "function-params-1",
    """|object Main extends App {
       |  def func(a@@: Int, b: Int) =
       |    a + b
       |}""".stripMargin,
    List[String](
      """|object Main extends App {
         |  def func(>>region>>a<<region<<: Int, b: Int) =
         |    a + b
         |}""".stripMargin,
      """|object Main extends App {
         |  def func(>>region>>a: Int<<region<<, b: Int) =
         |    a + b
         |}""".stripMargin,
      """|object Main extends App {
         |  def func(>>region>>a: Int, b: Int<<region<<) =
         |    a + b
         |}""".stripMargin,
      """|object Main extends App {
         |  >>region>>def func(a: Int, b: Int) =
         |    a + b<<region<<
         |}""".stripMargin
    )
  )

  check(
    "function-params-2",
    """|object Main extends App {
       |  val func = (a@@: Int, b: Int) =>
       |    a + b
       |}""".stripMargin,
    List[String](
      """|object Main extends App {
         |  val func = (>>region>>a<<region<<: Int, b: Int) =>
         |    a + b
         |}""".stripMargin,
      """|object Main extends App {
         |  val func = (>>region>>a: Int<<region<<, b: Int) =>
         |    a + b
         |}""".stripMargin,
      """|object Main extends App {
         |  val func = (>>region>>a: Int, b: Int<<region<<) =>
         |    a + b
         |}""".stripMargin,
      """|object Main extends App {
         |  val func = >>region>>(a: Int, b: Int) =>
         |    a + b<<region<<
         |}""".stripMargin,
      """|object Main extends App {
         |  >>region>>val func = (a: Int, b: Int) =>
         |    a + b<<region<<
         |}""".stripMargin
    )
  )

  check(
    "def - type params",
    "object Main extends App { def foo[Type@@ <: T1, B](hi: Int, b: Int, c:Int) = ??? }",
    List(
      "object Main extends App { def foo[>>region>>Type<<region<< <: T1, B](hi: Int, b: Int, c:Int) = ??? }",
      "object Main extends App { def foo[>>region>>Type <: T1<<region<<, B](hi: Int, b: Int, c:Int) = ??? }",
      "object Main extends App { def foo[>>region>>Type <: T1, B<<region<<](hi: Int, b: Int, c:Int) = ??? }",
      "object Main extends App { >>region>>def foo[Type <: T1, B](hi: Int, b: Int, c:Int) = ???<<region<< }"
    )
  )

  check(
    "arithmetic",
    """|object Main extends App {
       |  def x = 12 * (34 + 5@@6)
       |}""".stripMargin,
    List(
      """|object Main extends App {
         |  def x = 12 * (34 + >>region>>56<<region<<)
         |}""".stripMargin,
      """|object Main extends App {
         |  def x = 12 * (>>region>>34 + 56<<region<<)
         |}""".stripMargin,
      """|object Main extends App {
         |  def x = 12 * >>region>>(34 + 56)<<region<<
         |}""".stripMargin,
      """|object Main extends App {
         |  def x = >>region>>12 * (34 + 56)<<region<<
         |}""".stripMargin
    )
  )

  check(
    "function",
    "val hello = (aaa: Int, bb@@b: Int, ccc: Int) => ???",
    List(
      "val hello = (aaa: Int, >>region>>bbb<<region<<: Int, ccc: Int) => ???",
      "val hello = (aaa: Int, >>region>>bbb: Int<<region<<, ccc: Int) => ???",
      "val hello = (>>region>>aaa: Int, bbb: Int, ccc: Int<<region<<) => ???",
      "val hello = >>region>>(aaa: Int, bbb: Int, ccc: Int) => ???<<region<<",
      ">>region>>val hello = (aaa: Int, bbb: Int, ccc: Int) => ???<<region<<"
    )
  )

  check(
    "defdef",
    "def hello(aaa: Int, bb@@b: Int, ccc: Int) = ???",
    List(
      "def hello(aaa: Int, >>region>>bbb<<region<<: Int, ccc: Int) = ???",
      "def hello(aaa: Int, >>region>>bbb: Int<<region<<, ccc: Int) = ???",
      "def hello(>>region>>aaa: Int, bbb: Int, ccc: Int<<region<<) = ???",
      ">>region>>def hello(aaa: Int, bbb: Int, ccc: Int) = ???<<region<<"
    )
  )

  check(
    "apply",
    "def hello = List(111, 2@@22, 333)",
    List(
      "def hello = List(111, >>region>>222<<region<<, 333)",
      "def hello = List(>>region>>111, 222, 333<<region<<)",
      "def hello = >>region>>List(111, 222, 333)<<region<<",
      ">>region>>def hello = List(111, 222, 333)<<region<<"
    )
  )

  check(
    "type-apply",
    "def hello = Map[String, I@@nt]()",
    List(
      "def hello = Map[String, >>region>>Int<<region<<]()",
      "def hello = Map[>>region>>String, Int<<region<<]()",
      "def hello = >>region>>Map[String, Int]<<region<<()",
      "def hello = >>region>>Map[String, Int]()<<region<<",
      ">>region>>def hello = Map[String, Int]()<<region<<"
    )
  )

  check(
    "unapply",
    "val List(aaa, b@@bb, ccc) = List(111, 222, 333)",
    List(
      "val List(aaa, >>region>>bbb<<region<<, ccc) = List(111, 222, 333)",
      "val List(>>region>>aaa, bbb, ccc<<region<<) = List(111, 222, 333)",
      "val >>region>>List(aaa, bbb, ccc)<<region<< = List(111, 222, 333)",
      ">>region>>val List(aaa, bbb, ccc) = List(111, 222, 333)<<region<<"
    )
  )

  check(
    "single",
    "def hello = List(2@@22)",
    List(
      "def hello = List(>>region>>222<<region<<)",
      "def hello = >>region>>List(222)<<region<<",
      ">>region>>def hello = List(222)<<region<<"
    )
  )

  check(
    "constructor-argument",
    "class Foo(val ar@@g: Int)",
    List(
      """class Foo(val >>region>>arg<<region<<: Int)""",
      """class Foo(>>region>>val arg: Int<<region<<)"""
    )
  )

  check(
    "object-backticked",
    "object `Foo B@@ar Baz` extends SomeTrait",
    List(
      """object `>>region>>Foo Bar Baz<<region<<` extends SomeTrait""",
      """>>region>>object `Foo Bar Baz` extends SomeTrait<<region<<"""
    )
  )

  check(
    "def-lhs",
    "object Main extends App { def `foo ba@@r` = ??? }",
    List(
      """object Main extends App { def `>>region>>foo bar<<region<<` = ??? }""",
      """object Main extends App { >>region>>def `foo bar` = ???<<region<< }"""
    )
  )

  check(
    "expr-apply",
    "object Main extends App { def foo = bar.baz(1, math.floor(p@@i), 2) }",
    List(
      """object Main extends App { def foo = bar.baz(1, math.floor(>>region>>pi<<region<<), 2) }""",
      """object Main extends App { def foo = bar.baz(1, >>region>>math.floor(pi)<<region<<, 2) }""",
      """object Main extends App { def foo = bar.baz(>>region>>1, math.floor(pi), 2<<region<<) }""",
      """object Main extends App { def foo = >>region>>bar.baz(1, math.floor(pi), 2)<<region<< }""",
      """object Main extends App { >>region>>def foo = bar.baz(1, math.floor(pi), 2)<<region<< }"""
    )
  )

  check(
    "expr-backticked",
    "object Main extends App { def foo = `foo ba@@r` + 1 }",
    List(
      """object Main extends App { def foo = >>region>>`foo bar`<<region<< + 1 }""",
      """object Main extends App { def foo = >>region>>`foo bar` + 1<<region<< }""",
      """object Main extends App { >>region>>def foo = `foo bar` + 1<<region<< }"""
    )
  )

  check(
    "type-apply-nested",
    "object Main extends App { def foo: Tuple3[Int, List[In@@t], Double] = ??? }",
    List(
      """object Main extends App { def foo: Tuple3[Int, List[>>region>>Int<<region<<], Double] = ??? }""",
      """object Main extends App { def foo: Tuple3[Int, >>region>>List[Int]<<region<<, Double] = ??? }""",
      """object Main extends App { def foo: Tuple3[>>region>>Int, List[Int], Double<<region<<] = ??? }""",
      """object Main extends App { def foo: >>region>>Tuple3[Int, List[Int], Double]<<region<< = ??? }""",
      """object Main extends App { >>region>>def foo: Tuple3[Int, List[Int], Double] = ???<<region<< }"""
    )
  )

  check(
    "comment-match",
    """|object Main extends App {
       |  /*com@@ment*/
       |  val total = for {
       |    a <- Some(1)
       |    b <- Some(2)
       |  } yield a + b
       |}""".stripMargin,
    List(
      """|object Main extends App {
         |  >>region>>/*comment*/<<region<<
         |  val total = for {
         |    a <- Some(1)
         |    b <- Some(2)
         |  } yield a + b
         |}""".stripMargin,
      """|object Main extends >>region>>App {
         |  /*comment*/
         |  val total = for {
         |    a <- Some(1)
         |    b <- Some(2)
         |  } yield a + b<<region<<
         |}
         |""".stripMargin
    )
  )
