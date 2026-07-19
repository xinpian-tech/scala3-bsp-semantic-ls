/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/completion/CompletionExtensionSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness.
 *
 * Curation note: most of the upstream suite exercises DISCOVERY of
 * out-of-scope extension methods, which the PC delegates to
 * `SymbolSearch.searchMethods` (a workspace/classpath index seam). Our facade
 * deliberately no-ops that seam (`IndexBackedSymbolSearch` implements only
 * `definition`), so those cases are not portable and were dropped: simple,
 * simple-old-syntax, simple2, filter-by-type, filter-by-type-subtype,
 * simple-edit-suffix, directly-in-pkg1, nested-pkg, implicit-val-var, and
 * name-conflict (even an imported extension resolves through the seam). The
 * retained case resolves the extension through the companion object's
 * implicit scope, which the compiler completes natively; lexically-scoped
 * extension completion is covered by `extension-definition-type-variable-
 * inference` in CompletionCorpusSuite.
 */
package ls.pc.corpus

class CompletionExtensionCorpusSuite extends CorpusCompletionHarness:

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
