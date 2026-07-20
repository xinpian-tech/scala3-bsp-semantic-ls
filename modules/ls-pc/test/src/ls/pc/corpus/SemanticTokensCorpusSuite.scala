/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/tokens/SemanticTokensSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness; curated subset
 * favoring Scala 3 syntax (enums, end markers, export clauses, extension
 * blocks, structural types, @main, for-comprehensions), rendered through the
 * harness port of TestSemanticTokens.pcSemanticString.
 */
package ls.pc.corpus

class SemanticTokensCorpusSuite extends CorpusSemanticTokensHarness:

  check(
    "class, object, var, val(readonly), method, type, parameter, String(single-line)",
    s"""|package <<example>>/*namespace*/
        |
        |class <<Test>>/*class*/{
        |
        |  var <<wkStr>>/*variable,definition*/ = "Dog-"
        |  val <<nameStr>>/*variable,definition,readonly*/ = "Jack"
        |
        |  def <<Main>>/*method,definition*/={
        |
        |    val <<preStr>>/*variable,definition,readonly*/= "I am "
        |    var <<postStr>>/*variable,definition*/= "in a house. "
        |    <<wkStr>>/*variable*/=<<nameStr>>/*variable,readonly*/ <<+>>/*method*/ "Cat-"
        |
        |    <<testC>>/*class*/.<<bc>>/*method*/(<<preStr>>/*variable,readonly*/
        |      <<+>>/*method*/ <<wkStr>>/*variable*/
        |      <<+>>/*method*/ <<preStr>>/*variable,readonly*/)
        |  }
        |}
        |
        |object <<testC>>/*class*/{
        |
        |  def <<bc>>/*method,definition*/(<<msg>>/*parameter,declaration,readonly*/:<<String>>/*type*/)={
        |    <<println>>/*method*/(<<msg>>/*parameter,readonly*/)
        |  }
        |}
        |""".stripMargin
  )

  check(
    "metals-6823",
    s"""|package <<example>>/*namespace*/
        |
        | @<<main>>/*class*/ def <<main1>>/*method,definition*/(): <<Unit>>/*class,abstract*/ =
        |     val <<array>>/*variable,definition,readonly*/ = <<Array>>/*class*/(1, 2, 3)
        |     <<println>>/*method*/(<<array>>/*variable,readonly*/)
        |
        |@<<main>>/*class*/ def <<main2>>/*method,definition*/(): <<Unit>>/*class,abstract*/ =
        |   val <<list>>/*variable,definition,readonly*/ = <<List>>/*class*/(1, 2, 3)
        |   <<println>>/*method*/(<<list>>/*variable,readonly*/)
        |
        |@<<main>>/*class*/ def <<main3>>/*method,definition*/(): <<Unit>>/*class,abstract*/ =
        |   val <<list>>/*variable,definition,readonly*/ = <<List>>/*class*/(1, 2, 3)
        |   <<println>>/*method*/(<<list>>/*variable,readonly*/)
        |""".stripMargin
  )

  check(
    "Comment(Single-Line, Multi-Line)",
    s"""|package <<example>>/*namespace*/
        |
        |object <<Main>>/*class*/{
        |
        |   /**
        |   * Test of Comment Block
        |   */  val <<x>>/*variable,definition,readonly*/ = 1
        |
        |  def <<add>>/*method,definition*/(<<a>>/*parameter,declaration,readonly*/ : <<Int>>/*class,abstract*/) = {
        |    // Single Line Comment
        |    <<a>>/*parameter,readonly*/ <<+>>/*method*/ 1 // com = 1
        |  }
        |}
        |""".stripMargin
  )

  check(
    "abstract(modifier), trait, type parameter",
    s"""|
        |package <<a>>/*namespace*/.<<b>>/*namespace*/
        |object <<Sample5>>/*class*/ {
        |
        |  type <<PP>>/*type,definition*/ = <<Int>>/*class,abstract*/
        |  def <<main>>/*method,definition*/(<<args>>/*parameter,declaration,readonly*/: <<Array>>/*class*/[<<String>>/*type*/]) ={
        |      val <<itr>>/*variable,definition,readonly*/ = new <<IntIterator>>/*class*/(5)
        |      var <<str>>/*variable,definition*/ = <<itr>>/*variable,readonly*/.<<next>>/*method*/().<<toString>>/*method*/ <<+>>/*method*/ ","
        |          <<str>>/*variable*/ += <<itr>>/*variable,readonly*/.<<next>>/*method*/().<<toString>>/*method*/
        |      <<println>>/*method*/("count:"<<+>>/*method*/<<str>>/*variable*/)
        |  }
        |
        |  trait <<Iterator>>/*interface,abstract*/[<<A>>/*typeParameter,definition,abstract*/] {
        |    def <<next>>/*method,declaration*/(): <<A>>/*typeParameter,abstract*/
        |  }
        |
        |  abstract class <<hasLogger>>/*class,abstract*/ {
        |    def <<log>>/*method,definition*/(<<str>>/*parameter,declaration,readonly*/:<<String>>/*type*/) = {<<println>>/*method*/(<<str>>/*parameter,readonly*/)}
        |  }
        |
        |  class <<IntIterator>>/*class*/(<<to>>/*variable,declaration,readonly*/: <<Int>>/*class,abstract*/)
        |  extends <<hasLogger>>/*class,abstract*/ with <<Iterator>>/*interface,abstract*/[<<Int>>/*class,abstract*/]  {
        |    private var <<current>>/*variable,definition*/ = 0
        |    override def <<next>>/*method,definition*/(): <<Int>>/*class,abstract*/ = {
        |      if (<<current>>/*variable*/ <<<>>/*method*/ <<to>>/*variable,readonly*/) {
        |        <<log>>/*method*/("main")
        |        val <<t>>/*variable,definition,readonly*/ = <<current>>/*variable*/
        |        <<current>>/*variable*/ = <<current>>/*variable*/ <<+>>/*method*/ 1
        |        <<t>>/*variable,readonly*/
        |      } else 0
        |    }
        |  }
        |}
        |
        |
        |""".stripMargin
  )

  check(
    "deprecated",
    s"""|package <<example>>/*namespace*/
        |object <<sample9>>/*class*/ {
        |  @<<deprecated>>/*class*/("this method will be removed", "FooLib 12.0")
        |  def <<oldMethod>>/*method,definition,deprecated*/(<<x>>/*parameter,declaration,readonly*/: <<Int>>/*class,abstract*/) = <<x>>/*parameter,readonly*/
        |
        |  def <<main>>/*method,definition*/(<<args>>/*parameter,declaration,readonly*/: <<Array>>/*class*/[<<String>>/*type*/]) ={
        |    val <<str>>/*variable,definition,readonly*/ = <<oldMethod>>/*method,deprecated*/(2).<<toString>>/*method*/
        |     <<println>>/*method*/("Hello, world!"<<+>>/*method*/ <<str>>/*variable,readonly*/)
        |  }
        |}
        |""".stripMargin
  )

  check(
    "import(Out of File)",
    s"""|package <<example>>/*namespace*/
        |
        |import <<scala>>/*namespace*/.<<collection>>/*namespace*/.<<immutable>>/*namespace*/.<<SortedSet>>/*class*/
        |
        |object <<sample3>>/*class*/ {
        |
        |  def <<sorted1>>/*method,definition*/(<<x>>/*parameter,declaration,readonly*/: <<Int>>/*class,abstract*/)
        |     = <<SortedSet>>/*class*/(<<x>>/*parameter,readonly*/)
        |}
        |
        |""".stripMargin
  )

  check(
    "anonymous-class",
    s"""|package <<example>>/*namespace*/
        |object <<A>>/*class*/ {
        |  trait <<Methodable>>/*interface,abstract*/[<<T>>/*typeParameter,definition,abstract*/] {
        |    def <<method>>/*method,declaration*/(<<asf>>/*parameter,declaration,readonly*/: <<T>>/*typeParameter,abstract*/): <<Int>>/*class,abstract*/
        |  }
        |
        |  abstract class <<Alp>>/*class,abstract*/(<<alp>>/*variable,declaration,readonly*/: <<Int>>/*class,abstract*/) extends <<Methodable>>/*interface,abstract*/[<<String>>/*type*/] {
        |    def <<method>>/*method,definition*/(<<adf>>/*parameter,declaration,readonly*/: <<String>>/*type*/) = 123
        |  }
        |  val <<a>>/*variable,definition,readonly*/ = new <<Alp>>/*class,abstract*/(<<alp>>/*parameter,readonly*/ = 10) {
        |    override def <<method>>/*method,definition*/(<<adf>>/*parameter,declaration,readonly*/: <<String>>/*type*/): <<Int>>/*class,abstract*/ = 321
        |  }
        |}""".stripMargin
  )

  check(
    "import-rename",
    s"""|package <<example>>/*namespace*/
        |
        |import <<util>>/*namespace*/.{<<Failure>>/*class*/ => <<NoBad>>/*class*/}
        |
        |class <<Imports>>/*class*/ {
        |  // rename reference
        |  <<NoBad>>/*class*/(null)
        |}""".stripMargin
  )

  check(
    "pattern-match",
    s"""|package <<example>>/*namespace*/
        |
        |class <<Imports>>/*class*/ {
        |
        |  val <<a>>/*variable,definition,readonly*/ = <<Option>>/*class*/(<<Option>>/*class*/(""))
        |  <<a>>/*variable,readonly*/ match {
        |    case <<Some>>/*class*/(<<Some>>/*class*/(<<b>>/*variable,definition,readonly*/)) => <<b>>/*variable,readonly*/
        |    case <<Some>>/*class*/(<<b>>/*variable,definition,readonly*/) => <<b>>/*variable,readonly*/
        |    case <<other>>/*variable,definition,readonly*/ =>
        |  }
        |}""".stripMargin
  )

  check(
    "pattern-match-value",
    s"""|package <<example>>/*namespace*/
        |
        |object <<A>>/*class*/ {
        |  val <<x>>/*variable,definition,readonly*/ = <<List>>/*class*/(1,2,3)
        |  val <<s>>/*variable,definition,readonly*/ = <<Some>>/*class*/(1)
        |  val <<Some>>/*class*/(<<s1>>/*variable,definition,readonly*/) = <<s>>/*variable,readonly*/
        |  val <<Some>>/*class*/(<<s2>>/*variable,definition,readonly*/) = <<s>>/*variable,readonly*/
        |}
        |""".stripMargin
  )

  check(
    "enum",
    """|package <<example>>/*namespace*/
       |
       |enum <<FooEnum>>/*enum,abstract*/:
       |  case <<Bar>>/*enum*/, <<Baz>>/*enum*/
       |object <<FooEnum>>/*class*/
       |""".stripMargin
  )

  check(
    "enum1",
    """|package <<example>>/*namespace*/
       |
       |enum <<FooEnum>>/*enum,abstract*/:
       |  case <<A>>/*enum*/(<<a>>/*variable,declaration,readonly*/: <<Int>>/*class,abstract*/)
       |  case <<B>>/*enum*/(<<a>>/*variable,declaration,readonly*/: <<Int>>/*class,abstract*/, <<b>>/*variable,declaration,readonly*/: <<Int>>/*class,abstract*/)
       |  case <<C>>/*enum*/(<<a>>/*variable,declaration,readonly*/: <<Int>>/*class,abstract*/, <<b>>/*variable,declaration,readonly*/: <<Int>>/*class,abstract*/, <<c>>/*variable,declaration,readonly*/: <<Int>>/*class,abstract*/)
       |
       |""".stripMargin
  )

  check(
    "structural-types",
    s"""|package <<example>>/*namespace*/
        |
        |import <<reflect>>/*namespace*/.<<Selectable>>/*class*/.<<reflectiveSelectable>>/*method*/
        |
        |object <<StructuralTypes>>/*class*/:
        |  type <<User>>/*type,definition*/ = {
        |    def <<name>>/*method,declaration*/: <<String>>/*type*/
        |    def <<age>>/*method,declaration*/: <<Int>>/*class,abstract*/
        |  }
        |
        |  val <<user>>/*variable,definition,readonly*/ = null.<<asInstanceOf>>/*method*/[<<User>>/*type*/]
        |  <<user>>/*variable,readonly*/.<<name>>/*method*/
        |  <<user>>/*variable,readonly*/.<<age>>/*method*/
        |
        |  val <<V>>/*variable,definition,readonly*/: <<Object>>/*class*/ {
        |    def <<scalameta>>/*method,declaration*/: <<String>>/*type*/
        |  } = new:
        |    def <<scalameta>>/*method,definition*/ = "4.0"
        |  <<V>>/*variable,readonly*/.<<scalameta>>/*method*/
        |end <<StructuralTypes>>/*class,definition*/
        |""".stripMargin
  )

  check(
    "vars",
    s"""|package <<example>>/*namespace*/
        |
        |object <<A>>/*class*/ {
        |  val <<a>>/*variable,definition,readonly*/ = 1
        |  var <<b>>/*variable,definition*/ = 2
        |  val <<c>>/*variable,definition,readonly*/ = <<List>>/*class*/(1,<<a>>/*variable,readonly*/,<<b>>/*variable*/)
        |  <<b>>/*variable*/ = <<a>>/*variable,readonly*/
        |""".stripMargin
  )

  check(
    "val-object",
    """
      |case class <<X>>/*class*/(<<a>>/*variable,declaration,readonly*/: <<Int>>/*class,abstract*/)
      |object <<X>>/*class*/
      |
      |object <<Main>>/*class*/ {
      |  val <<x>>/*class,definition*/ = <<X>>/*class*/
      |  val <<y>>/*variable,definition,readonly*/ = <<X>>/*class*/(1)
      |}
      |""".stripMargin
  )

  check(
    "case-class",
    """|case class <<Foo>>/*class*/(<<i>>/*variable,declaration,readonly*/: <<Int>>/*class,abstract*/, <<j>>/*variable,declaration,readonly*/: <<Int>>/*class,abstract*/)
       |
       |object <<A>>/*class*/ {
       |  val <<f>>/*variable,definition,readonly*/ = <<Foo>>/*class*/(1,2)
       |}
       |""".stripMargin
  )

  check(
    "main-annot",
    """|@<<main>>/*class*/ def <<main>>/*method,definition*/(<<args>>/*parameter,declaration,readonly*/: <<Array>>/*class*/[<<String>>/*type*/]): <<Unit>>/*class,abstract*/ = ()
       |""".stripMargin
  )

  check(
    "for-comprehension",
    """|package <<example>>/*namespace*/
       |
       |object <<B>>/*class*/ {
       |  val <<a>>/*variable,definition,readonly*/ = for {
       |    <<foo>>/*parameter,declaration,readonly*/ <- <<List>>/*class*/("a", "b", "c")
       |    <<_>>/*class,abstract*/ = <<println>>/*method*/("print!")
       |  } yield <<foo>>/*parameter,readonly*/
       |}
       |""".stripMargin
  )

  check(
    "named-arg-backtick",
    """|object <<Main>>/*class*/ {
       |  def <<foo>>/*method,definition*/(<<`type`>>/*parameter,declaration,readonly*/: <<String>>/*type*/): <<String>>/*type*/ = <<`type`>>/*parameter,readonly*/
       |  val <<x>>/*variable,definition,readonly*/ = <<foo>>/*method*/(
       |    <<`type`>>/*parameter,readonly*/ = "abc"
       |  )
       |}
       |""".stripMargin
  )

  check(
    "end-marker",
    """|def <<foo>>/*method,definition*/ =
       |  1
       |end <<foo>>/*method,definition*/
       |""".stripMargin
  )

  check(
    "constructor2",
    """
      |object <<Main>>/*class*/ {
      |  class <<Abc>>/*class*/[<<T>>/*typeParameter,definition,abstract*/](<<abc>>/*variable,declaration,readonly*/: <<T>>/*typeParameter,abstract*/)
      |  object <<Abc>>/*class*/ {
      |    def <<apply>>/*method,definition*/[<<T>>/*typeParameter,definition,abstract*/](<<abc>>/*parameter,declaration,readonly*/: <<T>>/*typeParameter,abstract*/, <<bde>>/*parameter,declaration,readonly*/: <<T>>/*typeParameter,abstract*/) = new <<Abc>>/*class*/(<<abc>>/*parameter,readonly*/)
      |  }
      |  val <<x>>/*variable,definition,readonly*/ = <<Abc>>/*class*/(123, 456)
      |}""".stripMargin
  )

  check(
    "i5977",
    """
      |sealed trait <<ExtensionProvider>>/*interface,abstract*/ {
      |  extension [<<A>>/*typeParameter,definition,abstract*/] (<<self>>/*parameter,declaration,readonly*/: <<A>>/*typeParameter,abstract*/) {
      |    def <<typeArg>>/*method,declaration*/[<<B>>/*typeParameter,definition,abstract*/]: <<B>>/*typeParameter,abstract*/
      |    def <<inferredTypeArg>>/*method,declaration*/[<<C>>/*typeParameter,definition,abstract*/](<<value>>/*parameter,declaration,readonly*/: <<C>>/*typeParameter,abstract*/): <<C>>/*typeParameter,abstract*/
      |}
      |
      |object <<Repro>>/*class*/ {
      |  def <<usage>>/*method,definition*/[<<A>>/*typeParameter,definition,abstract*/](<<f>>/*parameter,declaration,readonly*/: <<ExtensionProvider>>/*interface,abstract*/ ?=> <<A>>/*typeParameter,abstract*/ => <<Any>>/*class,abstract*/): <<Any>>/*class,abstract*/ = <<???>>/*method*/
      |
      |  <<usage>>/*method*/[<<Int>>/*class,abstract*/](<<_>>/*parameter,readonly*/.<<inferredTypeArg>>/*method*/("str"))
      |  <<usage>>/*method*/[<<Int>>/*class,abstract*/](<<_>>/*parameter,readonly*/.<<inferredTypeArg>>/*method*/[<<String>>/*type*/]("str"))
      |  <<usage>>/*method*/[<<Option>>/*class,abstract*/[<<Int>>/*class,abstract*/]](<<_>>/*parameter,readonly*/.<<typeArg>>/*method*/[<<Some>>/*class*/[<<Int>>/*class,abstract*/]].<<value>>/*variable,readonly*/.<<inferredTypeArg>>/*method*/("str"))
      |  <<usage>>/*method*/[<<Option>>/*class,abstract*/[<<Int>>/*class,abstract*/]](<<_>>/*parameter,readonly*/.<<typeArg>>/*method*/[<<Some>>/*class*/[<<Int>>/*class,abstract*/]].<<value>>/*variable,readonly*/.<<inferredTypeArg>>/*method*/[<<String>>/*type*/]("str"))
      |}
      |""".stripMargin
  )

  check(
    "local-object-with-end-i7246",
    """|def <<bar>>/*method,definition*/ =
       |  object <<foo>>/*class*/:
       |    def <<aaa>>/*method,definition*/ = <<???>>/*method*/
       |  end <<foo>>/*class,definition*/
       |""".stripMargin
  )

  check(
    "i7256",
    """|object <<Test>>/*class*/:
       |  def <<methodA>>/*method,definition*/: <<Unit>>/*class,abstract*/ = <<???>>/*method*/
       |export <<Test>>/*class*/.<<methodA>>/*method*/
       |""".stripMargin
  )
