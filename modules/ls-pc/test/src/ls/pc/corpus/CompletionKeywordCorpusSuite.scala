/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/completion/CompletionKeywordSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness; curated
 * given/using/derives keyword subset.
 */
package ls.pc.corpus

class CompletionKeywordCorpusSuite extends CorpusCompletionHarness:

  check(
    "given-def",
    """
      |package foo
      |
      |object A {
      |  def someMethod = {
      |    gi@@
      |  }
      |}
      |""".stripMargin,
    """given (commit: '')
      |""".stripMargin,
    includeCommitCharacter = true,
    topLines = Some(5)
  )

  check(
    "using",
    """|object A{
       |  def hello(u@@)
       |}""".stripMargin,
    """|using (commit: '')
       |""".stripMargin,
    includeCommitCharacter = true
  )

  check(
    "not-using",
    """|object A{
       |  def hello(a: String, u@@)
       |}""".stripMargin,
    ""
  )

  check(
    "extends-enum",
    """
      |package foo
      |
      |enum Foo(x: Int) ext@@
        """.stripMargin,
    """|extends
       |""".stripMargin
  )

  check(
    "derives-object",
    """
      |package foo
      |
      |object Foo der@@
      """.stripMargin,
    """|derives
       |""".stripMargin
  )

  check(
    "derives-with-constructor",
    """
      |package foo
      |
      |class Foo(x: Int) der@@
      """.stripMargin,
    """|derives
       |""".stripMargin
  )

  check(
    "derives-comma-extends",
    """
      |package foo
      |
      |trait Bar {}
      |trait Baz {}
      |
      |class Foo(x: Int) extends Bar, Baz der@@
        """.stripMargin,
    """|derives
       |""".stripMargin
  )

  check(
    "derives-extends",
    """
      |package foo
      |
      |trait Bar {}
      |class Foo(x: Int) extends Bar der@@
          """.stripMargin,
    """|derives
       |""".stripMargin
  )

  check(
    "derives-extends-type-param",
    """
      |package foo
      |
      |trait Bar[B] {}
      |class Foo(x: Int) extends Bar[Int] der@@
            """.stripMargin,
    """|derives
       |""".stripMargin
  )

  check(
    "derives-with-extends",
    """|package foo
       |
       |trait Bar {}
       |trait Baz {}
       |
       |class Foo(x: Int) extends Bar with Baz der@@
       |""".stripMargin,
    """|derives
       |""".stripMargin
  )

  check(
    "derives-with-constructor-extends",
    """|package foo
       |
       |trait Bar {}
       |class Baz(b: Int) {}
       |
       |class Foo(x: Int) extends Bar with Baz(1) der@@
       |""".stripMargin,
    """|derives
       |""".stripMargin
  )

  check(
    "no-derives",
    """
      |package foo
      |
      |object Main {
      |  def main = {
      |    foo.der@@
      |  }
      |}
      """.stripMargin,
    ""
  )
