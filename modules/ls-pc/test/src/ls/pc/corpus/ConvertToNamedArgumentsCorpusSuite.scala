/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/edit/ConvertToNamedArgumentsSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness (the PcFacade
 * codeAction(ConvertToNamedArguments) carrier; LSP positions instead of offsets).
 * The FULL 3.8.4 suite (6 cases - the task's ~8 exceeds the upstream suite): every edit case plus the java-object refusal (DisplayableException as data).
 */
package ls.pc.corpus

import scala.meta.internal.pc.CodeActionErrorMessages

class ConvertToNamedArgumentsCorpusSuite extends CorpusConvertToNamedArgumentsHarness:

  checkEdit(
    "scala-std-lib",
    """|object A{
       |  val a = <<scala.math.max(1, 2)>>
       |}""".stripMargin,
    List(0, 1),
    """|object A{
       |  val a = scala.math.max(x = 1, y = 2)
       |}""".stripMargin
  )

  checkEdit(
    "backticked-name",
    """|object A{
       |  final case class Foo(`type`: Int, arg: String)
       |  val a = <<Foo(1, "a")>>
       |}""".stripMargin,
    List(0, 1),
    """|object A{
       |  final case class Foo(`type`: Int, arg: String)
       |  val a = Foo(`type` = 1, arg = "a")
       |}""".stripMargin
  )

  checkEdit(
    "backticked-name-method",
    """|object A{
       |  def foo(`type`: Int, arg: String) = "a"
       |  val a = <<foo(1, "a")>>
       |}""".stripMargin,
    List(0, 1),
    """|object A{
       |  def foo(`type`: Int, arg: String) = "a"
       |  val a = foo(`type` = 1, arg = "a")
       |}""".stripMargin
  )

  checkEdit(
    "new-apply",
    """|object Something {
       |  class Foo(param1: Int, param2: Int)
       |  val a = <<new Foo(1, param2 = 2)>>
       |}""".stripMargin,
    List(0),
    """|object Something {
       |  class Foo(param1: Int, param2: Int)
       |  val a = new Foo(param1 = 1, param2 = 2)
       |}""".stripMargin
  )

  checkEdit(
    "new-apply-multiple",
    """|object Something {
       |  class Foo(param1: Int, param2: Int)(param3: Int)
       |  val a = <<new Foo(1, param2 = 2)(3)>>
       |}""".stripMargin,
    List(0, 2),
    """|object Something {
       |  class Foo(param1: Int, param2: Int)(param3: Int)
       |  val a = new Foo(param1 = 1, param2 = 2)(param3 = 3)
       |}""".stripMargin
  )

  checkRefusal(
    "java-object",
    """|object A{
       |  val a = <<new java.util.Vector(3)>>
       |}
       |""".stripMargin,
    List(0, 1),
    CodeActionErrorMessages.ConvertToNamedArguments.IsJavaObject
  )
