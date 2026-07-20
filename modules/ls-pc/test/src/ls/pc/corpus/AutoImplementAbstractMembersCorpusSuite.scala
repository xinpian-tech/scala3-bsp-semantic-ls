/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/edit/AutoImplementAbstractMembersSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness (the PcFacade
 * codeAction(ImplementAbstractMembers) carrier; LSP positions instead of offsets).
 * Curated 12 of 46 cases favoring Scala 3 syntax (braceless templates, extension methods, givens, end markers) plus the classic class/object/anonymous-creation and overload shapes; the remainder re-walk the same indentation matrix.
 */
package ls.pc.corpus

class AutoImplementAbstractMembersCorpusSuite extends CorpusCodeActionEditHarness(ls.pc.PcCodeActionId.ImplementAbstractMembers):

  checkEdit(
    "classdef",
    """|package a
       |
       |object A {
       |  trait Base {
       |    def foo(x: Int): Int
       |    def bar(x: String): String
       |  }
       |  class <<Concrete>> extends Base {
       |  }
       |}
       |""".stripMargin,
    """|package a
       |
       |object A {
       |  trait Base {
       |    def foo(x: Int): Int
       |    def bar(x: String): String
       |  }
       |  class Concrete extends Base {
       |
       |    override def foo(x: Int): Int = ???
       |
       |    override def bar(x: String): String = ???
       |
       |  }
       |}
       |""".stripMargin
  )

  checkEdit(
    "objectdef",
    """|package a
       |
       |object A {
       |  trait Base {
       |    def foo(x: Int): Int
       |  }
       |  object <<Concrete>> extends Base {
       |  }
       |}
       |""".stripMargin,
    """|package a
       |
       |object A {
       |  trait Base {
       |    def foo(x: Int): Int
       |  }
       |  object Concrete extends Base {
       |
       |    override def foo(x: Int): Int = ???
       |
       |  }
       |}
       |""".stripMargin
  )

  checkEdit(
    "overload",
    """|package a
       |
       |object A {
       |  trait Base {
       |    def foo(x: Int): Int
       |    def bar(x: String): String
       |  }
       |  class <<Concrete>> extends Base {
       |    override def foo(x: Int): Int = x
       |  }
       |}
       |""".stripMargin,
    """|package a
       |
       |object A {
       |  trait Base {
       |    def foo(x: Int): Int
       |    def bar(x: String): String
       |  }
       |  class Concrete extends Base {
       |
       |    override def bar(x: String): String = ???
       |
       |    override def foo(x: Int): Int = x
       |  }
       |}
       |""".stripMargin
  )

  checkEdit(
    "braces",
    """|package a
       |
       |object A {
       |  trait Base {
       |    def foo(x: Int): Int
       |    def bar(x: String): String
       |  }
       |  class <<Concrete>> extends Base {}
       |}
       |""".stripMargin,
    """|package a
       |
       |object A {
       |  trait Base {
       |    def foo(x: Int): Int
       |    def bar(x: String): String
       |  }
       |  class Concrete extends Base {
       |
       |    override def foo(x: Int): Int = ???
       |
       |    override def bar(x: String): String = ???
       |
       |  }
       |}
       |""".stripMargin
  )

  checkEdit(
    "object-creation",
    """
      |object Main {
      |  new <<Iterable>>[Int] {}
      |}
    """.stripMargin,
    """
      |object Main {
      |  new Iterable[Int] {
      |
      |    override def iterator: Iterator[Int] = ???
      |
      |  }
      |}
      |""".stripMargin
  )

  checkEdit(
    "val",
    """|abstract class Abstract {
       |  val baz: String
       |}
       |class <<Main>> extends Abstract {
       |}
       |""".stripMargin,
    """|abstract class Abstract {
       |  val baz: String
       |}
       |class Main extends Abstract {
       |
       |  override val baz: String = ???
       |
       |}
       |""".stripMargin
  )

  checkEdit(
    "braceless-basic",
    """|package a
       |
       |object A {
       |  trait Base:
       |    def foo(x: Int): Int
       |    def bar(x: String): String
       |
       |  class <<Concrete>> extends Base:
       |    def foo(x: Int): Int = x
       |
       |}
       |""".stripMargin,
    """|package a
       |
       |object A {
       |  trait Base:
       |    def foo(x: Int): Int
       |    def bar(x: String): String
       |
       |  class Concrete extends Base:
       |
       |    override def bar(x: String): String = ???
       |
       |    def foo(x: Int): Int = x
       |
       |}
       |""".stripMargin
  )

  checkEdit(
    "extension-methods",
    """|package a
       |
       |trait Base:
       |  extension (x: Int)
       |    def foo: Int
       |    def bar: String
       |
       |class <<Concrete>> extends Base
       |""".stripMargin,
    """|package a
       |
       |trait Base:
       |  extension (x: Int)
       |    def foo: Int
       |    def bar: String
       |
       |class Concrete extends Base {
       |
       |  extension (x: Int) override def foo: Int = ???
       |
       |  extension (x: Int) override def bar: String = ???
       |
       |}
       |""".stripMargin
  )

  checkEdit(
    "given-object-creation",
    """|package given
       |
       |trait Foo:
       |  def foo(x: Int): Int
       |  def bar(x: String): String
       |
       |given <<Foo>> with
       |  def foo(x: Int): Int = x
       |""".stripMargin,
    """|package given
       |
       |trait Foo:
       |  def foo(x: Int): Int
       |  def bar(x: String): String
       |
       |given Foo with
       |
       |  override def bar(x: String): String = ???
       |
       |  def foo(x: Int): Int = x
       |""".stripMargin
  )

  checkEdit(
    "end-marker",
    """|package a
       |
       |object A {
       |  trait Base:
       |    def foo(x: Int): Int
       |    def bar(x: String): String
       |
       |  class <<Concrete>> extends Base:
       |
       |  end Concrete
       |
       |}
       |""".stripMargin,
    """|package a
       |
       |object A {
       |  trait Base:
       |    def foo(x: Int): Int
       |    def bar(x: String): String
       |
       |  class Concrete extends Base:
       |
       |    override def foo(x: Int): Int = ???
       |
       |    override def bar(x: String): String = ???
       |
       |
       |  end Concrete
       |
       |}
       |""".stripMargin
  )

  checkEdit(
    "type-alias",
    """|package example
       |
       |trait NodeDb {
       |  type N
       |  def method(node: N): String
       |}
       |
       |class <<InMemoryNodeDb>> extends NodeDb
       |""".stripMargin,
    """|package example
       |
       |trait NodeDb {
       |  type N
       |  def method(node: N): String
       |}
       |
       |class InMemoryNodeDb extends NodeDb {
       |
       |  override def method(node: N): String = ???
       |
       |}
       |""".stripMargin
  )

  checkEdit(
    "case-class",
    """|package example
       |
       |sealed trait Demo {
       |  def implementMe: Int
       |}
       |
       |case class <<ADemo>>(value: Int) extends Demo
       |""".stripMargin,
    """|package example
       |
       |sealed trait Demo {
       |  def implementMe: Int
       |}
       |
       |case class ADemo(value: Int) extends Demo {
       |
       |  override def implementMe: Int = ???
       |
       |}
       |""".stripMargin
  )
