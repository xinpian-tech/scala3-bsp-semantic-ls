/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/definition/TypeDefinitionSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness; curated subset.
 * Cross-file expectations resolve through CorpusPc.MockResolver.
 */
package ls.pc.corpus

import org.eclipse.lsp4j.Location

class TypeDefinitionCorpusSuite extends CorpusDefinitionHarness:

  def definitions(uri: String, line: Int, character: Int): List[Location] =
    CorpusPc.facade.typeDefinition(uri, line, character).locations.map(_.location).toList

  check(
    "val",
    """|class <<TClass>>(i: Int)
       |
       |object Main {
       |  val ts@@t = new TClass(2)
       |}""".stripMargin
  )

  check(
    "for",
    """|
       |object Main {
       |  for {
       |    x <- List(1)
       |    y <- 1.to(x)
       |    z = y + x
       |    if y < /*scala/Int# Int.scala*/@@x
       |  } yield y
       |}
       |""".stripMargin
  )

  check(
    "for-flatMap",
    """|
       |object Main {
       |  for {
       |    x /*scala/Option# Option.scala*/@@<- Option(1)
       |    y <- Option(x)
       |  } yield y
       |}
       |""".stripMargin
  )

  check(
    "constructor",
    """
      |class <<TClass>>(i: Int) {}
      |
      |object Main {
      | def tst(m: TClass): Unit = {}
      |
      |  tst(new T@@Class(2))
      |}""".stripMargin
  )

  check(
    "function",
    """|
       |object Main {
       |  val increment: Int => Int = _ + 2
       |  incre/*scala/Int# Int.scala*/@@ment(1)
       |}
       |""".stripMargin
  )

  check(
    "method",
    """|object Main {
       |  def tst(): Unit = {}
       |
       |  ts@@/*scala/Unit# Unit.scala*/t()
       |}""".stripMargin
  )

  check(
    "named-arg-multiple",
    """|object Main {
       |  def tst(par1: Int, par2: String, par3: Boolean): Unit = {}
       |
       |  tst(1, p/*scala/Boolean# Boolean.scala*/@@ar3 = true, par2 = "")
       |}""".stripMargin
  )

  check(
    "named-arg-reversed",
    """|object Main {
       |  def tst(par1: Int, par2: String): Unit = {}
       |
       |  tst(p/*scala/Predef.String# Predef.scala*/@@ar2 = "foo", par1 = 1)
       |}""".stripMargin
  )

  check(
    "named-arg-local",
    """|object Main {
       |  def foo(arg: Int): Unit = ()
       |
       |  foo(a/*scala/Int# Int.scala*/@@rg = 42)
       |}
       |""".stripMargin
  )

  check(
    "list",
    """|object Main {
       |  List(1).hea/*scala/Int# Int.scala*/@@d
       |}
       |""".stripMargin
  )

  check(
    "class",
    """|object Main {
       |  class <<F@@oo>>(val x: Int)
       |}
       |""".stripMargin
  )

  check(
    "literal",
    """|object Main {
       |  val x = 4/*scala/Int# Int.scala*/@@2
       |}
       |""".stripMargin
  )

  check(
    "method-generic",
    """|object Main {
       |  def foo[<<T>>](param: T): T = para@@m
       |}
       |""".stripMargin
  )

  check(
    "method-generic-result",
    """|object A {
       |  def foo[T](param: T): T = param
       |}
       |object Main {
       |  println(A.fo/*scala/Int# Int.scala*/@@o(2))
       |}
       |""".stripMargin
  )

  check(
    "apply",
    """|
       |object Main {
       |  /*scala/collection/immutable/List# List.scala*/@@List(1)
       |}
       |""".stripMargin
  )

  check(
    "symbolic-infix",
    """|
       |object Main {
       |  val lst = 1 /*scala/collection/immutable/List# List.scala*/@@:: Nil
       |}
       |""".stripMargin
  )

  check(
    "result-type",
    """|
       |object Main {
       |  def x: /*scala/Int# Int.scala*/@@Int = 42
       |}
       |""".stripMargin
  )
