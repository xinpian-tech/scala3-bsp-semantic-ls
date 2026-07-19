/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/completion/CompletionCaseSuite.scala
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/completion/CompletionMatchSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness; curated
 * enum-exhaustiveness subset.
 */
package ls.pc.corpus

class CompletionCaseCorpusSuite extends CorpusCompletionHarness:

  // from CompletionCaseSuite
  check(
    "scala-enum",
    """
      |package example
      |enum Color:
      |  case Red, Blue, Green
      |
      |object Main {
      |  val x: Color = ???
      |  x match
      |    case@@
      |}""".stripMargin,
    """|case Color.Blue =>
       |case Color.Green =>
       |case Color.Red =>
       |""".stripMargin
  )

  // from CompletionCaseSuite
  check(
    "scala-enum2",
    """
      |package example
      |enum Color:
      |  case Red, Blue, Green
      |
      |object Main {
      |  val colors = List(Color.Red, Color.Green).map{
      |    case C@@
      |  }
      |}""".stripMargin,
    """|Color.Blue
       |Color.Green
       |Color.Red
       |""".stripMargin,
    topLines = Some(3)
  )

  // from CompletionCaseSuite
  checkEdit(
    "scala-enum-with-param",
    """
      |package withenum {
      |enum Foo:
      |  case Bla, Bar
      |  case Buzz(arg1: Int, arg2: Int)
      |}
      |package example
      |object Main {
      |  val x: withenum.Foo = ???
      |  x match
      |    case@@
      |}""".stripMargin,
    """
      |import withenum.Foo
      |
      |package withenum {
      |enum Foo:
      |  case Bla, Bar
      |  case Buzz(arg1: Int, arg2: Int)
      |}
      |package example
      |object Main {
      |  val x: withenum.Foo = ???
      |  x match
      |    case Foo.Buzz(arg1, arg2) => $0
      |}""".stripMargin,
    filter = _.contains("Buzz")
  )

  // from CompletionCaseSuite
  check(
    "exhaustive-enum-tags",
    """|object Tags:
       |  trait Hobby
       |  trait Chore
       |  trait Physical
       |
       |
       |import Tags.*
       |
       |enum Activity:
       |  case Reading(book: String, author: String) extends Activity, Hobby
       |  case Sports(time: Long, intensity: Double) extends Activity, Physical, Hobby
       |  case Cleaning                              extends Activity, Physical, Chore
       |  case Singing(song: String)                 extends Activity, Hobby
       |  case DishWashing(amount: Int)              extends Activity, Chore
       |
       |import Activity.*
       |
       |def energySpend(act: Activity & (Physical | Chore)): Double =
       |  act match
       |    cas@@
       |
       |""".stripMargin,
    """|case Cleaning =>Activity & Physical & Chore
       |case DishWashing(amount) => test.Activity
       |case Sports(time, intensity) => test.Activity""".stripMargin
  )

  // from CompletionMatchSuite
  checkEdit(
    "exhaustive-scala-enum",
    """
      |package withenum {
      |enum Color(rank: Int):
      |  case Red extends Color(1)
      |  case Blue extends Color(2)
      |  case Green extends Color(3)
      |}
      |
      |package example
      |
      |object Main {
      |  val x: withenum.Color = ???
      |  x match@@
      |}""".stripMargin,
    s"""|import withenum.Color
        |
        |package withenum {
        |enum Color(rank: Int):
        |  case Red extends Color(1)
        |  case Blue extends Color(2)
        |  case Green extends Color(3)
        |}
        |
        |package example
        |
        |object Main {
        |  val x: withenum.Color = ???
        |  x match
        |\tcase Color.Red => $$0
        |\tcase Color.Blue =>
        |\tcase Color.Green =>
        |
        |}
        |""".stripMargin,
    filter = _.contains("exhaustive")
  )
