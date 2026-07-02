package ls.semanticdb

class Md5Suite extends munit.FunSuite:

  test("computeHex matches known MD5 vectors, uppercase"):
    assertEquals(Md5.computeHex(""), "D41D8CD98F00B204E9800998ECF8427E")
    assertEquals(Md5.computeHex("hello"), "5D41402ABC4B2A76B9719D911017C592")
    // non-ASCII goes through UTF-8
    assertEquals(Md5.computeHex("héllo"), Md5.computeHex("héllo"))

  test("validate: fresh when md5 matches (case-insensitive)"):
    val md5 = Md5.computeHex("object A")
    assertEquals(Md5.validate("object A", md5), FreshnessCheck.Fresh)
    assertEquals(Md5.validate("object A", md5.toLowerCase), FreshnessCheck.Fresh)
    assert(Md5.validate("object A", md5).isFresh)

  test("validate: stale when text changed"):
    val stored = Md5.computeHex("object A")
    Md5.validate("object B", stored) match
      case FreshnessCheck.Stale(documentMd5, sourceMd5) =>
        assertEquals(documentMd5, stored)
        assertEquals(sourceMd5, Md5.computeHex("object B"))
      case other => fail(s"expected Stale, got $other")
    assert(!Md5.validate("object B", stored).isFresh)

  test("validate: missing md5"):
    assertEquals(Md5.validate("anything", ""), FreshnessCheck.MissingMd5)
    assert(!Md5.validate("anything", "").isFresh)

  test("validate against SdbDocument"):
    val doc = SdbDocument(4, "a.scala", "", Md5.computeHex("src"), 1, Vector.empty, Vector.empty)
    assertEquals(Md5.validate("src", doc), FreshnessCheck.Fresh)
    assertEquals(
      Md5.validate("changed", doc),
      FreshnessCheck.Stale(doc.md5, Md5.computeHex("changed"))
    )
