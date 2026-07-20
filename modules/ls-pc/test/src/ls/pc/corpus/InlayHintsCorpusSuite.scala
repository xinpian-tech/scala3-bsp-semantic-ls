/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/inlayHints/InlayHintsSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness; curated subset
 * favoring Scala 3 syntax (givens, using, quotes, named tuples, xray chains,
 * closing labels), rendered through the harness port of TestInlayHints.
 */
package ls.pc.corpus

class InlayHintsCorpusSuite extends CorpusInlayHintsHarness:

  check(
    "local",
    """|object Main {
      |  def foo() = {
      |    implicit val imp: Int = 2
      |    def addOne(x: Int)(implicit one: Int) = x + one
      |    val x = addOne(1)
      |  }
      |}
      |""".stripMargin,
    """|object Main {
      |  def foo()/*: Unit<<scala/Unit#>>*/ = {
      |    implicit val imp: Int = 2
      |    def addOne(x: Int)(implicit one: Int)/*: Int<<scala/Int#>>*/ = x + one
      |    val x/*: Int<<scala/Int#>>*/ = addOne(/*x = */1)/*(using imp<<(3:17)>>)*/
      |  }
      |}
      |""".stripMargin
  )

  check(
    "type-params",
    """|object Main {
      |  def hello[T](t: T) = t
      |  val x = hello(List(1))
      |}
      |""".stripMargin,
    """|object Main {
      |  def hello[T](t: T)/*: T<<(2:12)>>*/ = t
      |  val x/*: List<<scala/collection/immutable/List#>>[Int<<scala/Int#>>]*/ = hello/*[List<<scala/collection/immutable/List#>>[Int<<scala/Int#>>]]*/(/*t = */List/*[Int<<scala/Int#>>]*/(/*elems = */1))
      |}
      |""".stripMargin
  )

  check(
    "implicit-param",
    """|case class User(name: String)
      |object Main {
      |  implicit val imp: Int = 2
      |  def addOne(x: Int)(implicit one: Int) = x + one
      |  val x = addOne(1)
      |}
      |""".stripMargin,
    """|case class User(name: String)
      |object Main {
      |  implicit val imp: Int = 2
      |  def addOne(x: Int)(implicit one: Int)/*: Int<<scala/Int#>>*/ = x + one
      |  val x/*: Int<<scala/Int#>>*/ = addOne(/*x = */1)/*(using imp<<(3:15)>>)*/
      |}
      |""".stripMargin
  )

  check(
    "implicit-conversion",
    """|case class User(name: String)
      |object Main {
      |  implicit def intToUser(x: Int): User = new User(x.toString)
      |  val y: User = 1
      |}
      |""".stripMargin,
    """|case class User(name: String)
      |object Main {
      |  implicit def intToUser(x: Int): User = new User(/*name = */x.toString)
      |  val y: User = /*intToUser<<(3:15)>>(*/1/*)*/
      |}
      |""".stripMargin
  )

  check(
    "using-param",
    """|case class User(name: String)
      |object Main {
      |  implicit val imp: Int = 2
      |  def addOne(x: Int)(using one: Int) = x + one
      |  val x = addOne(1)
      |}
      |""".stripMargin,
    """|case class User(name: String)
      |object Main {
      |  implicit val imp: Int = 2
      |  def addOne(x: Int)(using one: Int)/*: Int<<scala/Int#>>*/ = x + one
      |  val x/*: Int<<scala/Int#>>*/ = addOne(/*x = */1)/*(using imp<<(3:15)>>)*/
      |}
      |""".stripMargin
  )

  check(
    "given-conversion",
    """|case class User(name: String)
      |object Main {
      |  given intToUser: Conversion[Int, User] = User(_.toString)
      |  val y: User = 1
      |}
      |""".stripMargin,
    """|case class User(name: String)
      |object Main {
      |  given intToUser: Conversion[Int, User] = User(/*name = */_.toString)
      |  val y: User = /*intToUser<<(3:8)>>(*/1/*)*/
      |}
      |""".stripMargin
  )

  check(
    "basic",
    """|object Main {
      |  val foo = 123
      |}
      |""".stripMargin,
    """|object Main {
      |  val foo/*: Int<<scala/Int#>>*/ = 123
      |}
      |""".stripMargin
  )

  check(
    "list",
    """|object Main {
      |  val foo = List[Int](123)
      |}
      |""".stripMargin,
    """|object Main {
      |  val foo/*: List<<scala/collection/immutable/List#>>[Int<<scala/Int#>>]*/ = List[Int](/*elems = */123)
      |}
      |""".stripMargin
  )

  check(
    "tuple",
    """|object Main {
      |  val foo = (123, 456)
      |}
      |""".stripMargin,
    """|object Main {
      |  val foo/*: (Int<<scala/Int#>>, Int<<scala/Int#>>)*/ = (123, 456)
      |}
      |""".stripMargin
  )

  check(
    "lambda-type",
    """|object Main {
      |  val foo = () => 123
      |}
      |""".stripMargin,
    """|object Main {
      |  val foo/*: () => Int<<scala/Int#>>*/ = () => 123
      |}
      |""".stripMargin
  )

  check(
    "block",
    """|object Main {
      |  val foo = { val z = 123; z + 2}
      |}
      |""".stripMargin,
    """|object Main {
      |  val foo/*: Int<<scala/Int#>>*/ = { val z/*: Int<<scala/Int#>>*/ = 123; z + 2}
      |}
      |""".stripMargin
  )

  check(
    "dealias",
    """|class Foo() {
      |  type T = Int
      |  def getT: T = 1
      |}
      |
      |object O {
      | val c = new Foo().getT
      |}
      |""".stripMargin,
    """|class Foo() {
      |  type T = Int
      |  def getT: T = 1
      |}
      |
      |object O {
      | val c/*: Int<<scala/Int#>>*/ = new Foo().getT
      |}
      |""".stripMargin
  )

  check(
    "list-match",
    """|object Main {
      |  val x = List(1, 2) match {
      |    case hd :: tail => hd
      |  }
      |}
      |""".stripMargin,
    """|object Main {
      |  val x/*: Int<<scala/Int#>>*/ = List/*[Int<<scala/Int#>>]*/(/*elems = */1, 2) match {
      |    case hd :: tail => hd
      |  }
      |}
      |""".stripMargin
  )

  check(
    "case-class-unapply",
    """|object Main {
      |case class Foo[A](x: A, y: A)
      |  val Foo(fst, snd) = Foo(1, 2)
      |}
      |""".stripMargin,
    """|object Main {
      |case class Foo[A](x: A, y: A)
      |  val Foo(fst/*: Int<<scala/Int#>>*/, snd/*: Int<<scala/Int#>>*/) = Foo/*[Int<<scala/Int#>>]*/(/*x = */1, /*y = */2)
      |}
      |""".stripMargin,
    hintsInPatternMatch = true
  )

  check(
    "ord",
    """|object Main {
      |  val ordered = "acb".sorted
      |}
      |""".stripMargin,
    """|object Main {
      |  val ordered/*: String<<scala/Predef.String#>>*/ = /*augmentString<<scala/Predef.augmentString().>>(*/"acb"/*)*/.sorted/*[Char<<scala/Char#>>]*//*(using Char<<scala/math/Ordering.Char.>>)*/
      |}
      |""".stripMargin
  )

  check(
    "partial-fun",
    """|object Main {
      |  List(1).collect { case x => x }
      |  val x: PartialFunction[Int, Int] = {
      |    case 1 => 2
      |  }
      |}
      |""".stripMargin,
    """|object Main {
      |  List/*[Int<<scala/Int#>>]*/(/*elems = */1).collect/*[Int<<scala/Int#>>]*/ { case x => x }
      |  val x: PartialFunction[Int, Int] = {
      |    case 1 => 2
      |  }
      |}
      |""".stripMargin
  )

  check(
    "anonymous-given",
    """|package example
      |
      |trait Ord[T]:
      |  def compare(x: T, y: T): Int
      |
      |given intOrd: Ord[Int] with
      |  def compare(x: Int, y: Int) =
      |    if x < y then -1 else if x > y then +1 else 0
      |
      |given Ord[String] with
      |  def compare(x: String, y: String) =
      |    x.compare(y)
      |
      |""".stripMargin,
    """|package example
      |
      |trait Ord[T]:
      |  def compare(x: T, y: T): Int
      |
      |given intOrd: Ord[Int] with
      |  def compare(x: Int, y: Int)/*: Int<<scala/Int#>>*/ =
      |    if x < y then -1 else if x > y then +1 else 0
      |
      |given Ord[String] with
      |  def compare(x: String, y: String)/*: Int<<scala/Int#>>*/ =
      |    /*augmentString<<scala/Predef.augmentString().>>(*/x/*)*/.compare(/*that = */y)
      |
      |""".stripMargin
  )

  check(
    "context-bounds1",
    """|package example
      |object O {
      |  given Int = 1
      |  def test[T: Ordering](x: T)(using Int) = ???
      |  test(1)
      |}
      |""".stripMargin,
    """|package example
      |object O {
      |  given Int = 1
      |  def test[T: Ordering](x: T)(using Int)/*: Nothing<<scala/Nothing#>>*/ = ???
      |  test/*[Int<<scala/Int#>>]*/(/*x = */1)/*(using Int<<scala/math/Ordering.Int.>>, given_Int<<(2:8)>>)*/
      |}
      |""".stripMargin
  )

  check(
    "quotes1",
    """|package example
       |import scala.quoted.*
       |object O:
       |  def matchTypeImpl[T: Type](param1: Expr[T])(using Quotes) =
       |    import quotes.reflect.*
       |    Type.of[T] match
       |      case '[f] =>
       |        val fr = TypeRepr.of[T]
       |""".stripMargin,
    """|package example
       |import scala.quoted.*
       |object O:
       |  def matchTypeImpl[T: Type](param1: Expr[T])(using Quotes)/*: Unit<<scala/Unit#>>*/ =
       |    import quotes.reflect.*
       |    Type.of[T] match
       |      case '[f] =>
       |        val fr/*: TypeRepr<<scala/quoted/Quotes#reflectModule#TypeRepr#>>*/ = TypeRepr.of[T]/*(using evidence$1<<(3:27)>>)*/
       |""".stripMargin
  )

  check(
    "named-tuples",
    """|def hello = (path = ".", num = 5)
       |
       |def test =
       |  hello ++ (line = 1)
       |
       |@main def bla =
       |   val x: (path: String, num: Int, line: Int) = test
       |""".stripMargin,
    """|def hello/*: (path : String<<java/lang/String#>>, num : Int<<scala/Int#>>)*/ = (path = ".", num = 5)/*[(String<<java/lang/String#>>, Int<<scala/Int#>>)]*/
       |
       |def test/*: (path : String<<java/lang/String#>>, num : Int<<scala/Int#>>, line : Int<<scala/Int#>>)*/ =
       |  hello ++/*[Tuple1<<scala/Tuple1#>>["line"], Tuple1<<scala/Tuple1#>>[Int<<scala/Int#>>]]*/ (line = 1)/*[Tuple1<<scala/Tuple1#>>[Int<<scala/Int#>>]]*//*(using refl<<scala/`<:<`.refl().>>)*/
       |
       |@main def bla/*: Unit<<scala/Unit#>>*/ =
       |   val x: (path: String, num: Int, line: Int) = test
       |""".stripMargin
  )

  check(
    "by-name-regular",
    """|object Main:
       |  def foo(x: => Int, y: Int, z: => Int)(w: Int, v: => Int): Unit = ()
       |  foo(1, 2, 3)(4, 5)
       |""".stripMargin,
    """|object Main:
       |  def foo(x: => Int, y: Int, z: => Int)(w: Int, v: => Int): Unit = ()
       |  foo(/*x = => */1, /*y = */2, /*z = => */3)(/*w = */4, /*v = => */5)
       |""".stripMargin
  )

  check(
    "by-name-block",
    """|object Main:
       |  def Future[A](arg: => A): A = arg
       |
       |  Future(1 + 2)
       |  Future {
       |    1 + 2
       |  }
       |  Future {
       |    val x = 1
       |    val y = 2
       |    x + y
       |  }
       |  Some(Option(2)
       |    .getOrElse {
       |      List(1,2)
       |        .headOption
       |    })
       |""".stripMargin,
    """|object Main:
       |  def Future[A](arg: => A): A = arg
       |
       |  Future/*[Int<<scala/Int#>>]*/(/*arg = => */1 + 2)
       |  Future/*[Int<<scala/Int#>>]*/ {/*=> */
       |    1 + 2
       |  }
       |  Future/*[Int<<scala/Int#>>]*/ {/*=> */
       |    val x/*: Int<<scala/Int#>>*/ = 1
       |    val y/*: Int<<scala/Int#>>*/ = 2
       |    x + y
       |  }
       |  Some/*[Int<<scala/Int#>> | Option<<scala/Option#>>[Int<<scala/Int#>>]]*/(/*value = */Option/*[Int<<scala/Int#>>]*/(/*x = */2)
       |    .getOrElse/*[Int<<scala/Int#>> | Option<<scala/Option#>>[Int<<scala/Int#>>]]*/ {/*=> */
       |      List/*[Int<<scala/Int#>>]*/(/*elems = */1,2)
       |        .headOption
       |    })
       |""".stripMargin
  )

  check(
    "named-parameter",
    """|object Main{
       |  def hello[T](arg: T) = arg
       |  val x = hello(arg = List(1))
       |}
       |""".stripMargin,
    """|object Main{
       |  def hello[T](arg: T)/*: T<<(2:12)>>*/ = arg
       |  val x/*: List<<scala/collection/immutable/List#>>[Int<<scala/Int#>>]*/ = hello/*[List<<scala/collection/immutable/List#>>[Int<<scala/Int#>>]]*/(arg = List/*[Int<<scala/Int#>>]*/(/*elems = */1))
       |}
       |""".stripMargin
  )

  check(
    "default-parameter",
    """|object Main {
       |  def foo(a: Int, b: Int = 2) = a + b
       |  val x = foo(1)
       |}
       |""".stripMargin,
    """|object Main {
       |  def foo(a: Int, b: Int = 2)/*: Int<<scala/Int#>>*/ = a + b
       |  val x/*: Int<<scala/Int#>>*/ = foo(/*a = */1)
       |}
       |""".stripMargin
  )

  check(
    "pattern-match",
    """|package example
       |object O {
       |  val head :: tail = List(1)
       |  List(1) match {
       |    case head :: next =>
       |    case Nil =>
       |  }
       |  Option(Option(1)) match {
       |    case Some(Some(value)) =>
       |    case None =>
       |  }
       |  val (local, _) = ("", 1.0)
       |  val Some(x) = Option(1)
       |  for {
       |    x <- List((1,2))
       |    (z, y) = x
       |  } yield {
       |    x
       |  }
       |}
       |""".stripMargin,
    """|package example
       |object O {
       |  val head :: tail = List/*[Int<<scala/Int#>>]*/(/*elems = */1)
       |  List/*[Int<<scala/Int#>>]*/(/*elems = */1) match {
       |    case head :: next =>
       |    case Nil =>
       |  }
       |  Option/*[Option<<scala/Option#>>[Int<<scala/Int#>>]]*/(/*x = */Option/*[Int<<scala/Int#>>]*/(/*x = */1)) match {
       |    case Some(Some(value)) =>
       |    case None =>
       |  }
       |  val (local, _) = ("", 1.0)
       |  val Some(x) = Option/*[Int<<scala/Int#>>]*/(/*x = */1)
       |  for {
       |    x <- List/*[(Int<<scala/Int#>>, Int<<scala/Int#>>)]*/(/*elems = */(1,2))
       |    (z, y) = x
       |  } yield {
       |    x
       |  }
       |}
       |""".stripMargin
  )

  check(
    "pattern-match1",
    """|package example
       |object O {
       |  val head :: tail = List(1)
       |  List(1) match {
       |    case head :: next =>
       |    case Nil =>
       |  }
       |  Option(Option(1)) match {
       |    case Some(Some(value)) =>
       |    case None =>
       |  }
       |  val (local, _) = ("", 1.0)
       |  val Some(x) = Option(1)
       |  for {
       |    x <- List((1,2))
       |    (z, y) = x
       |  } yield {
       |    x
       |  }
       |}
       |""".stripMargin,
    """|package example
       |object O {
       |  val head/*: Int<<scala/Int#>>*/ :: tail/*: List<<scala/collection/immutable/List#>>[Int<<scala/Int#>>]*/ = List/*[Int<<scala/Int#>>]*/(/*elems = */1)
       |  List/*[Int<<scala/Int#>>]*/(/*elems = */1) match {
       |    case head/*: Int<<scala/Int#>>*/ :: next/*: List<<scala/collection/immutable/List#>>[Int<<scala/Int#>>]*/ =>
       |    case Nil =>
       |  }
       |  Option/*[Option<<scala/Option#>>[Int<<scala/Int#>>]]*/(/*x = */Option/*[Int<<scala/Int#>>]*/(/*x = */1)) match {
       |    case Some(Some(value/*: Int<<scala/Int#>>*/)) =>
       |    case None =>
       |  }
       |  val (local/*: String<<java/lang/String#>>*/, _) = ("", 1.0)
       |  val Some(x/*: Int<<scala/Int#>>*/) = Option/*[Int<<scala/Int#>>]*/(/*x = */1)
       |  for {
       |    x/*: (Int<<scala/Int#>>, Int<<scala/Int#>>)*/ <- List/*[(Int<<scala/Int#>>, Int<<scala/Int#>>)]*/(/*elems = */(1,2))
       |    (z/*: Int<<scala/Int#>>*/, y/*: Int<<scala/Int#>>*/) = x
       |  } yield {
       |    x
       |  }
       |}
       |""".stripMargin,
    hintsInPatternMatch = true
  )

  check(
    "xray-simple-chain",
    """|object Main{
       |  trait Foo {
       |   def bar: Bar
       |  }
       |
       |  trait Bar {
       |    def foo(): Foo
       |  }
       |
       |val foo: Foo = ???
       |
       |val thingy: Bar = foo
       |  .bar
       |  .foo()
       |  .bar
       |}
       |""".stripMargin,
    """|object Main{
       |  trait Foo {
       |   def bar: Bar
       |  }
       |
       |  trait Bar {
       |    def foo(): Foo
       |  }
       |
       |val foo: Foo = ???
       |
       |val thingy: Bar = foo
       |  .bar/*  : Bar<<(6:8)>>*/
       |  .foo()/*: Foo<<(2:8)>>*/
       |  .bar/*  : Bar<<(6:8)>>*/
       |}
       |""".stripMargin
  )

  check(
    "closing-labels-1",
    """|object Main{
       |  def bestNumber: Int = {
       |    234
       |  }
       |}
       |""".stripMargin,
    """|object Main{
       |  def bestNumber: Int = {
       |    234
       |  }/*bestNumber*/
       |}/*Main*/
       |""".stripMargin,
    closingLabels = true
  )

  check(
    "closing-labels-inferred-type",
    """|object Main{
       |  def bestNumber = {
       |    234
       |  }
       |}
       |""".stripMargin,
    """|object Main{
       |  def bestNumber/*: Int<<scala/Int#>>*/ = {
       |    234
       |  }/*bestNumber*/
       |}/*Main*/
       |""".stripMargin,
    closingLabels = true
  )
