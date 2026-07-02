package ls.semanticdb

class WireDecoderSuite extends munit.FunSuite:

  private val doc1 = SdbDocument(
    schema = 4,
    uri = "src/A.scala",
    text = "object A:\n  def f = 1\n",
    md5 = "ABCDEF0123456789ABCDEF0123456789",
    languageCode = SdbLanguage.Scala,
    symbols = Vector(
      SdbSymbolInfo("a/A.", 10, 0x8 | 0x4000, "A", Vector.empty),
      // multi-byte varint properties: OPAQUE | INLINE = 0x220000
      SdbSymbolInfo("a/A.f().", 3, 0x200000 | 0x20000, "f", Vector("a/B#f().", "a/C#f()."))
    ),
    occurrences = Vector(
      SdbOccurrence(Some(SdbRange(0, 7, 0, 8)), "a/A.", SdbRole.Definition),
      // multi-byte varint line numbers
      SdbOccurrence(Some(SdbRange(12345, 40, 12345, 45)), "a/A.f().", SdbRole.Reference),
      SdbOccurrence(None, "a/NoRange#", SdbRole.Reference),
      SdbOccurrence(Some(SdbRange(0, 0, 0, 0)), "local0", SdbRole.Definition)
    )
  )

  private val doc2 = SdbDocument(
    schema = 3,
    uri = "src/B.scala",
    text = "",
    md5 = "",
    languageCode = SdbLanguage.Java,
    symbols = Vector.empty,
    occurrences = Vector.empty
  )

  test("round-trips a TextDocuments payload"):
    val bytes = ProtoTestEncoder.encode(Seq(doc1, doc2))
    val parsed = SemanticdbParser.parseTextDocuments(bytes)
    assertEquals(parsed, SdbDocuments(Vector(doc1, doc2)))

  test("skips unknown fields of every wire type, diagnostics and synthetics"):
    val noisy = ProtoTestEncoder.encode(Seq(doc1, doc2), noise = true)
    val plain = ProtoTestEncoder.encode(Seq(doc1, doc2), noise = false)
    assert(noisy.length > plain.length, "noise must actually add bytes")
    val parsed = SemanticdbParser.parseTextDocuments(noisy)
    assertEquals(parsed, SdbDocuments(Vector(doc1, doc2)))

  test("decodes negative int32 range values (10-byte varints, no zigzag)"):
    // Range fields are plain int32 in semanticdb.proto, so -1 rides as a
    // 10-byte sign-extended varint and must decode back to -1 (not zigzag 0).
    val doc = doc2.copy(occurrences =
      Vector(SdbOccurrence(Some(SdbRange(-1, 0, -1, 5)), "a/X#", SdbRole.Reference))
    )
    val parsed = SemanticdbParser.parseTextDocuments(ProtoTestEncoder.encode(Seq(doc)))
    assertEquals(parsed.documents.head.occurrences.head.range, Some(SdbRange(-1, 0, -1, 5)))

  test("empty embedded Range message decodes to all zeros"):
    val w = new ProtoTestWriter
    w.messageField(1) { dw =>
      dw.stringField(2, "u.scala")
      dw.messageField(6) { ow =>
        ow.messageField(1)(_ => ()) // Range present but empty
        ow.stringField(2, "a/X#")
        ow.varintField(3, 1L)
      }
    }
    val parsed = SemanticdbParser.parseTextDocuments(w.bytes)
    assertEquals(
      parsed.documents.head.occurrences,
      Vector(SdbOccurrence(Some(SdbRange(0, 0, 0, 0)), "a/X#", SdbRole.Reference))
    )

  test("empty payload decodes to zero documents"):
    assertEquals(SemanticdbParser.parseTextDocuments(Array.emptyByteArray), SdbDocuments(Vector.empty))

  test("proto3 defaults: absent fields keep zero values"):
    val w = new ProtoTestWriter
    w.messageField(1)(_ => ()) // fully empty TextDocument
    val parsed = SemanticdbParser.parseTextDocuments(w.bytes)
    assertEquals(parsed.documents, Vector(SdbDocument(0, "", "", "", 0, Vector.empty, Vector.empty)))

  test("truncated varint fails with SemanticdbParseException"):
    // tag says field 1 varint, then a continuation byte with no terminator
    val bytes = Array[Byte](0x08, 0x80.toByte)
    intercept[SemanticdbParseException](SemanticdbParser.parseTextDocuments(bytes))

  test("over-long varint fails"):
    // tag (field 1, varint) followed by 10 continuation bytes + terminator:
    // one byte longer than the longest legal varint
    val bytes = Array[Byte](0x08) ++ Array.fill(10)(0x80.toByte) ++ Array[Byte](0x01)
    intercept[SemanticdbParseException](SemanticdbParser.parseTextDocuments(bytes))

  test("truncated length-delimited field fails"):
    val w = new ProtoTestWriter
    w.writeTag(1, 2)
    w.writeRawVarint(100L) // declares 100 bytes, provides none
    intercept[SemanticdbParseException](SemanticdbParser.parseTextDocuments(w.bytes))

  test("multi-byte varint boundary values survive skipping"):
    // Unknown varint field with Long.MaxValue and Long.MinValue around real data
    val w = new ProtoTestWriter
    w.varintField(77, Long.MaxValue)
    w.messageField(1)(dw => dw.stringField(2, "x.scala"))
    w.varintField(78, Long.MinValue)
    val parsed = SemanticdbParser.parseTextDocuments(w.bytes)
    assertEquals(parsed.documents.map(_.uri), Vector("x.scala"))
