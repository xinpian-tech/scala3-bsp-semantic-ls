/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/definition/PcDefinitionSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness; curated subset
 * favoring export/enum/derives/extension syntax. Cross-file expectations
 * resolve through CorpusPc.MockResolver (the PcDefinitionResolver seam).
 */
package ls.pc.corpus

import org.eclipse.lsp4j.Location

class PcDefinitionCorpusSuite extends CorpusDefinitionHarness:

  def definitions(uri: String, line: Int, character: Int): List[Location] =
    CorpusPc.facade.definition(uri, line, character).locations.map(_.location).toList

  check(
    "basic",
    """|
       |object Main {
       |  val <<abc>> = 42
       |  println(a@@bc)
       |}
       |""".stripMargin
  )

  check(
    "for",
    """|object Main {
       |  for {
       |    <<x>> <- List(1)
       |    y <- 1.to(x)
       |    z = y + x
       |    if y < @@x
       |  } yield y
       |}
       |""".stripMargin
  )

  check(
    "function",
    """|
       |object Main {
       |  val <<increment>>: Int => Int = _ + 2
       |  incre@@ment(1)
       |}
       |""".stripMargin
  )

  check(
    "apply",
    """|
       |object Main {
       |  /*scala/package.List. package.scala*/@@List(1)
       |}
       |""".stripMargin
  )

  check(
    "import1",
    """|
       |import scala.concurrent./*scala/concurrent/Future# Future.scala*//*scala/concurrent/Future. Future.scala*/@@Future
       |object Main {
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

  check(
    "symbolic-infix",
    """|
       |object Main {
       |  val lst = 1 /*scala/collection/immutable/List#`::`(). List.scala*/@@:: Nil
       |}
       |""".stripMargin
  )

  check(
    "exportType0",
    """object Foo:
      |  trait <<Cat>>
      |object Bar:
      |  export Foo.*
      |class Test:
      |  import Bar.*
      |  def test = new Ca@@t {}
      |""".stripMargin
  )

  check(
    "exportType1",
    """object Foo:
      |  trait <<Cat>>[A]
      |object Bar:
      |  export Foo.*
      |class Test:
      |  import Bar.*
      |  def test = new Ca@@t[Int] {}
      |""".stripMargin
  )

  check(
    "exportTerm0Nullary",
    """trait Foo:
      |  def <<meth>>: Int
      |class Bar(val foo: Foo):
      |  export foo.*
      |  def test(bar: Bar) = bar.me@@th
      |""".stripMargin
  )

  check(
    "exportTerm0",
    """trait Foo:
      |  def <<meth>>(): Int
      |class Bar(val foo: Foo):
      |  export foo.*
      |  def test(bar: Bar) = bar.me@@th()
      |""".stripMargin
  )

  check(
    "exportTerm1",
    """trait Foo:
      |  def <<meth>>(x: Int): Int
      |class Bar(val foo: Foo):
      |  export foo.*
      |  def test(bar: Bar) = bar.me@@th(0)
      |""".stripMargin
  )

  check(
    "exportTerm1Poly",
    """trait Foo:
      |  def <<meth>>[A](x: A): A
      |class Bar(val foo: Foo):
      |  export foo.*
      |  def test(bar: Bar) = bar.me@@th(0)
      |""".stripMargin
  )

  check(
    "exportTerm1Overload",
    """trait Foo:
      |  def <<meth>>(x: Int): Int
      |  def meth(x: String): String
      |class Bar(val foo: Foo):
      |  export foo.*
      |  def test(bar: Bar) = bar.me@@th(0)
      |""".stripMargin
  )

  check(
    "exportTermExtension",
    """|package a
       |class Test extends A {
       |  assert("Hello".fo@@o == "HelloFoo")
       |}
       |
       |trait A {
       |  export B.*
       |}
       |
       |object B {
       |  extension (value: String) def <<foo>>: String = s"${value}Foo"
       |}
       |""".stripMargin
  )

  check(
    "enum-class-type-param",
    """|
       |enum Options[<<AA>>]:
       |  case Some(x: A@@A)
       |  case None extends Options[Nothing]
       |""".stripMargin
  )

  check(
    "enum-class-type-param-covariant",
    """|
       |enum Options[+<<AA>>]:
       |  case Some(x: A@@A)
       |  case None extends Options[Nothing]
       |""".stripMargin
  )

  check(
    "enum-class-type-param-duplicate",
    """|
       |enum Testing[AA]:
       |  case Some[<<AA>>](x: A@@A) extends Testing[AA]
       |  case None extends Testing[Nothing]
       |""".stripMargin
  )

  check(
    "derives-def",
    """|
       |import scala.deriving.Mirror
       |
       |trait <<Show>>[A]:
       |  def show(a: A): String
       |
       |object Show:
       |  inline def derived[T](using Mirror.Of[T]): Show[T] = new Show[T]:
       |    override def show(a: T): String = a.toString
       |
       |case class Box[A](value: A) derives Sh@@ow
       |
       |""".stripMargin
  )

  check(
    "implicit-extension",
    """|class MyIntOut(val value: Int)
       |object MyIntOut:
       |  extension (i: MyIntOut) def <<uneven>> = i.value % 2 == 1
       |
       |val a = MyIntOut(1).un@@even
       |""".stripMargin
  )

  check(
    "named-tuples",
    """|
       |val <<foo>> = (name = "Bob", age = 42, height = 1.9d)
       |val foo_name = foo.na@@me
       |""".stripMargin
  )

  check(
    "i7256",
    """|object Test:
       |  def <<methodA>>: Unit = ???
       |export Test.me@@thodA
       |""".stripMargin
  )

  check(
    "i7256-2",
    """|object Test:
       |  def <<methodA>>: Unit = ???
       |  def methodB: Unit = ???
       |export Test.{me@@thodA, methodB}
       |""".stripMargin
  )

  check(
    "i7256-3",
    """|object Test:
       |  def <<methodA>>: Unit = ???
       |  def methodB: Unit = ???
       |export Test.{methodA, methodB}
       |
       |val i = met@@hodA
       |""".stripMargin
  )

  check(
    "i7427",
    """|package a
       |object Repro:
       |    export scala.collection.immutable.V/*scala/collection/immutable/Vector. Vector.scala*/@@ector
       |""".stripMargin
  )
