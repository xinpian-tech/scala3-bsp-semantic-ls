/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/completion/SingletonCompletionsSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness; curated
 * singleton/literal/union-type subset.
 */
package ls.pc.corpus

class SingletonCompletionsCorpusSuite extends CorpusCompletionHarness:

  check(
    "literal",
    """|val k: 1 = 1@@
       |""".stripMargin,
    "1: 1",
    topLines = Some(1)
  )

  check(
    "string",
    """|val k: "aaa" = "@@"
       |""".stripMargin,
    """|"aaa": "aaa"
       |""".stripMargin
  )

  check(
    "union",
    """|val k: "aaa" | "bbb" = "@@"
       |""".stripMargin,
    """|"aaa": "aaa" | "bbb"
       |"bbb": "aaa" | "bbb"
       |""".stripMargin
  )

  check(
    "type-alias-union",
    """|type Color = "red" | "green" | "blue"
       |val c: Color = "r@@"
       |""".stripMargin,
    """|"red": Color
       |""".stripMargin
  )

  check(
    "and-type",
    """|type Color = "red" | "green" | "blue" | "black"
       |type FordColor = Color & "black"
       |val i: FordColor = "@@"
       |""".stripMargin,
    """|"black": FordColor
       |""".stripMargin
  )

  check(
    "match-case-result",
    """|val h: "foo" =
       |  1 match
       |    case _ => "@@"
       |""".stripMargin,
    """|"foo": "foo"
       |""".stripMargin
  )

  check(
    "match-case",
    """|def h(foo: "foo") =
       |  foo match
       |    case "@@" =>
       |""".stripMargin,
    """|"foo": "foo"
       |""".stripMargin
  )

  check(
    "named-args",
    """|def h(foo: "foo") = ???
       |def k = h(foo = "@@")
       |""".stripMargin,
    """|"foo": "foo"
       |""".stripMargin
  )
