package ls.pc.host

import java.lang.foreign.{MemorySegment, ValueLayout}
import java.nio.charset.StandardCharsets.UTF_8

import ls.pc.host.boundary.{boundary_h, AllocFn, FreeFn, LsBuf, LsStr, RustVtable}
import ls.pc.host.codec.CodecException

/** Allocates Rust-owned response buffers. `alloc(size)` returns an
  * address-only segment of `size` bytes, or a NULL-address segment on failure;
  * the consumer (Rust) later frees it. Abstracted so the boundary marshalling
  * can be unit-tested against an in-arena allocator without the real vtable.
  */
trait LsAllocator:
  def alloc(size: Int): MemorySegment

  /** Frees a buffer obtained from [[alloc]]. The default is a no-op for
    * arena-backed allocators (the arena releases the memory on close); the real
    * vtable allocator overrides it to call the Rust `free` slot, so a failure
    * after a successful allocation can release the Rust-owned buffer eagerly
    * instead of leaking it.
    */
  def free(ptr: MemorySegment, size: Int): Unit = ()

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
    val freeFn = RustVtable.free(vtable)
    PcHostRuntime(new LsAllocator:
      def alloc(size: Int): MemorySegment = AllocFn.invoke(allocFn, size)
      override def free(ptr: MemorySegment, size: Int): Unit = FreeFn.invoke(freeFn, ptr, size)
    )

  /** Reads a borrowed `LsStr` argument (UTF-8, no NUL) into a Scala string. The
    * ABI `len` is a `u32` that jextract surfaces as a signed `Int`, so a
    * high-bit length arrives negative: reject it (and a null pointer with a
    * positive length) as a typed decode error rather than accessing foreign
    * memory. A zero length is a valid empty string.
    */
  def readLsStr(struct: MemorySegment): String =
    val len = LsStr.len(struct)
    if len < 0 then throw CodecException(s"negative LsStr length $len")
    if len == 0 then ""
    else
      val ptr = LsStr.ptr(struct)
      if ptr.address() == 0 then throw CodecException("null LsStr pointer with positive length")
      String(ptr.reinterpret(len.toLong).toArray(ValueLayout.JAVA_BYTE), UTF_8)

  /** Reads a borrowed request-payload buffer (`ptr`, `len`) into a byte array,
    * rejecting a signed-negative length or a null pointer with a positive
    * length (see [[readLsStr]]). A zero length is a valid empty payload.
    */
  def readRequest(ptr: MemorySegment, len: Int): Array[Byte] =
    if len < 0 then throw CodecException(s"negative request length $len")
    if len == 0 then Array.emptyByteArray
    else
      if ptr.address() == 0 then throw CodecException("null request pointer with positive length")
      ptr.reinterpret(len.toLong).toArray(ValueLayout.JAVA_BYTE)

  /** Writes `bytes` into a freshly `alloc`-ed Rust-owned buffer and points the
    * caller's `out` `LsBuf` at it, following the boundary memory protocol:
    * measure the length, call `alloc` exactly once, copy, then set
    * `LsBuf.ptr`/`LsBuf.len`. A failed allocation returns `STATUS_ALLOC`
    * without touching `out`. (Codec responses always carry the 16-byte
    * envelope, so `bytes` is never empty.)
    */
  def writeResponse(out: MemorySegment, bytes: Array[Byte], allocator: LsAllocator): Int =
    // Reject a null `out` before allocating, so a buffer is never allocated for a
    // caller we cannot write back to (which would leak it).
    if out == null || out.address() == 0 then boundary_h.STATUS_BAD_ARG()
    else
      val raw = allocator.alloc(bytes.length)
      if raw.address() == 0 then boundary_h.STATUS_ALLOC()
      else
        try
          val dst = raw.reinterpret(bytes.length.toLong)
          MemorySegment.copy(bytes, 0, dst, ValueLayout.JAVA_BYTE, 0L, bytes.length)
          LsBuf.ptr(out, raw)
          LsBuf.len(out, bytes.length)
          boundary_h.STATUS_OK()
        catch
          case t: Throwable =>
            // The buffer is Rust-allocated; free it eagerly so a failure after
            // allocation (e.g. writing into a bad `out`) does not leak it, then
            // let the op's status mapping handle the throwable.
            allocator.free(raw, bytes.length)
            throw t
