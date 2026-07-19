/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/signaturehelp/SignatureHelpSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness; curated subset
 * favoring using/context/opaque/named parameters.
 */
package ls.pc.corpus

class SignatureHelpCorpusSuite extends CorpusSignatureHelpHarness:

  check(
    "method",
    """
      |object a {
      |  assert(true, ms@@)
      |}
    """.stripMargin,
    """|assert(assertion: Boolean): Unit
       |assert(assertion: Boolean, message: => Any): Unit
       |                           ^^^^^^^^^^^^^^^
       |""".stripMargin
  )

  check(
    "empty",
    """
      |object a {
      |  assert(@@)
      |}
    """.stripMargin,
    """|assert(assertion: Boolean): Unit
       |       ^^^^^^^^^^^^^^^^^^
       |assert(assertion: Boolean, message: => Any): Unit
       |""".stripMargin
  )

  check(
    "erroneous",
    """
      |object a {
      |  Option(1).fold("")(_ => a@@)
      |}
    """.stripMargin,
    """|fold[B](ifEmpty: => B)(f: Int => B): B
       |                       ^^^^^^^^^^^
       |""".stripMargin
  )

  check(
    "canbuildfrom2",
    """
      |object a {
      |  List(1).map(@@)
      |}
    """.stripMargin,
    """|map[B](f: Int => B): List[B]
       |       ^^^^^^^^^^^
       |""".stripMargin
  )

  check(
    "named",
    """
      |case class User(name: String = "John", age: Int = 42)
      |object A {
      |  User(age = 1, @@)
      |}
    """.stripMargin,
    """|apply([age: Int], [name: String]): User
       |                  ^^^^^^^^^^^^^^
       |""".stripMargin
  )

  check(
    "named1",
    """
      |case class User(name: String = "John", age: Int = 42)
      |object A {
      |  User(name = "", @@)
      |}
    """.stripMargin,
    """|apply(name: String, age: Int): User
       |                    ^^^^^^^^
       |""".stripMargin
  )

  check(
    "named2",
    """
      |object A {
      |  def user(name: String, age: Int) = age
      |  user(na@@me = "", age = 42)
      |}
    """.stripMargin,
    """|user(name: String, age: Int): Int
       |     ^^^^^^^^^^^^
       |""".stripMargin
  )

  check(
    "named3",
    """
      |object A {
      |  def user(name: String, age: Int): Int = age
      |  def user(name: String, age: Int, street: Int): Int = age
      |  def x = user(str@@eet = 42, name = "", age = 2)
      |}
    """.stripMargin,
    """|user([street: Int], [name: String], [age: Int]): Int
       |     ^^^^^^^^^^^^^
       |user(name: String, age: Int): Int
       |""".stripMargin
  )

  check(
    "named4",
    """
      |object A {
      |  identity(x = @@)
      |}
    """.stripMargin,
    """|identity[A](x: A): A
       |            ^^^^
       |""".stripMargin
  )

  check(
    "curried-help-works-in-select",
    """|object Main:
       |  def test(xxx: Int, yyy: Int)(zzz: Int): Int = ???
       |  test(yyy = 5, xxx = 7)(@@)
       |""".stripMargin,
    """|test([yyy: Int], [xxx: Int])(zzz: Int): Int
       |                             ^^^^^^^^
       |""".stripMargin
  )

  check(
    "show-methods-returning-tuples",
    """|object Main:
       |  def test(): (Int, Int) = ???
       |  test(@@)
       |""".stripMargin,
    "test(): (Int, Int)"
  )

  check(
    "show-methods-returning-tuples-2",
    """|object Main:
       |  def test(x: Int): (Int, Int) = ???
       |  test(@@)
       |""".stripMargin,
    """|test(x: Int): (Int, Int)
       |     ^^^^^^
       |""".stripMargin
  )

  check(
    "implicit-param",
    """|object M:
       |  trait Context
       |  def test(x: Int)(using ctx: Context): Int = ???
       |  test(@@)
       |""".stripMargin,
    """|test(x: Int)(using ctx: Context): Int
       |     ^^^^^^
       |""".stripMargin
  )

  check(
    "context-param",
    """|object M:
       |  def test(x: Int, y: Int = 7)(z: Int ?=> Int): Int = ???
       |  test(@@)
       |""".stripMargin,
    """|test(x: Int, y: Int)(z: (Int) ?=> Int): Int
       |     ^^^^^^
       |""".stripMargin
  )

  check(
    "empty-implicit-params",
    """|object M:
       |  def test(x: Int)(using String): Int = ???
       |  test(1)(@@)
       |""".stripMargin,
    """|test(x: Int)(using String): Int
       |                   ^^^^^^
       |""".stripMargin
  )

  check(
    "multiple-implicits-1",
    """|object M:
       |  def a(using Int)(using String): Int = ???
       |  a(@@)
       |""".stripMargin,
    """|a(using Int)(using String): Int
       |        ^^^
       |""".stripMargin
  )

  check(
    "multiple-implicits-2",
    """|object M:
       |  def a(using Int)(using String): Int = ???
       |  a(using 5)(@@)
       |""".stripMargin,
    """|a(using Int)(using String): Int
       |                   ^^^^^^
       |""".stripMargin
  )

  check(
    "multiple-implicits-3",
    """|object M:
       |  def a(using Int)(using String)(x: Int): Int = ???
       |  a(@@)
       |""".stripMargin,
    """|a(using Int)(using String)(x: Int): Int
       |        ^^^
       |""".stripMargin
  )

  check(
    "multiple-implicits-4",
    """|object M:
       |  def a(using Int)(using String)(x: Int): Int = ???
       |  a(using 5)(@@)
       |""".stripMargin,
    """|a(using Int)(using String)(x: Int): Int
       |                   ^^^^^^
       |""".stripMargin
  )

  check(
    "multiple-implicits-error-2",
    """|object M:
       |  def a(using Int)(using String)(x: Int): Int = ???
       |  a(5)(@@)
       |""".stripMargin,
    """|a(using Int)(using String)(x: Int): Int
       |                   ^^^^^^
       |""".stripMargin
  )

  check(
    "opaque-type-parameter",
    """|object History {
       |  opaque type Builder[A] = String
       |  def build(b: Builder[Unit]): Int = ???
       |}
       |object Main {
       |  History.build(@@)
       |}
       |""".stripMargin,
    """|build(b: Builder[Unit]): Int
       |      ^^^^^^^^^^^^^^^^
       |""".stripMargin
  )
