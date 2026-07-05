package ls.pc.host.codec

import java.io.ByteArrayOutputStream
import java.nio.charset.StandardCharsets.UTF_8

/** A payload failed to decode (or encode). The boundary maps this to a typed
  * `STATUS_DECODE` rather than letting a malformed buffer crash the island.
  */
final class CodecException(message: String) extends RuntimeException(message)

/** Flat little-endian codec for the variable op payloads — the island-side
  * byte-for-byte mirror of the Rust `ls-pc-abi` `codec` module.
  *
  * A payload is a 16-byte envelope (`magic`, `kind`, `body_len`, `blob_len`), a
  * body of fixed-width 4-byte records (`u32`/`i32`, blob-referenced strings as
  * `offset,len`, count-prefixed lists), and a trailing UTF-8 string blob. Every
  * record field is 4 bytes, so a record's byte image equals its `#[repr(C)]`
  * layout. The [[Reader]] bounds-checks every field and blob slice, so a
  * malformed buffer yields a [[CodecException]] rather than an out-of-bounds
  * read.
  */
object Codec:
  /** The envelope magic (`"LPAB"` little-endian). */
  val Magic: Int = 0x4241504c

  /** Builds a payload buffer: fixed records into `body`, strings/opaque bytes
    * into `blob`, then `finish` prepends the envelope.
    */
  final class Writer:
    private val body = ByteArrayOutputStream()
    private val blob = ByteArrayOutputStream()

    def u32(v: Int): Unit =
      body.write(v & 0xff)
      body.write((v >>> 8) & 0xff)
      body.write((v >>> 16) & 0xff)
      body.write((v >>> 24) & 0xff)

    def i32(v: Int): Unit = u32(v)

    def bool32(v: Boolean): Unit = u32(if v then 1 else 0)

    /** A required string: a `BlobStr` (`offset`, `len`) into the blob. */
    def str(s: String): Unit =
      val (offset, len) = intern(s.getBytes(UTF_8))
      u32(offset)
      u32(len)

    /** An optional string: a presence flag then a `BlobStr`. `None` and
      * `Some("")` are distinct (`0` vs `1` present).
      */
    def optStr(s: Option[String]): Unit = s match
      case Some(v) =>
        u32(1)
        str(v)
      case None =>
        u32(0)
        u32(0)
        u32(0)

    /** An optional opaque byte payload (e.g. a completion item's `data`). */
    def optBytes(b: Option[Array[Byte]]): Unit = b match
      case Some(v) =>
        u32(1)
        val (offset, len) = intern(v)
        u32(offset)
        u32(len)
      case None =>
        u32(0)
        u32(0)
        u32(0)

    /** An optional `i32`: a presence flag then the value. `None` and `Some(0)`
      * are distinct.
      */
    def optI32(v: Option[Int]): Unit = v match
      case Some(x) =>
        u32(1)
        i32(x)
      case None =>
        u32(0)
        i32(0)

    /** An optional bool: a presence flag then the value. `None` and
      * `Some(false)` are distinct.
      */
    def optBool(v: Option[Boolean]): Unit = v match
      case Some(x) =>
        u32(1)
        bool32(x)
      case None =>
        u32(0)
        u32(0)

    /** A flattened `[start, end)` range: four `u32`s. */
    def range(startLine: Int, startCharacter: Int, endLine: Int, endCharacter: Int): Unit =
      u32(startLine)
      u32(startCharacter)
      u32(endLine)
      u32(endCharacter)

    /** A count-prefixed list of required strings. */
    def strList(list: Seq[String]): Unit =
      u32(list.length)
      list.foreach(str)

    private def intern(bytes: Array[Byte]): (Int, Int) =
      val offset = blob.size()
      blob.write(bytes, 0, bytes.length)
      (offset, bytes.length)

    /** Concatenates the envelope, body, and blob into the final buffer. */
    def finish(kind: Int): Array[Byte] =
      val bodyBytes = body.toByteArray
      val blobBytes = blob.toByteArray
      val out = ByteArrayOutputStream(16 + bodyBytes.length + blobBytes.length)
      writeLe(out, Magic)
      writeLe(out, kind)
      writeLe(out, bodyBytes.length)
      writeLe(out, blobBytes.length)
      out.write(bodyBytes, 0, bodyBytes.length)
      out.write(blobBytes, 0, blobBytes.length)
      out.toByteArray

    private def writeLe(out: ByteArrayOutputStream, v: Int): Unit =
      out.write(v & 0xff)
      out.write((v >>> 8) & 0xff)
      out.write((v >>> 16) & 0xff)
      out.write((v >>> 24) & 0xff)

  /** Reads a payload buffer produced by [[Writer]]. */
  final class Reader private (
      buf: Array[Byte],
      bodyStart: Int,
      bodyLen: Int,
      blobStart: Int,
      blobLen: Int
  ):
    private var pos: Int = 0

    private def take(n: Int): Int =
      val end = pos + n
      if n < 0 || end < 0 then throw CodecException("body cursor overflow")
      if end > bodyLen then throw CodecException("body underrun")
      val at = bodyStart + pos
      pos = end
      at

    def u32(): Int =
      val at = take(4)
      (buf(at) & 0xff) |
        ((buf(at + 1) & 0xff) << 8) |
        ((buf(at + 2) & 0xff) << 16) |
        ((buf(at + 3) & 0xff) << 24)

    def i32(): Int = u32()

    def bool32(): Boolean = u32() != 0

    /** Reads a count and guards it against the remaining body: each element is
      * at least 4 bytes, so a fabricated huge count is rejected before any
      * allocation.
      */
    def count(): Int =
      val n = u32()
      val remaining = bodyLen - pos
      if n < 0 || n > remaining / 4 then
        throw CodecException(s"list count $n exceeds the remaining body")
      n

    private def blobSlice(offset: Int, len: Int): String =
      val end = offset + len
      if offset < 0 || len < 0 || end < 0 || end > blobLen then
        throw CodecException("blob slice out of range")
      String(buf, blobStart + offset, len, UTF_8)

    private def blobBytes(offset: Int, len: Int): Array[Byte] =
      val end = offset + len
      if offset < 0 || len < 0 || end < 0 || end > blobLen then
        throw CodecException("blob slice out of range")
      java.util.Arrays.copyOfRange(buf, blobStart + offset, blobStart + end)

    def str(): String =
      val offset = u32()
      val len = u32()
      blobSlice(offset, len)

    def optStr(): Option[String] =
      val present = u32()
      val offset = u32()
      val len = u32()
      if present == 0 then None else Some(blobSlice(offset, len))

    def optBytes(): Option[Array[Byte]] =
      val present = u32()
      val offset = u32()
      val len = u32()
      if present == 0 then None else Some(blobBytes(offset, len))

    def optI32(): Option[Int] =
      val present = u32()
      val value = i32()
      if present == 0 then None else Some(value)

    def optBool(): Option[Boolean] =
      val present = u32()
      val value = bool32()
      if present == 0 then None else Some(value)

    /** Reads a flattened range as `(startLine, startCharacter, endLine, endCharacter)`. */
    def range(): (Int, Int, Int, Int) = (u32(), u32(), u32(), u32())

    /** Reads a count-prefixed list of required strings. */
    def strList(): Seq[String] =
      val n = count()
      val out = Vector.newBuilder[String]
      var i = 0
      while i < n do
        out += str()
        i += 1
      out.result()

    /** Requires the body to be fully consumed (no trailing garbage). */
    def finish(): Unit =
      if pos != bodyLen then throw CodecException(s"${bodyLen - pos} trailing body bytes")

  object Reader:
    /** Validates the envelope (magic, kind, exact `16 + body_len + blob_len`
      * length) and splits the buffer into its body and blob regions.
      */
    def apply(buf: Array[Byte], expectedKind: Int): Reader =
      if buf.length < 16 then throw CodecException("buffer shorter than the 16-byte envelope")
      val magic = readLe(buf, 0)
      if magic != Magic then throw CodecException(f"bad magic 0x$magic%08x")
      val kind = readLe(buf, 4)
      if kind != expectedKind then
        throw CodecException(s"payload kind $kind != expected $expectedKind")
      val bodyLen = readLe(buf, 8)
      val blobLen = readLe(buf, 12)
      if bodyLen < 0 || blobLen < 0 then throw CodecException("negative length")
      val total = 16L + bodyLen.toLong + blobLen.toLong
      if total != buf.length.toLong then
        throw CodecException(s"declared length $total != actual ${buf.length}")
      new Reader(buf, 16, bodyLen, 16 + bodyLen, blobLen)

    private def readLe(buf: Array[Byte], at: Int): Int =
      (buf(at) & 0xff) |
        ((buf(at + 1) & 0xff) << 8) |
        ((buf(at + 2) & 0xff) << 16) |
        ((buf(at + 3) & 0xff) << 24)
