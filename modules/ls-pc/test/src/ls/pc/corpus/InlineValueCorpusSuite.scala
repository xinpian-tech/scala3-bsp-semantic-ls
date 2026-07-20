/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/edit/InlineValueSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness (the PcFacade
 * codeAction(InlineValue) carrier; LSP positions instead of offsets).
 * Curated 8 of 37 cases: the local/all-references/precedence bracket shapes, lambdas, for-comprehensions, plus BOTH refusal families (non-local definition, shadowed-variable scoping) exercising the DisplayableException refusal-as-data mapping; the remainder re-walk the same interpolation/scoping shapes.
 */
package ls.pc.corpus

import scala.meta.internal.pc.InlineValueProvider.Errors as InlineErrors

class InlineValueCorpusSuite extends CorpusCodeActionEditHarness(ls.pc.PcCodeActionId.InlineValue):

  checkEdit(
    "inline-local",
    """|object Main {
       |  def u(): Unit = {
       |    val o: Int = 1
       |    val p: Int = <<o>> + 2
       |  }
       |}""".stripMargin,
    """|object Main {
       |  def u(): Unit = {
       |    val p: Int = 1 + 2
       |  }
       |}""".stripMargin
  )

  checkEdit(
    "inline-local-same-name",
    """|object Main {
       |  val a = { val a = 1; val b = <<a>> + 1 }
       |}""".stripMargin,
    """|object Main {
       |  val a = { val b = 1 + 1 }
       |}""".stripMargin
  )

  checkEdit(
    "inline-all-local",
    """|object Main {
       |  def u(): Unit = {
       |    val <<o>>: Int = 1
       |    val p: Int = o + 2
       |    val i: Int = o + 3
       |  }
       |}""".stripMargin,
    """|object Main {
       |  def u(): Unit = {
       |    val p: Int = 1 + 2
       |    val i: Int = 1 + 3
       |  }
       |}""".stripMargin
  )

  checkEdit(
    "inline-local-brackets",
    """|object Main {
       |  def u(): Unit = {
       |    val o: Int = 1 + 6
       |    val p: Int = 2 - <<o>>
       |    val k: Int = o
       |  }
       |}""".stripMargin,
    """|object Main {
       |  def u(): Unit = {
       |    val o: Int = 1 + 6
       |    val p: Int = 2 - (1 + 6)
       |    val k: Int = o
       |  }
       |}""".stripMargin
  )

  checkEdit(
    "lambda-apply",
    """|object Main {
       |  def demo = {
       |    val plus1 = (x: Int) => x + 1
       |    println(<<plus1>>(1))
       |  }
       |}""".stripMargin,
    """|object Main {
       |  def demo = {
       |    println(((x: Int) => x + 1)(1))
       |  }
       |}""".stripMargin
  )

  checkEdit(
    "for-comprehension",
    """|object Main {
       |val a =
       |  for {
       |    i <- List(1,2,3)
       |  } yield i + 1
       |val b = <<a>>.map(_ + 1)
       |}""".stripMargin,
    """|object Main {
       |val a =
       |  for {
       |    i <- List(1,2,3)
       |  } yield i + 1
       |val b = (
       |  for {
       |    i <- List(1,2,3)
       |  } yield i + 1).map(_ + 1)
       |}""".stripMargin
  )

  checkRefusal(
    "inline-all-not-local",
    """|object Main {
       |  val <<o>>: Int = 6
       |  val p: Int = 2 - o
       |}""".stripMargin,
    InlineErrors.notLocal
  )

  checkRefusal(
    "bad-scoping",
    """|object Demo {
       |  def oo(j : Int) = {
       |    val m = j + 3
       |    def kk() = {
       |      val j = 0
       |      <<m>>
       |    }
       |  }
       |}""".stripMargin,
    InlineErrors.variablesAreShadowed("Demo.oo.j")
  )
