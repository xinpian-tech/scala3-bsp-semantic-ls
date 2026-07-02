package ls.index

class RuntimeContractSuite extends munit.FunSuite:

  test("tests run on Java 25 (Java 25 only contract)"):
    assertEquals(Runtime.version().feature(), 25)

  test("Span.pack round-trips line and character"):
    val p = Span.pack(1234, 56)
    assertEquals(Span.unpackLine(p), 1234)
    assertEquals(Span.unpackChar(p), 56)

  test("Span.pack saturates characters beyond 12 bits"):
    val p = Span.pack(3, 5000)
    assertEquals(Span.unpackLine(p), 3)
    assertEquals(Span.unpackChar(p), Span.CharMask)

  test("Span.pack orders by (line, char) as unsigned int"):
    assert(Span.pack(1, 4095) < Span.pack(2, 0))
    assert(Span.pack(7, 3) < Span.pack(7, 4))

  test("Span.contains is end-inclusive at boundaries"):
    val s = Span(2, 4, 2, 10)
    assert(s.contains(2, 4))
    assert(s.contains(2, 10))
    assert(!s.contains(2, 3))
    assert(!s.contains(2, 11))
    assert(!s.contains(1, 5))
    assert(!s.contains(3, 5))
