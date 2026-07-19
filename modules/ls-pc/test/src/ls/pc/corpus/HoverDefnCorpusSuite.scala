/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/hover/HoverDefnSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness; curated subset.
 */
package ls.pc.corpus

class HoverDefnCorpusSuite extends CorpusHoverHarness:

  check(
    "context-bound",
    """object a {
      |  <<def @@empty[T:Ordering] = Option.empty[T]>>
      |}
      |""".stripMargin,
    "def empty[T: Ordering]: Option[T]".hover
  )

  check(
    "implicit-param",
    """class a {
      |  def method(implicit <<@@x: Int>>) = this()
      |}
      |""".stripMargin,
    """|```scala
       |implicit x: Int
       |```
       |""".stripMargin
  )

  check(
    "implicit-param2",
    """class a {
      |  def method(implicit y: Int, <<@@x: Int>>) = this()
      |}
      |""".stripMargin,
    """|```scala
       |implicit x: Int
       |```
       |""".stripMargin
  )

  check(
    "val-int-literal-union",
    """object a {
      |  <<val @@x : 1 | 2 = 1>>
      |}
      |""".stripMargin,
    "val x: 1 | 2".hover
  )

  check(
    "dealias-appliedtype-params",
    """|trait Base {
       |  type T
       |  def f(t: T): Option[T] = Some(t)
       |}
       |
       |object Derived extends Base {
       |  override type T = Int
       |}
       |object O {
       |  <<val @@x = Derived.f(42)>>
       |}
       |""".stripMargin,
    """|```scala
       |val x: Option[Int]
       |```
       |""".stripMargin.hover
  )

  check(
    "opaque-type-method",
    """|object History {
       |  opaque type Builder[A] = String
       |  def <<bui@@ld>>(b: Builder[Unit]): Int = ???
       |}
       |""".stripMargin,
    """|def build(b: Builder[Unit]): Int
       |""".stripMargin.hover
  )
