package ls.semanticdb

import java.io.ByteArrayOutputStream
import java.nio.charset.StandardCharsets

/** Tiny protobuf wire-format ENCODER, test-only. Used to round-trip
  * SemanticDB messages through the production decoder, including unknown
  * fields of every wire type.
  */
final class ProtoTestWriter:
  private val out = new ByteArrayOutputStream()

  def bytes: Array[Byte] = out.toByteArray

  def writeRawVarint(value: Long): this.type =
    var v = value
    var continue = true
    while continue do
      val b = (v & 0x7f).toInt
      v = v >>> 7
      if v == 0L then
        out.write(b)
        continue = false
      else out.write(b | 0x80)
    this

  def writeTag(field: Int, wireType: Int): this.type =
    writeRawVarint((field.toLong << 3) | wireType.toLong)

  def varintField(field: Int, value: Long): this.type =
    writeTag(field, 0)
    writeRawVarint(value)

  /** Negative int32 values sign-extend to 10-byte varints, per proto spec. */
  def int32Field(field: Int, value: Int): this.type =
    varintField(field, value.toLong)

  def fixed64Field(field: Int, value: Long): this.type =
    writeTag(field, 1)
    var i = 0
    while i < 8 do
      out.write(((value >>> (8 * i)) & 0xff).toInt)
      i += 1
    this

  def fixed32Field(field: Int, value: Int): this.type =
    writeTag(field, 5)
    var i = 0
    while i < 4 do
      out.write((value >>> (8 * i)) & 0xff)
      i += 1
    this

  def bytesField(field: Int, data: Array[Byte]): this.type =
    writeTag(field, 2)
    writeRawVarint(data.length.toLong)
    out.write(data, 0, data.length)
    this

  def stringField(field: Int, value: String): this.type =
    bytesField(field, value.getBytes(StandardCharsets.UTF_8))

  def messageField(field: Int)(build: ProtoTestWriter => Unit): this.type =
    val nested = new ProtoTestWriter
    build(nested)
    bytesField(field, nested.bytes)

  /** Legacy group encoding (wire types 3/4), only produced by ancient
    * encoders; the decoder must be able to skip it as an unknown field.
    */
  def groupField(field: Int)(build: ProtoTestWriter => Unit): this.type =
    writeTag(field, 3)
    build(this)
    writeTag(field, 4)

/** Encodes the SemanticDB message subset (and optional unknown-field noise)
  * with the field numbers of scalameta semanticdb.proto.
  */
object ProtoTestEncoder:

  def writeRange(w: ProtoTestWriter, r: SdbRange): Unit =
    w.int32Field(1, r.startLine)
    w.int32Field(2, r.startCharacter)
    w.int32Field(3, r.endLine)
    w.int32Field(4, r.endCharacter)

  def writeOccurrence(w: ProtoTestWriter, o: SdbOccurrence, noise: Boolean): Unit =
    o.range.foreach(r => w.messageField(1)(writeRange(_, r)))
    if noise then w.fixed32Field(90, 0xdeadbeef)
    w.stringField(2, o.symbol)
    if noise then w.groupField(91)(_.varintField(1, 7L))
    w.varintField(3, o.roleCode.toLong)

  def writeSymbolInfo(w: ProtoTestWriter, s: SdbSymbolInfo, noise: Boolean): Unit =
    w.stringField(1, s.symbol)
    if noise then
      // signature (17) and access (18) are real fields we intentionally skip
      w.messageField(17)(_.messageField(2)(_.messageField(3)(_.stringField(2, "scala/Int#"))))
      w.messageField(18)(_.messageField(7)(_ => ()))
    w.varintField(3, s.kindCode.toLong)
    w.int32Field(4, s.properties)
    w.stringField(5, s.displayName)
    s.overriddenSymbols.foreach(o => w.stringField(19, o))
    if noise then w.varintField(16, 1L) // language

  def writeDocument(w: ProtoTestWriter, d: SdbDocument, noise: Boolean): Unit =
    w.varintField(1, d.schema.toLong)
    w.stringField(2, d.uri)
    w.stringField(3, d.text)
    if noise then
      // Diagnostic payload (field 7): must be skipped without breaking.
      w.messageField(7) { dw =>
        dw.messageField(1)(writeRange(_, SdbRange(1, 2, 3, 4)))
        dw.varintField(2, 2L)
        dw.stringField(3, "unused diagnostic")
      }
    w.stringField(11, d.md5)
    w.varintField(10, d.languageCode.toLong)
    d.symbols.foreach(s => w.messageField(5)(writeSymbolInfo(_, s, noise)))
    d.occurrences.foreach(o => w.messageField(6)(writeOccurrence(_, o, noise)))
    if noise then
      // Synthetic payload (field 12) and build_target (13), plus unknown
      // fields with large numbers and every wire type.
      w.messageField(12)(sw => sw.messageField(1)(writeRange(_, SdbRange(0, 0, 0, 1))))
      w.stringField(13, "build-target")
      w.varintField(98, Long.MaxValue)
      w.fixed64Field(99, Long.MinValue)
      w.varintField(100, -1L) // 10-byte varint

  def encode(docs: Seq[SdbDocument], noise: Boolean = false): Array[Byte] =
    val w = new ProtoTestWriter
    if noise then w.varintField(2, 42L) // unknown field in TextDocuments
    docs.foreach(d => w.messageField(1)(writeDocument(_, d, noise)))
    if noise then w.bytesField(55, Array[Byte](1, 2, 3))
    w.bytes
