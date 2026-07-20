/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/edit/InsertInferredTypeSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness (the PcFacade
 * codeAction(InsertInferredType) carrier; LSP positions instead of offsets).
 * Curated 16 of 69 cases favoring Scala 3 syntax (toplevel defs, named tuples, enums, backticks) plus the definition-form matrix (val/var/def/params/tuples), the auto-import edits, and the wrong-ascription rewrites; the remainder are repetition of the same shapes (dealias/refined/operator families).
 */
package ls.pc.corpus

class InsertInferredTypeCorpusSuite extends CorpusCodeActionEditHarness(ls.pc.PcCodeActionId.InsertInferredType):

  checkEdit(
    "val",
    """|object A{
       |  val <<alpha>> = 123
       |}""".stripMargin,
    """|object A{
       |  val alpha: Int = 123
       |}""".stripMargin
  )

  checkEdit(
    "wrong-def-params",
    """|object A{
       |  def <<alpha>>(a: Int, b: String): String = 123
       |}""".stripMargin,
    """|object A{
       |  def alpha(a: Int, b: String): Int = 123
       |}""".stripMargin
  )

  checkEdit(
    "wrong-val",
    """|object A{
       |  val <<alpha>>:  String = 123
       |}""".stripMargin,
    """|object A{
       |  val alpha: Int = 123
       |}""".stripMargin
  )

  checkEdit(
    "java-enum",
    """|object A{
       |  final val <<javaEnum>> = java.util.Locale.Category.DISPLAY
       |}""".stripMargin,
    """|import java.util.Locale.Category
       |object A{
       |  final val javaEnum: Category = java.util.Locale.Category.DISPLAY
       |}""".stripMargin
  )

  checkEdit(
    "toplevel",
    """|def <<alpha>> = List("")
       |""".stripMargin,
    """|def alpha: List[String] = List("")
       |""".stripMargin
  )

  checkEdit(
    "tuple",
    """|object A{
       |  val (<<alpha>>, beta) = (123, 12)
       |}""".stripMargin,
    """|object A{
       |  val (alpha: Int, beta) = (123, 12)
       |}""".stripMargin
  )

  checkEdit(
    "var",
    """|object A{
       |  var <<alpha>> = (123, 12)
       |}""".stripMargin,
    """|object A{
       |  var alpha: (Int, Int) = (123, 12)
       |}""".stripMargin
  )

  checkEdit(
    "def",
    """|object A{
       |  def <<alpha>> = (123, 12)
       |}""".stripMargin,
    """|object A{
       |  def alpha: (Int, Int) = (123, 12)
       |}""".stripMargin
  )

  checkEdit(
    "def-param",
    """|object A{
       |  def <<alpha>>(a : String) = (123, 12)
       |}""".stripMargin,
    """|object A{
       |  def alpha(a : String): (Int, Int) = (123, 12)
       |}""".stripMargin
  )

  checkEdit(
    "auto-import",
    """|object A{
       |  val <<buffer>> = List("").toBuffer
       |}""".stripMargin,
    """|import scala.collection.mutable.Buffer
       |object A{
       |  val buffer: Buffer[String] = List("").toBuffer
       |}""".stripMargin
  )

  checkEdit(
    "lambda",
    """|object A{
       |  val toStringList = List(1, 2, 3).map(<<int>> => int.toString)
       |}""".stripMargin,
    """|object A{
       |  val toStringList = List(1, 2, 3).map((int: Int) => int.toString)
       |}""".stripMargin
  )

  checkEdit(
    "pattern-match-option",
    """|object A{
       |  Option(1) match {
       |    case Some(<<t>>) => t
       |    case None =>
       |  }
       |}""".stripMargin,
    """|object A{
       |  Option(1) match {
       |    case Some(t: Int) => t
       |    case None =>
       |  }
       |}
       |""".stripMargin
  )

  checkEdit(
    "for-comprehension",
    """|object A{
       |  for {
       |    <<i>> <- 1 to 10
       |    j <- 1 to 11
       |  } yield (i, j)
       |}""".stripMargin,
    """|object A{
       |  for {
       |    i: Int <- 1 to 10
       |    j <- 1 to 11
       |  } yield (i, j)
       |}
       |""".stripMargin
  )

  checkEdit(
    "backticks-1",
    """|object O{
       |  val <<`bar`>> = 42
       |}""".stripMargin,
    """|object O{
       |  val `bar`: Int = 42
       |}
       |""".stripMargin
  )

  checkEdit(
    "named-tuples",
    """|def hello = (path = ".", num = 5)
       |
       |def <<test>> =
       |  hello ++ (line = 1)
       |
       |@main def bla =
       |   val x: (path: String, num: Int, line: Int) = test
       |""".stripMargin,
    """|def hello = (path = ".", num = 5)
       |
       |def test: (path : String, num : Int, line : Int) =
       |  hello ++ (line = 1)
       |
       |@main def bla =
       |   val x: (path: String, num: Int, line: Int) = test
       |""".stripMargin
  )

  checkEdit(
    "enums",
    """|object EnumerationValue:
       |  object Day extends Enumeration {
       |    type Day = Value
       |    val Weekday, Weekend = Value
       |  }
       |  object Bool extends Enumeration {
       |    type Bool = Value
       |    val True, False = Value
       |  }
       |  import Bool._
       |  def day(d: Day.Value): Unit = ???
       |  val <<d>> =
       |    if (true) Day.Weekday
       |    else Day.Weekend
       |""".stripMargin,
    """|object EnumerationValue:
       |  object Day extends Enumeration {
       |    type Day = Value
       |    val Weekday, Weekend = Value
       |  }
       |  object Bool extends Enumeration {
       |    type Bool = Value
       |    val True, False = Value
       |  }
       |  import Bool._
       |  def day(d: Day.Value): Unit = ???
       |  val d: EnumerationValue.Day.Value =
       |    if (true) Day.Weekday
       |    else Day.Weekend
       |""".stripMargin
  )
