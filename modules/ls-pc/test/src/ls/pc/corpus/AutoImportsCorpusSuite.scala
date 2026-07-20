/*
 * Test cases ported from the Scala 3 ("dotty") presentation-compiler test
 * suite, version 3.8.4:
 *   https://github.com/scala/scala3/blob/3.8.4/presentation-compiler/test/dotty/tools/pc/tests/edit/AutoImportsSuite.scala
 * Copyright 2002-2026 EPFL and Lightbend, Inc. dba Akka
 * Licensed under the Apache License, Version 2.0:
 *   http://www.apache.org/licenses/LICENSE-2.0
 * Modifications: re-homed onto the ls.pc facade munit harness (the PcFacade
 * autoImports carrier; LSP positions instead of offsets).
 * Curated 17 of 33 cases: dropped the Ammonite/worksheet/scala-cli directive cases (filename-scheme semantics the facade does not model), the workspace-search-dependent cases (basic-function-apply, package-object, i6477, soft-keyword-check-test - the corpus facade has no workspace symbol index), and symbol-prefix-existing (identical to symbol-no-prefix in the 3.8.4 tree).
 */
package ls.pc.corpus

class AutoImportsCorpusSuite extends CorpusAutoImportsHarness:

  check(
    "basic",
    """|object A {
       |  <<Future>>.successful(2)
       |}
       |""".stripMargin,
    """|scala.concurrent
       |""".stripMargin
  )

  check(
    "basic-apply",
    """|object A {
        |  <<Future>>(2)
        |}
        |""".stripMargin,
    """|scala.concurrent
       |""".stripMargin
  )

  check(
    "basic-apply-wrong",
    """|object A {
        |  new <<Future>>(2)
        |}
        |""".stripMargin,
    """|scala.concurrent
       |java.util.concurrent
       |""".stripMargin
  )

  check(
    "basic-fuzzy",
    """|object A {
        |  <<Future>>.thisMethodDoesntExist(2)
        |}
        |""".stripMargin,
    """|scala.concurrent
       |java.util.concurrent
       |""".stripMargin
  )

  check(
    "typed-simple",
    """|object A {
        |  import scala.concurrent.Promise
        |  val fut: <<Future>> = Promise[Unit]().future
        |}
        |""".stripMargin,
    """|scala.concurrent
       |java.util.concurrent
       |""".stripMargin
  )

  checkEdit(
    "basic-edit",
    """|package a
       |
       |object A {
       |  <<Future>>.successful(2)
       |}
       |""".stripMargin,
    """|package a
       |
       |import scala.concurrent.Future
       |
       |object A {
       |  Future.successful(2)
       |}
       |""".stripMargin
  )

  checkEdit(
    "basic-edit-comment",
    """|/**
       | * @param code
       | * @return
       |*/
       |object A {
       |  <<Future>>.successful(2)
       |}
       |""".stripMargin,
    """|import scala.concurrent.Future
       |/**
       | * @param code
       | * @return
       |*/
       |object A {
       |  Future.successful(2)
       |}
       |""".stripMargin
  )

  checkEdit(
    "symbol-no-prefix",
    """|package a
       |
       |object A {
       |  val uuid = <<UUID>>.randomUUID()
       |}
       |""".stripMargin,
    """|package a
       |
       |import java.util.UUID
       |
       |object A {
       |  val uuid = UUID.randomUUID()
       |}
       |""".stripMargin
  )

  checkEdit(
    "symbol-prefix",
    """|package a
       |
       |object A {
       |  val l : <<Map>>[String, Int] = ???
       |}
       |""".stripMargin,
    """|package a
       |
       |import java.{util => ju}
       |
       |object A {
       |  val l : ju.Map[String, Int] = ???
       |}
       |""".stripMargin
  )

  checkEdit(
    "interpolator-edit",
    """|package a
       |
       |object A {
       |  val l = s"${<<Seq>>(2)}"
       |}
       |""".stripMargin,
    """|package a
       |
       |import scala.collection.mutable
       |
       |object A {
       |  val l = s"${mutable.Seq(2)}"
       |}
       |""".stripMargin
  )

  checkEdit(
    "import-inside-package-object",
    """|package a
       |
       |package object b {
       |  val l = s"${<<ListBuffer>>(2)}"
       |}
       |""".stripMargin,
    """|package a
       |
       |import scala.collection.mutable.ListBuffer
       |
       |package object b {
       |  val l = s"${ListBuffer(2)}"
       |}
       |""".stripMargin
  )

  checkEdit(
    "multiple-packages",
    """|package a
       |package b
       |package c
       |
       |object A {
       |  val l = s"${<<ListBuffer>>(2)}"
       |}
       |""".stripMargin,
    """|package a
       |package b
       |package c
       |
       |import scala.collection.mutable.ListBuffer
       |
       |object A {
       |  val l = s"${ListBuffer(2)}"
       |}
       |""".stripMargin
  )

  checkEdit(
    "multiple-packages-existing-imports",
    """|package a
       |package b
       |package c
       |
       |import scala.concurrent.Future
       |
       |object A {
       |  val l = s"${<<ListBuffer>>(2)}"
       |}
       |""".stripMargin,
    """|package a
       |package b
       |package c
       |
       |import scala.concurrent.Future
       |import scala.collection.mutable.ListBuffer
       |
       |object A {
       |  val l = s"${ListBuffer(2)}"
       |}
       |""".stripMargin
  )

  checkEdit(
    "import-in-import",
    """|package inimport
       |
       |object A {
       |  import <<ExecutionContext>>.global
       |}
       |""".stripMargin,
    """|package inimport
       |
       |object A {
       |  import scala.concurrent.ExecutionContext.global
       |}
       |""".stripMargin
  )

  checkEdit(
    "object-import",
    """|object A {
       |  //some comment
       |  val p: <<Path>> = ???
       |}
       |""".stripMargin,
    """|import java.nio.file.Path
       |object A {
       |  //some comment
       |  val p: Path = ???
       |}
       |""".stripMargin
  )

  checkEdit(
    "toplevels-import",
    """|//some comment
       |
       |val p: <<Path>> = ???
       |
       |//some other comment
       |
       |val v = 1
       |""".stripMargin,
    """|//some comment
       |import java.nio.file.Path
       |
       |val p: Path = ???
       |
       |//some other comment
       |
       |val v = 1
       |""".stripMargin
  )

  checkEdit(
    "use-packages-in-scope",
    """|import scala.collection.mutable as mut
       |
       |val l = <<ListBuffer>>(2)
       |""".stripMargin,
    """|import scala.collection.mutable as mut
       |import mut.ListBuffer
       |
       |val l = ListBuffer(2)
       |""".stripMargin
  )
