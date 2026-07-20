/*
 * Cases ported from the Scala 3 ("dotty") presentation-compiler test suite,
 * version 3.8.4 (presentation-compiler/test/dotty/tools/pc/tests/completion/
 * CompletionMatchSuite.scala):
 *   https://github.com/scala/scala3/tree/3.8.4/presentation-compiler
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc corpus harness; the dotty suite's
 * suite-wide `MockEntries.definitionSourceTopLevels` override became the
 * per-case `mockToplevels` seed, and the scrambled-order cases (mirroring the
 * live boundary leg in crates/ls-jvm/tests/live_definition.rs) were added to
 * pin the seam consumption itself.
 */
package ls.pc.corpus

/** The `match` / `match (exhaustive)` keyword-completion cases exercising the
  * `definitionSourceToplevels` ordering seam at facade level: the PC's
  * exhaustive-match sorter (`MatchCaseCompletions.sortSubclasses`) orders the
  * generated `case` arms by the children's compiler source positions when
  * they ALL have one, and consults `SymbolSearch.definitionSourceToplevels`
  * — our [[ls.pc.PcDefinitionResolver]] seam, mocked per buffer in
  * [[CorpusPc]] — otherwise.
  *
  * Which sealed shapes actually consult the mock (verified empirically at
  * corpus level and pinned by these cases):
  *   - Java enums (`java/nio/file/AccessMode#`): classfile-only children
  *     carry NO source positions, so the sorter consults the seam — in the
  *     `match@@` shape AND the lambda `case@@` shape — and a seeded mock
  *     order is respected; with no (matching) mock entry every child maps to
  *     the sorter's `-1` default and the stable sort keeps the compiler's
  *     declaration order. The same shape the live boundary leg drives
  *     end-to-end in `crates/ls-jvm/tests/live_definition.rs`;
  *   - scala-library `scala/Option#`: the pickled children DO carry source
  *     positions here, so the seam is NEVER consulted and the arms follow
  *     declaration order (Some, None) — dotty's own `MockEntries` Option
  *     seed is inert for ordering (pinned by the skip case below);
  *   - in-buffer Scala enums/sealed hierarchies carry positions and never
  *     reach the seam (`CompletionCaseCorpusSuite`'s `exhaustive-scala-enum`).
  */
