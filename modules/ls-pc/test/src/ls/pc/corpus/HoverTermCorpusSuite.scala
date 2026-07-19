/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/hover/HoverTermSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness; curated subset.
 */
package ls.pc.corpus

class HoverTermCorpusSuite extends CorpusHoverHarness:

  check(
    "implicit-conv",
    """
      |object Main {
      |  <<"".substring(0, 1).stripSu@@ffix("")>>
      |}
      |""".stripMargin,
    """|def stripSuffix(suffix: String): String
       |""".stripMargin.hover
  )

  check(
    "implicit-conv2",
    """case class Text[T](value: T)
      |object Text {
      |  implicit def conv[T](value: T): Text[T] =
      |    Text(value)
      |}
      |object Main {
      |  def foo[T](text: Text[T]): T = text.value
      |  val number = 42
      |  foo(<<num@@ber>>)
      |}
      |""".stripMargin,
    """|val number: Int
       |""".stripMargin.hover
  )

  check(
    "toplevel",
    """|
       |val (first, <<se@@cond>>) = (1, false)
       |""".stripMargin,
    "val second: Boolean".hover
  )

  check(
    "right-assoc-extension",
    """
      |case class Wrap[+T](x: T)
      |
      |extension [T](a: T)
      |  def <<*@@:>>[U <: Tuple](b: Wrap[U]): Wrap[T *: U] = Wrap(a *: b.x)
      |""".stripMargin,
    "extension [T](a: T) def *:[U <: Tuple](b: Wrap[U]): Wrap[T *: U]".hover
  )

  check(
    "dont-ignore-???-in-path",
    """object Obj:
      |  val x = ?@@??
      |""".stripMargin,
    """def ???: Nothing""".stripMargin.hover
  )

  check(
    "named-tuples",
    """
      |val foo = (name = "Bob", age = 42, height = 1.9d)
      |val foo_name = foo.na@@me
      |""".stripMargin,
    "name: String".hover
  )

  check(
    "named-tuples2",
    """|import NamedTuple.*
       |
       |class NamedTupleSelectable extends Selectable {
       |  type Fields <: AnyNamedTuple
       |  def selectDynamic(name: String): Any = ???
       |}
       |
       |val person = new NamedTupleSelectable {
       |  type Fields = (name: String, city: String)
       |}
       |
       |val person_name = person.na@@me
       |""".stripMargin,
    "name: String".hover
  )

  check(
    "named-tuples3",
    """|def hello = (path = ".", num = 5)
       |
       |def test =
       |  hello ++ (line = 1)
       |
       |@main def bla =
       |   val x: (path: String, num: Int, line: Int) = t@@est
       |""".stripMargin,
    "def test: (path : String, num : Int, line : Int)".hover
  )

  check(
    "named-tuples4",
    """|def hello = (path = ".", num = 5)
       |
       |def test =
       |  hel@@lo ++ (line = 1)
       |
       |@main def bla =
       |   val x: (path: String, num: Int, line: Int) = test
       |""".stripMargin,
    "def hello: (path : String, num : Int)".hover
  )

  check(
    "named-tuples5",
    """|def hello = (path = ".", num = 5)
       |
       |def test(x: (path: String, num: Int)) =
       |  x ++ (line = 1)
       |
       |@main def bla =
       |   val x: (path: String, num: Int, line: Int) = t@@est(hello)
       |""".stripMargin,
    "def test(x: (path : String, num : Int)): (path : String, num : Int, line : Int)".hover
  )

  check(
    "i7763",
    """|case class MyItem(name: String)
       |
       |def handle(item: MyItem) =
       |  item match {
       |    case MyItem(na@@me = n2) => println(n2)
       |  }
       |""".stripMargin,
    "val name: String".hover
  )

  check(
    "opaque-type-method-call",
    """|object History {
       |  opaque type Builder[A] = String
       |  def emptyBuilder: Builder[Unit] = ""
       |  def build(b: Builder[Unit]): Int = ???
       |}
       |object Main {
       |  <<History.bui@@ld(History.emptyBuilder)>>
       |}
       |""".stripMargin,
    """|def build(b: Builder[Unit]): Int
       |""".stripMargin.hover
  )
