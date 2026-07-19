/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/completion/CompletionExtensionSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness. Upstream's
 * `TestingWorkspaceSearch` indexes the case source itself and answers the
 * PC's `SymbolSearch.searchMethods` from it; here each discovery case seeds
 * the equivalent hits (hand-derived SemanticDB symbols of its own extension
 * methods / implicit-class members) into the corpus resolver's workspace
 * method registry, which serves them through the `PcDefinitionResolver.
 * searchMethods` seam.
 */
package ls.pc.corpus

import ls.pc.corpus.CorpusPc.WorkspaceMethod
import org.eclipse.lsp4j.SymbolKind

class CompletionExtensionCorpusSuite extends CorpusCompletionHarness:

  check(
    "simple",
    """|package example
       |
       |object enrichments:
       |  extension (num: Int)
       |    def incr: Int = num + 1
       |
       |def main = 100.inc@@
       |""".stripMargin,
    """|incr: Int (extension)
       |asInstanceOf[X0]: X0
       |isInstanceOf[X0]: Boolean
       |""".stripMargin,
    workspaceMethods = Seq(
      WorkspaceMethod("incr", "example/enrichments.incr().")
    )
  )

  check(
    "simple-old-syntax",
    """package example
      |
      |object Test:
      |  implicit class TestOps(a: Int):
      |    def testOps(b: Int): String = ???
      |
      |def main = 100.test@@
      |""".stripMargin,
    """testOps(b: Int): String (implicit)
      |""".stripMargin,
    topLines = Some(1),
    workspaceMethods = Seq(
      WorkspaceMethod("TestOps", "example/Test.TestOps#", SymbolKind.Class.getValue),
      WorkspaceMethod("testOps", "example/Test.TestOps#testOps().")
    )
  )

  check(
    "simple2",
    """|package example
       |
       |object enrichments:
       |  extension (num: Int)
       |    def incr: Int = num + 1
       |
       |def main = 100.i@@
       |""".stripMargin,
    """|incr: Int (extension)
       |""".stripMargin,
    filter = _.contains("(extension)"),
    workspaceMethods = Seq(
      WorkspaceMethod("incr", "example/enrichments.incr().")
    )
  )

  check(
    "filter-by-type",
    """|package example
       |
       |object enrichments:
       |  extension (num: Int)
       |    def incr: Int = num + 1
       |  extension (str: String)
       |    def identity: String = str
       |
       |def main = "foo".i@@
       |""".stripMargin,
    """|identity: String (extension)
       |""".stripMargin, // incr won't be available
    filter = _.contains("(extension)"),
    workspaceMethods = Seq(
      WorkspaceMethod("incr", "example/enrichments.incr()."),
      WorkspaceMethod("identity", "example/enrichments.identity().")
    )
  )

  check(
    "filter-by-type-subtype",
    """|package example
       |
       |class A
       |class B extends A
       |
       |object enrichments:
       |  extension (a: A)
       |    def doSomething: A = a
       |
       |def main = (new B).do@@
       |""".stripMargin,
    """|doSomething: A (extension)
       |""".stripMargin,
    filter = _.contains("(extension)"),
    workspaceMethods = Seq(
      WorkspaceMethod("doSomething", "example/enrichments.doSomething().")
    )
  )

  checkEdit(
    "simple-edit-suffix",
    """|package example
       |
       |object enrichments:
       |  extension (num: Int)
       |    def plus(other: Int): Int = num + other
       |
       |def main = 100.pl@@
       |""".stripMargin,
    """|package example
       |
       |import example.enrichments.plus
       |
       |object enrichments:
       |  extension (num: Int)
       |    def plus(other: Int): Int = num + other
       |
       |def main = 100.plus($0)
       |""".stripMargin,
    workspaceMethods = Seq(
      WorkspaceMethod("plus", "example/enrichments.plus().")
    )
  )

  check(
    "directly-in-pkg1",
    """|
       |package example:
       |  extension (num: Int)
       |    def incr: Int = num + 1
       |
       |package example2:
       |  def main = 100.inc@@
       |""".stripMargin,
    """|incr: Int (extension)
       |asInstanceOf[X0]: X0
       |isInstanceOf[X0]: Boolean
       |""".stripMargin,
    workspaceMethods = Seq(
      WorkspaceMethod("incr", "example/A$package.incr().")
    )
  )

  check(
    "nested-pkg",
    """|package a:  // some comment
       |  package c:
       |    extension (num: Int)
       |        def increment2 = num + 2
       |  extension (num: Int)
       |    def increment = num + 1
       |
       |
       |package b:
       |  def main: Unit = 123.incre@@
       |""".stripMargin,
    """|increment: Int (extension)
       |increment2: Int (extension)
       |""".stripMargin,
    workspaceMethods = Seq(
      WorkspaceMethod("increment2", "a/c/A$package.increment2()."),
      WorkspaceMethod("increment", "a/A$package.increment().")
    )
  )

  checkEdit(
    "name-conflict",
    """
      |package example
      |
      |import example.enrichments.*
      |
      |object enrichments:
      |  extension (num: Int)
      |    def plus(other: Int): Int = num + other
      |
      |def main = {
      |  val plus = 100.plus(19)
      |  val y = 19.pl@@
      |}
      |""".stripMargin,
    """
      |package example
      |
      |import example.enrichments.*
      |
      |object enrichments:
      |  extension (num: Int)
      |    def plus(other: Int): Int = num + other
      |
      |def main = {
      |  val plus = 100.plus(19)
      |  val y = 19.plus($0)
      |}
      |""".stripMargin,
    workspaceMethods = Seq(
      WorkspaceMethod("plus", "example/enrichments.plus().")
    )
  )

  check(
    "implicit-val-var",
    """|package example
       |
       |object Test:
       |  implicit class TestOps(val testArg: Int):
       |    var testVar: Int = 42
       |    val testVal: Int = 42
       |    def testOps(b: Int): String = ???
       |
       |def main = 100.test@@
       |""".stripMargin,
    """|testArg: Int (implicit)
       |testVal: Int (implicit)
       |testVar: Int (implicit)
       |testOps(b: Int): String (implicit)
       |""".stripMargin,
    topLines = Some(4),
    workspaceMethods = Seq(
      WorkspaceMethod("TestOps", "example/Test.TestOps#", SymbolKind.Class.getValue),
      WorkspaceMethod("testArg", "example/Test.TestOps#testArg.", SymbolKind.Field.getValue),
      WorkspaceMethod("testVar", "example/Test.TestOps#testVar.", SymbolKind.Variable.getValue),
      WorkspaceMethod("testVal", "example/Test.TestOps#testVal.", SymbolKind.Field.getValue),
      WorkspaceMethod("testOps", "example/Test.TestOps#testOps().")
    )
  )

  check(
    "extension-for-case-class",
    """|case class Bar():
       |  def baz(): Unit = ???
       |
       |object Bar:
       |  extension (f: Bar)
       |    def qux: Unit = ???
       |
       |object Main:
       |  val _ = Bar().@@
       |""".stripMargin,
    """|baz(): Unit
       |copy(): Bar
       |qux: Unit
       |asInstanceOf[X0]: X0
       |canEqual(that: Any): Boolean
       |equals(x$0: Any): Boolean
       |getClass[X0 >: Bar](): Class[? <: X0]
       |hashCode(): Int
       |isInstanceOf[X0]: Boolean
       |productArity: Int
       |productElement(n: Int): Any
       |productElementName(n: Int): String
       |productElementNames: Iterator[String]
       |productIterator: Iterator[Any]
       |productPrefix: String
       |synchronized[X0](x$0: X0): X0
       |toString(): String
       |->[B](y: B): (Bar, B)
       |ensuring(cond: Boolean): Bar
       |ensuring(cond: Bar => Boolean): Bar
       |ensuring(cond: Boolean, msg: => Any): Bar
       |ensuring(cond: Bar => Boolean, msg: => Any): Bar
       |nn: `?1`.type
       |runtimeChecked: `?2`.type
       |formatted(fmtstr: String): String
       |→[B](y: B): (Bar, B)
       | """.stripMargin
  )
