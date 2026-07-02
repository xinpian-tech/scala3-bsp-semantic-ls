package ls.semanticdb

import java.nio.charset.StandardCharsets

/** Raised when a `.semanticdb` payload is not decodable protobuf wire data. */
final class SemanticdbParseException(message: String) extends RuntimeException(message)

/** Minimal protobuf wire-format reader over a byte-array slice. Zero external
  * dependencies by design: the SemanticDB subset this project consumes is
  * decoded by hand against field numbers taken verbatim from
  * scalameta/semanticdb.proto (see [[SemanticdbParser]]).
  *
  * Wire types (protobuf encoding spec):
  *   - 0 varint
  *   - 1 fixed64
  *   - 2 length-delimited
  *   - 3 group start (legacy, skipped recursively)
  *   - 4 group end (legacy)
  *   - 5 fixed32
  */
private[semanticdb] final class ProtoReader(
    bytes: Array[Byte],
    start: Int,
    endExclusive: Int
):
  def this(bytes: Array[Byte]) = this(bytes, 0, bytes.length)

  private var pos = start

  def hasRemaining: Boolean = pos < endExclusive

  private def fail(message: String): Nothing =
    throw SemanticdbParseException(s"$message (at byte offset $pos)")

  /** Base-128 varint, at most 10 bytes. Bits above 63 are dropped, matching
    * standard protobuf truncation semantics.
    */
  def readVarint(): Long =
    var shift = 0
    var result = 0L
    while true do
      if shift >= 64 then fail("malformed varint (more than 10 bytes)")
      if pos >= endExclusive then fail("truncated varint")
      val b = bytes(pos)
      pos += 1
      result |= (b & 0x7fL) << shift
      if (b & 0x80) == 0 then return result
      shift += 7
    result // unreachable

  /** int32 fields are 64-bit varints truncated to Int (protobuf semantics:
    * negative int32 values are sign-extended 10-byte varints).
    */
  def readInt32(): Int = readVarint().toInt

  def readTag(): Int =
    val raw = readVarint()
    if raw <= 0 || raw > Int.MaxValue then fail(s"tag out of range: $raw")
    val tag = raw.toInt
    if (tag >>> 3) == 0 then fail("field number 0 is invalid")
    tag

  private def readLengthDelimitedSlice(): (Int, Int) =
    val len = readVarint()
    if len < 0 || len > (endExclusive - pos).toLong then
      fail(s"truncated length-delimited field (declared $len bytes)")
    val off = pos
    pos += len.toInt
    (off, len.toInt)

  def readString(): String =
    val (off, len) = readLengthDelimitedSlice()
    new String(bytes, off, len, StandardCharsets.UTF_8)

  /** Sub-reader over one embedded message; skips the payload in this reader
    * without copying bytes.
    */
  def readMessage(): ProtoReader =
    val (off, len) = readLengthDelimitedSlice()
    new ProtoReader(bytes, off, off + len)

  private def skipBytes(n: Int): Unit =
    if n > endExclusive - pos then fail(s"truncated field ($n bytes expected)")
    pos += n

  /** Skips one field payload of the given wire type. Unknown fields of every
    * wire type are supported, so schema evolution never breaks decoding.
    */
  def skipField(wireType: Int, fieldNumber: Int): Unit = wireType match
    case 0 =>
      readVarint()
      ()
    case 1 => skipBytes(8)
    case 2 =>
      readLengthDelimitedSlice()
      ()
    case 3 =>
      // Legacy group: skip nested fields until the matching end-group tag.
      var done = false
      while !done do
        val tag = readTag()
        val wt = tag & 7
        val fn = tag >>> 3
        if wt == 4 then
          if fn != fieldNumber then fail(s"mismatched end-group for field $fieldNumber")
          done = true
        else skipField(wt, fn)
    case 4 => fail("unexpected end-group tag")
    case 5 => skipBytes(4)
    case other => fail(s"unsupported wire type $other")
