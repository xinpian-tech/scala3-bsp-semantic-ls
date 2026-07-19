/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/completion/CompletionSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness; curated
 * Scala-3-syntax subset. Classpath-search completion items come from the
 * island's real `SymbolSearch.search` (the PC-vendored ClasspathSearch over
 * the corpus target's scala-library classpath, matching upstream's
 * `BuildInfo.ideTestsDependencyClasspath`).
 */
package ls.pc.corpus

class CompletionCorpusSuite extends CorpusCompletionHarness:

  check(
    "extension",
    """
      |object A {
      |  "".stripSu@@
      |}""".stripMargin,
    """
      |stripSuffix(suffix: String): String
      |""".stripMargin
  )

  check(
    "using",
    """|class Foo {
       |  def max[T](x: T, y: T)(using ord: Ordered[T]): T =
       |    if ord.compare(x, y) < 0 then y else x
       |}
       |object Main {
       |  new Foo().max@@
       |}
       |""".stripMargin,
    """|max[T](x: T, y: T)(using ord: Ordered[T]): T
       |""".stripMargin
  )

  check(
    "higher-kinded-match-type",
    """|package a
       |
       |trait Foo[A] {
       |  def map[B](f: A => B): Foo[B] = ???
       |}
       |case class Bar[F[_]](bar: F[Int])
       |type M[T] = T match {
       |  case Int => Foo[Int]
       |}
       |object Test:
       |  val x = Bar[M](new Foo[Int]{})
       |  x.bar.m@@
       |""".stripMargin,
    """|map[B](f: Int => B): Foo[B]
       |""".stripMargin,
    topLines = Some(1)
  )

  check(
    "type-lambda",
    """|object O {
       | type TTT = [A <: Int] =>> List[A]
       | val t: TT@@
       |}
       |""".stripMargin,
    "TTT[A <: Int]",
    includeDetail = false
  )

  check(
    "type-lambda2",
    """|object O {
       | type TTT[K <: Int] = [V] =>> Map[K, V]
       | val t: TT@@
       |}
       |""".stripMargin,
    "TTT[K <: Int]",
    includeDetail = false
  )

  check(
    "type-lambda2-with-detail",
    """|object O {
       | type TTT[K <: Int] = [V] =>> Map[K, V]
       | val t: TT@@
       |}
       |""".stripMargin,
    "TTT[K <: Int] = [V] =>> Map[K, V]"
  )

  private val extensionResult =
    """|Foo test
       |Found - scala.collection.Searching
       """.stripMargin

  check(
    "extension-definition-scope",
    """|trait Foo
       |object T:
       |  extension (x: Fo@@)
       |""".stripMargin,
    extensionResult,
    topLines = Some(2)
  )

  check(
    "extension-definition-symbol-search",
    """|object T:
       |  extension (x: ListBuffe@@)
       |""".stripMargin,
    """|ListBuffer[A] - scala.collection.mutable
       |ListBuffer - scala.collection.mutable
       |""".stripMargin
  )

  check(
    "extension-definition-type-parameter",
    """|trait Foo
       |object T:
       |  extension [A <: Fo@@]
       |""".stripMargin,
    extensionResult,
    topLines = Some(2)
  )

  check(
    "extension-definition-using-param-clause",
    """|trait Foo
       |object T:
       |  extension (using Fo@@)
       |""".stripMargin,
    extensionResult,
    topLines = Some(2)
  )

  check(
    "extension-definition-mix-1",
    """|trait Foo
       |object T:
       |  extension (x: Int)(using Fo@@)
       |""".stripMargin,
    extensionResult,
    topLines = Some(2)
  )

  check(
    "extension-definition-mix-2",
    """|trait Foo
       |object T:
       |  extension (using Fo@@)(x: Int)(using Foo)
       |""".stripMargin,
    extensionResult,
    topLines = Some(2)
  )

  check(
    "extension-definition-mix-3",
    """|trait Foo
       |object T:
       |  extension (using Foo)(x: Int)(using Fo@@)
       |""".stripMargin,
    extensionResult,
    topLines = Some(2)
  )

  check(
    "extension-definition-mix-4",
    """|trait Foo
       |object T:
       |  extension [A](x: Fo@@)
       |""".stripMargin,
    extensionResult,
    topLines = Some(2)
  )

  check(
    "extension-definition-mix-5",
    """|trait Foo
       |object T:
       |  extension [A](using Fo@@)(x: Int)
       |""".stripMargin,
    extensionResult,
    topLines = Some(2)
  )

  check(
    "extension-definition-mix-6",
    """|trait Foo
       |object T:
       |  extension [A](using Foo)(x: Fo@@)
       |""".stripMargin,
    extensionResult,
    topLines = Some(2)
  )

  check(
    "extension-definition-mix-7",
    """|trait Foo
       |object T:
       |  extension [A](using Foo)(x: Fo@@)(using Foo)
       |""".stripMargin,
    extensionResult,
    topLines = Some(2)
  )

  check(
    "extension-definition-select",
    """|object Test:
       |  class TestSelect()
       |object T:
       |  extension (x: Test.TestSel@@)
       |""".stripMargin,
    """|TestSelect test.Test
       |""".stripMargin
  )

  check(
    "extension-definition-select-mix-1",
    """|object Test:
       |  class TestSelect()
       |object T:
       |  extension (using Int)(x: Test.TestSel@@)
       |""".stripMargin,
    """|TestSelect test.Test
       |""".stripMargin
  )

  check(
    "extension-definition-select-mix-2",
    """|object Test:
       |  class TestSelect[T]()
       |object T:
       |  extension [T](x: Test.TestSel@@)
       |""".stripMargin,
    """|TestSelect[T] test.Test
       |""".stripMargin
  )

  check(
    "extension-definition-type-variable-inference",
    """|object M:
       |  extension [T](xs: List[T]) def test(p: T => Boolean): List[T] = ???
       |  List(1,2,3).tes@@
       |""".stripMargin,
    """|test(p: Int => Boolean): List[Int]
       |""".stripMargin,
    topLines = Some(1)
  )

  check(
    "old-style-extension-type-variable-inference",
    """|object M:
       |  implicit class ListUtils[T](xs: List[T]) {
       |    def test(p: T => Boolean): List[T] = ???
       |  }
       |  List(1,2,3).tes@@
       |""".stripMargin,
    """|test(p: Int => Boolean): List[Int]
       |""".stripMargin,
    topLines = Some(1)
  )

  check(
    "context-bound-in-extension-construct",
    """
      |object x {
      |  extension [T: Orde@@]
      |}
      |""".stripMargin,
    """Ordered[T] scala.math
      |Ordering[T] scala.math
      |""".stripMargin,
    topLines = Some(2)
  )

  check(
    "context-bounds-in-extension-construct",
    """
      |object x {
      |  extension [T: Ordering: Orde@@]
      |}
      |""".stripMargin,
    """Ordered[T] scala.math
      |Ordering[T] scala.math
      |""".stripMargin,
    topLines = Some(2)
  )

  check(
    "type-bound-in-extension-construct",
    """
      |object x {
      |  extension [T <: Orde@@]
      |}
      |""".stripMargin,
    """Ordered[T] scala.math
      |Ordering[T] scala.math
      |""".stripMargin,
    topLines = Some(2)
  )

  check(
    "no-enum-completions-in-new-context",
    """enum TestEnum:
      |  case TestCase
      |object M:
      |  new TestEnu@@
      |""".stripMargin,
    ""
  )

  check(
    "no-enum-case-completions-in-new-context",
    """enum TestEnum:
      |  case TestCase
      |object M:
      |  new TestEnum.TestCas@@
      |""".stripMargin,
    ""
  )

  check(
    "deduplicated-enum-completions",
    """enum TestEnum:
      |  case TestCase
      |object M:
      |  val x: TestEn@@
      |""".stripMargin,
    """TestEnum test
      |""".stripMargin
  )

  check(
    "multi-export",
    """export scala.collection.{AbstractMap, Se@@}
      |""".stripMargin,
    """Set scala.collection
      |SetOps scala.collection
      |AbstractSet scala.collection
      |BitSet scala.collection
      |BitSetOps scala.collection
      |SortedSet scala.collection
      |SortedSetFactoryDefaults scala.collection
      |SortedSetOps scala.collection
      |StrictOptimizedSetOps scala.collection
      |StrictOptimizedSortedSetOps scala.collection
      |GenSet = scala.collection.Set[X]
      |""".stripMargin,
    filter = _.contains("Set")
  )

  check(
    "empty-export",
    """|export @@
       |""".stripMargin,
    """|java `<root>`
       |javax `<root>`
       |""".stripMargin,
    filter = _.startsWith("java")
  )

  check(
    "empty-export-selector",
    """|export java.@@
       |""".stripMargin,
    """|util java
       |""".stripMargin,
    filter = _.startsWith("util")
  )

  check(
    "derives-no-square-brackets",
    """
      |case class Miau(y: Int) derives Ordering, CanEqu@@
      |""".stripMargin,
    "CanEqual scala"
  )

  check(
    "namedTuple completions",
    """|import scala.NamedTuple.*
       |
       |val person = (name = "Jamie", city = "Lausanne")
       |
       |val n = person.na@@""".stripMargin,
    "name: String",
    filter = _.contains("name")
  )

  check(
    "namedTuple-completions-2",
    """|import scala.NamedTuple.*
       |
       |def hello = (path = ".", num = 5)++ (line = 1)
       |val hello2 = (path = ".", num = 5)++ (line = 1)
       |
       |@main def bla =
       |   hello@@
       |""".stripMargin,
    """|hello2: (path : String, num : Int, line : Int)
       |hello: (path : String, num : Int, line : Int)
    """.stripMargin
  )

  check(
    "Selectable with namedTuple Fields member",
    """|import scala.NamedTuple.*
       |
       |class NamedTupleSelectable extends Selectable {
       |  type Fields <: AnyNamedTuple
       |  def selectDynamic(name: String): Any = ???
       |}
       |
       |val person2 = new NamedTupleSelectable {
       |  type Fields = (name: String, city: String)
       |}
       |
       |val n = person2.na@@""".stripMargin,
    """|name: String
       |selectDynamic(name: String): Any
    """.stripMargin,
    filter = _.contains("name")
  )
