package ls.index

class TargetGraphSuite extends munit.FunSuite:

  private def t(i: Long) = TargetId(i)

  // a <- b <- c, a <- d (b and d depend on a, c depends on b)
  private val graph = TargetGraph(
    Vector(t(1), t(2), t(3), t(4)),
    Map(
      t(2) -> Set(t(1)),
      t(3) -> Set(t(2)),
      t(4) -> Set(t(1))
    )
  )

  test("reverseDependencyClosure includes the target itself"):
    assertEquals(graph.reverseDependencyClosure(t(3)), Set(t(3)))

  test("reverseDependencyClosure is transitive"):
    assertEquals(graph.reverseDependencyClosure(t(1)), Set(t(1), t(2), t(3), t(4)))
    assertEquals(graph.reverseDependencyClosure(t(2)), Set(t(2), t(3)))

  test("dependentsOf is the direct reverse edge set"):
    assertEquals(graph.dependentsOf(t(1)), Set(t(2), t(4)))
    assertEquals(graph.dependentsOf(t(3)), Set.empty[TargetId])

  test("TargetBitset membership and bounds"):
    val bs = TargetBitset.of(130, Seq(0, 64, 129))
    assert(bs.contains(0))
    assert(bs.contains(64))
    assert(bs.contains(129))
    assert(!bs.contains(1))
    assert(!bs.contains(-1))
    assert(!bs.contains(130))
    assertEquals(bs.cardinality, 3)

  test("TargetBitset intersects and intersectsWords"):
    val a = TargetBitset.of(128, Seq(3, 70))
    val b = TargetBitset.of(128, Seq(70))
    val c = TargetBitset.of(128, Seq(4))
    assert(a.intersects(b))
    assert(!a.intersects(c))
    assert(a.intersectsWords(b.toWords))
    assert(!a.intersectsWords(c.toWords))

  test("UnsafeReason.explain lists every set bit"):
    val mask = UnsafeReason.External | UnsafeReason.OverrideFamily
    val msgs = UnsafeReason.explain(mask)
    assertEquals(msgs.length, 2)
    assert(msgs.exists(_.contains("outside the workspace")))
    assert(msgs.exists(_.contains("override family")))
    assertEquals(UnsafeReason.explain(0L), Nil)