class CompletionMatchCorpusSuite extends CorpusCompletionHarness:

  private val accessModeSymbol = "java/nio/file/AccessMode#"

  /** The live-leg scramble: WRITE before READ (declaration order is READ,
    * WRITE, EXECUTE), so mock-driven ordering is distinguishable from the
    * compiler fallback.
    */
  private val accessModeScrambled = Map(
    accessModeSymbol -> Vector(
      "java/nio/file/AccessMode#WRITE.",
      "java/nio/file/AccessMode#READ.",
      "java/nio/file/AccessMode#EXECUTE."
    )
  )

  /** The seam consultations logged since `before` for `symbol`, from a corpus
    * buffer.
    */
  private def seamQueries(before: Int, symbol: String): Vector[(String, String)] =
    CorpusPc.toplevelsQueries.drop(before).filter { (sym, sourceUri) =>
      sym == symbol && sourceUri.startsWith("file:///ls-pc-corpus/")
    }

  check(
    "match",
    """
      |object A {
      |  Option(1) match@@
      |}""".stripMargin,
    """|match
       |match (exhaustive) Option[Int] (2 cases)
       |""".stripMargin
  )

  // The `match (exhaustive)` edit over a scala-library type: Some before None
  // — the children's declaration order via their source positions (upstream
  // `exhaustive-sorting-scalalib`; dotty seeds MockEntries with
  // `scala/Option#` for this, but the positions win before the seam is ever
  // consulted — see the skip case below).
  checkEdit(
    "exhaustive-sorting-scalalib",
    """package sort
      |object App {
      |  Option(1) matc@@
      |}
      |""".stripMargin,
    s"""package sort
       |object App {
       |  Option(1) match
       |\tcase Some(value) => $$0
       |\tcase None =>
       |
       |}
       |""".stripMargin,
    filter = _.contains("exhaustive")
  )

  // The negative seam pin: children WITH source positions never reach the
  // seam. A deliberately scrambled Option mock ([None., Some.]) changes
  // NOTHING — the arms keep the position order and the mock records zero
  // consultations.
  test("exhaustive-scalalib-children-skip-the-seam") {
    val original =
      """package sort
        |object App {
        |  Option(1) matc@@
        |}
        |""".stripMargin
    val before = CorpusPc.toplevelsQueries.length
    val items = getItems(
      original,
      mockToplevels = Map("scala/Option#" -> Vector("scala/None.", "scala/Some."))
    ).filter(_.getLabel.contains("exhaustive"))
    assertEquals(items.length, 1, items.map(_.getLabel).toString)
    assertCorpusNoDiff(
      applyEdits(params(original)._1, items.head),
      s"""package sort
         |object App {
         |  Option(1) match
         |\tcase Some(value) => $$0
         |\tcase None =>
         |
         |}
         |""".stripMargin
    )
    assertEquals(
      seamQueries(before, "scala/Option#"),
      Vector.empty,
      "positioned children must be sorted WITHOUT consulting definitionSourceToplevels"
    )
  }

  // The Java-enum shape WITHOUT a mock entry: the seam is consulted but
  // answers empty, every child maps to the sorter's -1 default, and the
  // stable sort keeps the compiler's declaration order — READ, WRITE,
  // EXECUTE. Ported verbatim from upstream `exhaustive-java-enum` (dotty's
  // MockEntries has no AccessMode entry either).
  checkEdit(
    "exhaustive-java-enum",
    """
      |package example
      |
      |import java.nio.file.AccessMode
      |
      |object Main {
      |  (null: AccessMode) match@@
      |}""".stripMargin,
    s"""
       |package example
       |
       |import java.nio.file.AccessMode
       |
       |object Main {
       |  (null: AccessMode) match
       |\tcase AccessMode.READ => $$0
       |\tcase AccessMode.WRITE =>
       |\tcase AccessMode.EXECUTE =>
       |
       |}""".stripMargin,
    filter = _.contains("exhaustive")
  )

  // The corpus twin of the live boundary leg (live_definition.rs): the mock
  // answers the SCRAMBLED order for the Java enum and the generated arms
  // follow it — plus the explicit seam-consumption pin (the sorter consulted
  // the mock for the enum symbol with the completion buffer as sourceUri).
  test("exhaustive-java-enum-scrambled-mock-order") {
    val original =
      """
        |package example
        |
        |import java.nio.file.AccessMode
        |
        |object Main {
        |  (null: AccessMode) match@@
        |}""".stripMargin
    val before = CorpusPc.toplevelsQueries.length
    val items = getItems(original, mockToplevels = accessModeScrambled)
      .filter(_.getLabel.contains("exhaustive"))
    assertEquals(items.length, 1, items.map(_.getLabel).toString)
    assertCorpusNoDiff(
      applyEdits(params(original)._1, items.head),
      s"""
         |package example
         |
         |import java.nio.file.AccessMode
         |
         |object Main {
         |  (null: AccessMode) match
         |\tcase AccessMode.WRITE => $$0
         |\tcase AccessMode.READ =>
         |\tcase AccessMode.EXECUTE =>
         |
         |}""".stripMargin
    )
    assert(
      seamQueries(before, accessModeSymbol).nonEmpty,
      s"the exhaustive-match sorter must consult definitionSourceToplevels for $accessModeSymbol: " +
        CorpusPc.toplevelsQueries.drop(before).toString
    )
  }

  // `case (exhaustive)` inside a lambda: the same sorter behind the `case`
  // keyword completion (upstream `exhaustive-map` uses Option, whose
  // positioned children never consult the seam — and whose lambda shape
  // yields no exhaustive item at facade level — so the corpus drives the
  // lambda through the Java enum, where the seam IS consulted and the
  // scrambled mock order must show up in the generated arms).
  test("exhaustive-case-lambda-java-enum-scrambled-mock-order") {
    val original =
      """package example
        |import java.nio.file.AccessMode
        |object C {
        |  List.empty[AccessMode].map{ ca@@ }
        |}""".stripMargin
    val before = CorpusPc.toplevelsQueries.length
    val items = getItems(original, mockToplevels = accessModeScrambled)
      .filter(_.getLabel.contains("exhaustive"))
    assertEquals(items.length, 1, items.map(_.getLabel).toString)
    assertCorpusNoDiff(
      trimTrailingSpace(applyEdits(params(original)._1, items.head)),
      s"""package example
         |import java.nio.file.AccessMode
         |object C {
         |  List.empty[AccessMode].map{
         |\tcase AccessMode.WRITE => $$0
         |\tcase AccessMode.READ =>
         |\tcase AccessMode.EXECUTE =>
         | }
         |}""".stripMargin
    )
    assert(
      seamQueries(before, accessModeSymbol).nonEmpty,
      s"the lambda case completion must consult definitionSourceToplevels for $accessModeSymbol: " +
        CorpusPc.toplevelsQueries.drop(before).toString
    )
  }
