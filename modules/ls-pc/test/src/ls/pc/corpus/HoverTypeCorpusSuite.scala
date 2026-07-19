/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/hover/HoverTypeSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness; curated subset.
 */
package ls.pc.corpus

class HoverTypeCorpusSuite extends CorpusHoverHarness:

  check(
    "union",
    """
      |import java.nio.file._
      |case class Foo(x: Int)
      |case class Bar[T](x: T)
      |object a {
      |  val name: Foo | Bar[Files] = Foo(1)
      |  <<na@@me>>
      |}
      |""".stripMargin,
    """|val name: Foo | Bar[Files]
       |""".stripMargin.hover
  )

  check(
    "intersection",
    """
      |import java.nio.file._
      |
      |trait Resettable:
      |  def reset(): Unit
      |
      |trait Growable[T]:
      |  def add(t: T): Unit
      |
      |def f(arg: Resettable & Growable[Files]) = {
      |  <<ar@@g.reset()>>
      |}
      |""".stripMargin,
    """|arg: Resettable & Growable[Files]
       |""".stripMargin.hover
  )

  check(
    "enums",
    """|
       |object SimpleEnum:
       |  enum Color:
       |   case <<Re@@d>>, Green, Blue
       |
       |""".stripMargin,
    """|case Red: Color
       |""".stripMargin.hover
  )

  check(
    "enums2",
    """|
       |object SimpleEnum:
       |  enum <<Col@@or>>:
       |   case Red, Green, Blue
       |
       |""".stripMargin,
    """|enum Color: SimpleEnum
       |""".stripMargin.hover
  )

  check(
    "enums-outermost",
    """|enum Color:
       |  case Red
       |  case <<Bl@@ue>>
       |  case Cyan
       |""".stripMargin,
    """|case Blue: Color
       |""".stripMargin.hover
  )

  check(
    "enums3",
    """|
       |object SimpleEnum:
       |  enum Color:
       |    case Red, Green, Blue
       |  val color = <<Col@@or>>.Red
       |
       |""".stripMargin,
    """|enum Color: SimpleEnum
       |""".stripMargin.hover
  )

  check(
    "enum-params",
    """|
       |object SimpleEnum:
       |  enum Color:
       |    case <<Gr@@een>> extends Color(2)
       |    case Red extends Color(1)
       |    case Blue extends Color(3)
       |
       |
       |""".stripMargin,
    """|case Green: Color
       |""".stripMargin.hover
  )

  check(
    "extension-methods",
    """|
       |object Foo:
       |    extension (s: String)
       |        def double = s + s
       |        def double2 = s + s
       |    end extension
       |    "".<<doub@@le2>>
       |end Foo
       |""".stripMargin,
    "extension (s: String) def double2: String".hover
  )

  check(
    "extension-methods-complex",
    """|class A
       |class B
       |class C
       |object Foo:
       |    extension [T](using A)(s: T)(using B)
       |        def double[G <: Int](using C)(times: G) = (s.toString + s.toString) * times
       |    end extension
       |    given A with {}
       |    given B with {}
       |    given C with {}
       |    "".<<doub@@le(1)>>
       |end Foo
       |""".stripMargin,
    "extension [T](using A)(s: T) def double(using B)[G <: Int](using C)(times: G): String".hover
  )

  check(
    "extension-methods-complex-binary",
    """|class A
       |class B
       |class C
       |
       |object Foo:
       |    extension [T](using A)(main: T)(using B)
       |      def %:[R](res: R)(using C): R = ???
       |    given A with {}
       |    given B with {}
       |    given C with {}
       |    val c = C()
       |    "" <<%@@:>> 11
       |end Foo
       |""".stripMargin,
    """|Int
       |extension [T](using A)(main: T) def %:[R](res: R)(using B)(using C): R""".stripMargin.hover
  )

  check(
    "using",
    """
      |object a {
      |  def apply[T](a: T)(using Int): T = ???
      |  implicit val ev = 1
      |  <<ap@@ply("test")>>
      |}
      |""".stripMargin,
    """|String
       |def apply[T](a: T)(using Int): T
       |""".stripMargin.hover
  )

  check(
    "toplevel-left",
    """|def foo = <<L@@eft>>("")
       |""".stripMargin,
    """|Left[String, Nothing]
       |def apply[A, B](value: A): Left[A, B]
       |""".stripMargin.hover
  )

  check(
    "selectable",
    """|trait Sel extends Selectable:
       |  def selectDynamic(name: String): Any = ???
       |  def applyDynamic(name: String)(args: Any*): Any = ???
       |val sel = (new Sel {}).asInstanceOf[Sel { def foo2: Int}]
       |val foo2 = sel.fo@@o2
       |""".stripMargin,
    """|def foo2: Int
       |""".stripMargin.hover
  )

  check(
    "selectable2",
    """|trait Sel extends Selectable:
       |  def selectDynamic(name: String): Any = ???
       |  def applyDynamic(name: String)(args: Any*): Any = ???
       |val sel = (new Sel {}).asInstanceOf[Sel { def bar2(x: Int): Int }]
       |val bar2 = sel.ba@@r2(3)
       |""".stripMargin,
    """|def bar2(x: Int): Int
       |""".stripMargin.hover
  )

  check(
    "structural-types",
    """|
       |import reflect.Selectable.reflectiveSelectable
       |
       |object StructuralTypes:
       |   type User = {
       |   def name: String
       |   def age: Int
       |   }
       |
       |   val user = null.asInstanceOf[User]
       |   user.name
       |   user.ag@@e
       |
       |   val V: Object {
       |   def scalameta: String
       |   } = new:
       |   def scalameta = "4.0"
       |   V.scalameta
       |end StructuralTypes
       |""".stripMargin,
    """|def age: Int
       |""".stripMargin.hover
  )

  check(
    "infix-extension",
    """|class MyIntOut(val value: Int)
       |object MyIntOut:
       |  extension (i: MyIntOut) def uneven = i.value % 2 == 1
       |
       |object Test:
       |  val a = MyIntOut(1).un@@even
       |""".stripMargin,
    """|extension (i: MyIntOut) def uneven: Boolean
       |""".stripMargin.hover
  )

  check(
    "recursive-enum-without-type",
    """class Wrapper(n: Int):
      |  extension (x: Int)
      |    def + (y: Int) = new Wrap@@per(x) + y
      |""".stripMargin,
    """```scala
      |def this(n: Int): Wrapper
      |```
      |""".stripMargin
  )

  check(
    "recursive-enum-without-type-1",
    """class Wrapper(n: Int):
      |  def add(x: Int): Wrapper = ???
      |  extension (x: Int)
      |    def + (y: Int) = Wrap@@per(x).add(5)
      |""".stripMargin,
    """```scala
      |def this(n: Int): Wrapper
      |```
      |""".stripMargin
  )
