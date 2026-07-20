/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/edit/ConvertToNamedLambdaParametersSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness (the PcFacade
 * codeAction(ConvertToNamedLambdaParameters) carrier; LSP positions instead of offsets).
 * Curated 5 of 12 cases: single and multi-underscore lambdas, nested lambdas, and the match-with-wildcard shape; the remainder re-walk the same shapes over other element types (the upstream eta-expansion case is @Ignore'd there too).
 */
package ls.pc.corpus

class ConvertToNamedLambdaParametersCorpusSuite extends CorpusCodeActionEditHarness(ls.pc.PcCodeActionId.ConvertToNamedLambdaParameters):

  checkEdit(
    "Int => Int function in map",
    """|object A{
    |  val a = List(1, 2).map(<<_>> + 1)
    |}""".stripMargin,
    """|object A{
    |  val a = List(1, 2).map(i => i + 1)
    |}""".stripMargin
  )

  checkEdit(
    "String => String function in map",
    """|object A{
    |  val a = List("a", "b").map(<<_>> + "c")
    |}""".stripMargin,
    """|object A{
    |  val a = List("a", "b").map(s => s + "c")
    |}""".stripMargin
  )

  checkEdit(
    "(String, Int) => Int function in map with multiple underscores",
    """|object A{
    |  val a = List(("a", 1), ("b", 2)).map(<<_>> + _)
    |}""".stripMargin,
    """|object A{
    |  val a = List(("a", 1), ("b", 2)).map((s, i) => s + i)
    |}""".stripMargin
  )

  checkEdit(
    "Int => Float function in nested lambda 1",
    """|object A{
    |  val a = List(1, 2).flatMap(List(_).flatMap(v => List(v, v + 1).map(<<_>>.toFloat)))
    |}""".stripMargin,
    """|object A{
    |  val a = List(1, 2).flatMap(List(_).flatMap(v => List(v, v + 1).map(i => i.toFloat)))
    |}""".stripMargin
  )

  checkEdit(
    "Long => Long with match and wildcard pattern",
    """|object A{
    |  val a = List(1L, 2L).map(_ match {
    |    case 1L => 1L
    |    case _ => <<2L>>
    |  })
    |}""".stripMargin,
    """|object A{
    |  val a = List(1L, 2L).map(l => l match {
    |    case 1L => 1L
    |    case _ => 2L
    |  })
    |}""".stripMargin
  )
