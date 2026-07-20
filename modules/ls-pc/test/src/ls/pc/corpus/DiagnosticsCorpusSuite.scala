/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/DiagnosticProviderSuite.scala
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/ExplainDiagnosticProviderSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness (the facade
 * `diagnostics` path — the island's `pc_diagnostics` op — instead of a raw
 * `pc.didChange`); the `-explain` variant runs under a second registered
 * target carrying the option instead of a suite-level `options` override.
 */
package ls.pc.corpus

import scala.jdk.CollectionConverters.*

import org.eclipse.lsp4j.{CodeAction, DiagnosticSeverity}

class DiagnosticsCorpusSuite extends CorpusDiagnosticsHarness:

  check(
    "error",
    """|object M:
       |  Int.maaxValue
       |""".stripMargin,
    List(
      TestDiagnostic(
        12,
        25,
        "value maaxValue is not a member of object Int - did you mean Int.MaxValue?",
        DiagnosticSeverity.Error
      )
    )
  )

  check(
    "warning",
    """|object M:
       |  1 + 1
       |""".stripMargin,
    List(
      TestDiagnostic(
        12,
        17,
        "A pure expression does nothing in statement position",
        DiagnosticSeverity.Warning
      )
    )
  )

  check(
    "mixed",
    """object M:
      |  Int.maaxValue
      |  1 + 1
      |""".stripMargin,
    List(
      TestDiagnostic(
        12,
        25,
        "value maaxValue is not a member of object Int - did you mean Int.MaxValue?",
        DiagnosticSeverity.Error
      ),
      TestDiagnostic(
        28,
        33,
        "A pure expression does nothing in statement position",
        DiagnosticSeverity.Warning
      )
    )
  )

  check(
    "codeAction",
    """object M:
      |  private private class Test
      |""".stripMargin,
    List(
      TestDiagnostic(
        20,
        27,
        "Repeated modifier private",
        DiagnosticSeverity.Error
      )
    ),
    diags =>
      val action = diags.head
        .getData()
        .asInstanceOf[java.util.List[CodeAction]]
        .asScala
        .head
      assertNoDiff(
        action.getTitle(),
        "Remove repeated modifier: \"private\""
      )
      assertEquals(
        action.getEdit().getChanges().size(),
        1,
        "There should be one change"
      )
  )

/** The `-explain` variant: the same facade, a second target registered with
  * `scalacOptions = Vector("-explain")` — the diagnostic message carries the
  * full explanation block.
  */
class ExplainDiagnosticsCorpusSuite extends CorpusDiagnosticsHarness:

  override def diagnosticsTargetId: String = CorpusPc.explainTargetId

  check(
    "error1",
    """|object C:
       |  def m(x: Int) = 1
       |  object T extends K:
       |    val x = m(1) // error
       |class K:
       |  def m(i: Int) = 2
       |""".stripMargin,
    List(
      TestDiagnostic(
        64,
        65,
        """|Reference to m is ambiguous.
           |It is both defined in object C
           |and inherited subsequently in object T
           |
           |# Explanation (enabled by `-explain`)
           |
           |The identifier m is ambiguous because a name binding of lower precedence
           |in an inner scope cannot shadow a binding with higher precedence in
           |an outer scope.
           |
           |The precedence of the different kinds of name bindings, from highest to lowest, is:
           | - Definitions in an enclosing scope
           | - Inherited definitions and top-level definitions in packages
           | - Names introduced by import of a specific name
           | - Names introduced by wildcard import
           | - Definitions from packages in other files
           |Note:
           | - As a rule, definitions take precedence over imports.
           | - Definitions in an enclosing scope take precedence over inherited definitions,
           |   which can result in ambiguities in nested classes.
           | - When importing, you can avoid naming conflicts by renaming:
           |   import scala.{m => mTick}
           |""".stripMargin,
        DiagnosticSeverity.Error
      )
    )
  )
