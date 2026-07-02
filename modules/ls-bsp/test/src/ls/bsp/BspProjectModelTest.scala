package ls.bsp

import java.nio.file.Path

/** Pure graph-op tests over a hand-built model (diamond + dangling deps). */
class BspProjectModelTest extends munit.FunSuite:

  private def target(id: String, deps: String*): BspTarget =
    BspTarget(
      bspId = id,
      displayName = id,
      scalaVersion = "3.8.4",
      scalacOptions = Vector("-Xsemanticdb"),
      classDirectory = Path.of(s"/out/$id/classes"),
      semanticdbRoot = Some(Path.of(s"/out/$id/classes")),
      sourceroot = Some(Path.of("/ws")),
      sources = Vector(Path.of(s"/ws/$id/src/Main.scala")),
      directDeps = deps.toVector
    )

  // diamond: base <- left, base <- right, left <- top, right <- top
  private val diamond = BspProjectModel(
    targets = Vector(
      target("base"),
      target("left", "base"),
      target("right", "base", "external-dep"),
      target("top", "left", "right")
    ),
    uriToTarget = Map("file:///ws/base/src/Main.scala" -> "base")
  )

  test("reverseDependencyClosure walks the diamond exactly once per node") {
    assertEquals(
      diamond.reverseDependencyClosure("base"),
      Set("base", "left", "right", "top")
    )
    assertEquals(diamond.reverseDependencyClosure("left"), Set("left", "top"))
    assertEquals(diamond.reverseDependencyClosure("top"), Set("top"))
  }

  test("unknown ids yield empty results") {
    assertEquals(diamond.reverseDependencyClosure("nope"), Set.empty[String])
    assertEquals(diamond.dependenciesOf("nope"), Vector.empty[String])
    assertEquals(diamond.dependentsOf("nope"), Vector.empty[String])
    assertEquals(diamond.targetFor("nope"), None)
  }

  test("dangling deps to filtered-out targets are ignored by graph ops") {
    // 'right' lists external-dep, which is not part of the model
    assertEquals(diamond.dependenciesOf("right"), Vector("base"))
    assertEquals(diamond.dependentsOf("external-dep"), Vector.empty[String])
    assertEquals(diamond.reverseDependencyClosure("external-dep"), Set.empty[String])
  }

  test("dependentsOf is sorted and deduplicated") {
    assertEquals(diamond.dependentsOf("base"), Vector("left", "right"))
    assertEquals(diamond.dependenciesOf("top"), Vector("left", "right"))
  }

  test("targetOfUri resolves through uriToTarget") {
    assertEquals(
      diamond.targetOfUri("file:///ws/base/src/Main.scala").map(_.bspId),
      Some("base")
    )
    assertEquals(diamond.targetOfUri("file:///elsewhere.scala"), None)
  }

  test("indexable partitioning on a model with a non-semanticdb target") {
    val model = BspProjectModel(
      Vector(target("ok"), target("no-sdb").copy(semanticdbRoot = None, scalacOptions = Vector.empty)),
      Map.empty
    )
    assertEquals(model.indexableTargets.map(_.bspId), Vector("ok"))
    assertEquals(model.unavailableTargets.map(_.bspId), Vector("no-sdb"))
    assertEquals(model.unavailableErrors.map(_.message).size, 1)
  }
