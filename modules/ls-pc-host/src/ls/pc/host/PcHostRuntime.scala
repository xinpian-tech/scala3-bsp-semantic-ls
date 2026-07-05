package ls.pc.host

import java.lang.foreign.{MemorySegment, ValueLayout}
import java.nio.charset.StandardCharsets.UTF_8

import ls.pc.host.boundary.{boundary_h, AllocFn, LsBuf, LsStr, RustVtable}
import ls.pc.host.codec.CodecException

/** Allocates Rust-owned response buffers. `alloc(size)` returns an
  * address-only segment of `size` bytes, or a NULL-address segment on failure;
  * the consumer (Rust) later frees it. Abstracted so the boundary marshalling
  * can be unit-tested against an in-arena allocator without the real vtable.
  */
trait LsAllocator:
  def alloc(size: Int): MemorySegment

/** The island-side boundary runtime: owns the `alloc` downcall built from the
  * registered Rust vtable and marshals bytes across the FFM boundary for the 15
  * PC upcall stubs. Constructed once during premain after registration; the op
  * wiring (a later slice) routes every request/response through it.
  */
final class PcHostRuntime(allocator: LsAllocator):
  import PcHostRuntime.writeResponse

  /** Runs a query op that produces an encoded codec response and writes it into
    * the caller's Rust-owned `out` buffer. A `CodecException` (a malformed
    * request or response) maps to `STATUS_DECODE` and any other throwable to
    * `STATUS_INTERNAL`, so nothing escapes across the native boundary.
    */
  def runResponse(out: MemorySegment)(body: => Array[Byte]): Int =
    try writeResponse(out, body, allocator)
    catch
      case _: CodecException => boundary_h.STATUS_DECODE()
      case _: Throwable => boundary_h.STATUS_INTERNAL()

  /** Runs a lifecycle op that returns no payload: `STATUS_OK` on success,
    * `STATUS_DECODE` on a malformed request, `STATUS_INTERNAL` otherwise.
    */
  def runStatus(body: => Unit): Int =
    try
      body
      boundary_h.STATUS_OK()
    catch
      case _: CodecException => boundary_h.STATUS_DECODE()
      case _: Throwable => boundary_h.STATUS_INTERNAL()

object PcHostRuntime:
  /** Builds the runtime from the registered Rust vtable, wiring the allocator to
    * the real `alloc` slot. The slot returns an address-only segment; a NULL
    * address means the allocation failed.
    */
  def fromVtable(vtable: MemorySegment): PcHostRuntime =
    val allocFn = RustVtable.alloc(vtable)
    PcHostRuntime(size => AllocFn.invoke(allocFn, size))

  /** Reads a borrowed `LsStr` argument (UTF-8, no NUL) into a Scala string. */
  def readLsStr(struct: MemorySegment): String =
    val len = LsStr.len(struct)
    if len <= 0 then ""
    else
      val bytes = LsStr.ptr(struct).reinterpret(len.toLong).toArray(ValueLayout.JAVA_BYTE)
      String(bytes, UTF_8)

  /** Reads a borrowed request-payload buffer (`ptr`, `len`) into a byte array. */
  def readRequest(ptr: MemorySegment, len: Int): Array[Byte] =
    if len <= 0 then Array.emptyByteArray
    else ptr.reinterpret(len.toLong).toArray(ValueLayout.JAVA_BYTE)

  /** Writes `bytes` into a freshly `alloc`-ed Rust-owned buffer and points the
    * caller's `out` `LsBuf` at it, following the boundary memory protocol:
    * measure the length, call `alloc` exactly once, copy, then set
    * `LsBuf.ptr`/`LsBuf.len`. A failed allocation returns `STATUS_ALLOC`
    * without touching `out`. (Codec responses always carry the 16-byte
    * envelope, so `bytes` is never empty.)
    */
  def writeResponse(out: MemorySegment, bytes: Array[Byte], allocator: LsAllocator): Int =
    val raw = allocator.alloc(bytes.length)
    if raw.address() == 0 then boundary_h.STATUS_ALLOC()
    else
      val dst = raw.reinterpret(bytes.length.toLong)
      MemorySegment.copy(bytes, 0, dst, ValueLayout.JAVA_BYTE, 0L, bytes.length)
      LsBuf.ptr(out, raw)
      LsBuf.len(out, bytes.length)
      boundary_h.STATUS_OK()
