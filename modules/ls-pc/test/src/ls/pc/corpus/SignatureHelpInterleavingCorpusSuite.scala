/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/signaturehelp/SignatureHelpInterleavingSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness; curated subset
 * (interleaved type/term parameter clauses are Scala-3-only syntax).
 */
package ls.pc.corpus

class SignatureHelpInterleavingCorpusSuite extends CorpusSignatureHelpHarness:

  check(
    "proper-position-1",
    """
      |object Test:
      |  def pair[A](a: A)[B](b: B): (A, B) = (a, b)
      |  pair[@@Int](1)[String]("1")
    """.stripMargin,
    """
      |pair[A](a: A)[B](b: B): (A, B)
      |     ^
      |""".stripMargin
  )

  check(
    "proper-position-2",
    """
      |object Test:
      |  def pair[A](a: A)[B](b: B): (A, B) = (a, b)
      |  pair[Int](@@1)[String]("1")
    """.stripMargin,
    """
      |pair[A](a: A)[B](b: B): (A, B)
      |        ^^^^
      |""".stripMargin
  )

  check(
    "proper-position-3",
    """
      |object Test:
      |  def pair[A](a: A)[B](b: B): (A, B) = (a, b)
      |  pair[Int](1)[@@String]("1")
    """.stripMargin,
    """
      |pair[A](a: A)[B](b: B): (A, B)
      |              ^
      |""".stripMargin
  )

  check(
    "proper-position-4",
    """
      |object Test:
      |  def pair[A](a: A)[B](b: B): (A, B) = (a, b)
      |  pair[Int](1)[String](@@"1")
    """.stripMargin,
    """
      |pair[A](a: A)[B](b: B): (A, B)
      |                 ^^^^
      |""".stripMargin
  )

  check(
    "not-fully-applied-1",
    """
      |object Test:
      |  def pair[A](a: A)[B](b: B): (A, B) = (a, b)
      |  pair[@@Int]
    """.stripMargin,
    """
      |pair[A](a: A)[B](b: B): (A, B)
      |     ^
      |""".stripMargin
  )

  check(
    "not-fully-applied-2",
    """
      |object Test:
      |  def pair[A](a: A)[B](b: B): (A, B) = (a, b)
      |  pair[Int](@@1)
    """.stripMargin,
    """
      |pair[A](a: A)[B](b: B): (A, B)
      |        ^^^^
      |""".stripMargin
  )

  check(
    "not-fully-applied-3",
    """
      |object Test:
      |  def pair[A](a: A)[B](b: B): (A, B) = (a, b)
      |  pair[Int](1)[@@String]
    """.stripMargin,
    """
      |pair[A](a: A)[B](b: B): (A, B)
      |              ^
      |""".stripMargin
  )

  check(
    "error",
    """
      |object Test:
      |  def pair[A](a: A)[B](b: B): (A, B) = (a, b)
      |  pair[Int][@@String]
    """.stripMargin,
    """|apply(v1: Int): Any => (Int, Any)
       |      ^^^^^^^
       |""".stripMargin
  )

  check(
    "inferred-type-param-1",
    """
      |object Test:
      |  def pair[A](a: A)[B](b: B): (A, B) = (a, b)
      |  pair(1@@)
    """.stripMargin,
    """
      |pair[A](a: A)[B](b: B): (A, B)
      |        ^^^^
      |""".stripMargin
  )
