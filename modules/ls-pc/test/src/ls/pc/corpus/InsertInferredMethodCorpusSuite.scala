/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/edit/InsertInferredMethodSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness (the PcFacade
 * codeAction(InsertInferredMethod) carrier; LSP positions instead of offsets).
 * Curated 8 of 29 cases: the argument-inference matrix (simple calls, no-args, custom types, lambdas, val definitions), backticked names and extension methods; the remainder re-walk the same shapes in class/object hosts.
 */
package ls.pc.corpus

class InsertInferredMethodCorpusSuite extends CorpusCodeActionEditHarness(ls.pc.PcCodeActionId.InsertInferredMethod):

  checkEdit(
    "simple",
    """|
      |trait Main {
      |  def method1(s : String) = 123
      |
      |  method1(<<otherMethod>>(1))
      |}
      |
      |""".stripMargin,
    """|trait Main {
      |  def method1(s : String) = 123
      |
      |  def otherMethod(arg0: Int): String = ???
      |  method1(otherMethod(1))
      |}
      |""".stripMargin
  )

  checkEdit(
    "simple-2",
    """|
      |trait Main {
      |  def method1(s : String) = 123
      |
      |  <<otherMethod>>(1)
      |}
      |
      |""".stripMargin,
    """|trait Main {
      |  def method1(s : String) = 123
      |
      |  def otherMethod(arg0: Int) = ???
      |  otherMethod(1)
      |}
      |""".stripMargin
  )

  checkEdit(
    "backtick-method-name",
    """|
      |trait Main {
      |  <<`met ? hod`>>(10)
      |}
      |""".stripMargin,
    """|trait Main {
      |  def `met ? hod`(arg0: Int) = ???
      |  `met ? hod`(10)
      |}
      |""".stripMargin
  )

  checkEdit(
    "custom-type",
    """|
     |trait Main {
     |  def method1(b: Double, s : String) = 123
     |
     |  case class User(i : Int)
     |  val user = User(1)
     |
     |  method1(0.0, <<otherMethod>>(user, 1))
     |}
     |""".stripMargin,
    """|
     |trait Main {
     |  def method1(b: Double, s : String) = 123
     |
     |  case class User(i : Int)
     |  val user = User(1)
     |
     |  def otherMethod(arg0: User, arg1: Int): String = ???
     |  method1(0.0, otherMethod(user, 1))
     |}
     |""".stripMargin
  )

  checkEdit(
    "val-definition",
    """|
      |trait Main {
      |  val result: String = <<nonExistent>>(42, "hello")
      |}
      |
      |""".stripMargin,
    """|trait Main {
      |  def nonExistent(arg0: Int, arg1: String): String = ???
      |  val result: String = nonExistent(42, "hello")
      |}
      |""".stripMargin
  )

  checkEdit(
    "lambda-expression",
    """|
      |trait Main {
      |  val list = List(1, 2, 3)
      |  list.map(<<transform>>)
      |}
      |
      |""".stripMargin,
    """|trait Main {
      |  val list = List(1, 2, 3)
      |  def transform(arg0: Int) = ???
      |  list.map(transform)
      |}
      |""".stripMargin
  )

  checkEdit(
    "simple-method-no-args",
    """|
      |trait Main {
      |  <<missingMethod>>
      |}
      |
      |""".stripMargin,
    """|trait Main {
      |  def missingMethod = ???
      |  missingMethod
      |}
      |""".stripMargin
  )

  checkEdit(
    "extension-method",
    """|object Main:
       |  val x = 1
       |  x.<<incr>>
       |""".stripMargin,
    """|object Main:
       |  val x = 1
       |  extension (x: Int)
       |    def incr = ???
       |  x.incr
       |""".stripMargin
  )
