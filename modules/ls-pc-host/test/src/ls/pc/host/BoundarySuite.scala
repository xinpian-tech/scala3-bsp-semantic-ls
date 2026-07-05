package ls.pc.host

import java.lang.foreign.{Arena, MemorySegment, ValueLayout}
import java.nio.charset.StandardCharsets.UTF_8

import ls.pc.host.boundary.{boundary_h, LsBuf, LsStr}
import ls.pc.host.codec.{CodecException, Payloads}
import ls.pc.host.codec.Payloads.{Location, LocationsResult, Rng}

/** The FFM boundary-marshalling layer, exercised against an in-arena fake
  * allocator (a stand-in for the Rust `alloc` vtable slot) so the alloc-once /
  * copy / status behaviour and the `LsStr`/payload round-trips are verified
  * without a live PC facade or a booted JVM.
  */
class BoundarySuite extends munit.FunSuite:

  private def arenaAllocator(arena: Arena): LsAllocator =
    size => arena.allocate(size.toLong)

  private val failingAllocator: LsAllocator = _ => MemorySegment.NULL

  private def bytesOf(seg: MemorySegment, len: Int): Array[Byte] =
    seg.reinterpret(len.toLong).toArray(ValueLayout.JAVA_BYTE)

  test("writeResponse allocs once and the LsBuf bytes decode back to the payload"):
    val arena = Arena.ofConfined()
    try
      val payload = LocationsResult(
        Seq(Location("file:///A.scala", Rng(1, 2, 3, 4), Payloads.Origin.Workspace))
      ).encode()
      val out = LsBuf.allocate(arena)
      assertEquals(PcHostRuntime.writeResponse(out, payload, arenaAllocator(arena)), boundary_h.STATUS_OK())
      val len = LsBuf.len(out)
      assertEquals(len, payload.length)
      val got = bytesOf(LsBuf.ptr(out), len)
      assertEquals(got.toList, payload.toList)
      // The buffer the consumer sees decodes back to the original result.
      assertEquals(LocationsResult.decode(got).locations.length, 1)
    finally arena.close()

  test("a failed allocation returns STATUS_ALLOC"):
    val arena = Arena.ofConfined()
    try
      val out = LsBuf.allocate(arena)
      assertEquals(
        PcHostRuntime.writeResponse(out, Array[Byte](1, 2, 3), failingAllocator),
        boundary_h.STATUS_ALLOC()
      )
    finally arena.close()

  test("writeResponse rejects a null out before allocating (no buffer leaked)"):
    val arena = Arena.ofConfined()
    try
      var allocs = 0
      val counting = new LsAllocator:
        def alloc(size: Int): MemorySegment =
          allocs += 1
          arena.allocate(size.toLong)
      assertEquals(
        PcHostRuntime.writeResponse(MemorySegment.NULL, Array[Byte](1, 2, 3), counting),
        boundary_h.STATUS_BAD_ARG()
      )
      assertEquals(allocs, 0, "a null out must not allocate a buffer")
    finally arena.close()

  test("writeResponse frees the allocated buffer when writing back into out fails"):
    val arena = Arena.ofConfined()
    try
      var freed = List.empty[Int]
      val recording = new LsAllocator:
        def alloc(size: Int): MemorySegment = arena.allocate(size.toLong)
        override def free(ptr: MemorySegment, size: Int): Unit = freed = size :: freed
      // An `out` smaller than an LsBuf (its `ptr` field needs 8 bytes at offset 0)
      // makes the post-allocation write throw, exercising the eager-free path.
      val tooSmall = arena.allocate(4L)
      intercept[RuntimeException] {
        PcHostRuntime.writeResponse(tooSmall, Array[Byte](1, 2, 3, 4, 5), recording)
      }
      assertEquals(freed, List(5), "the Rust-owned buffer is freed on the error path")
    finally arena.close()

  test("runResponse maps a decode failure to STATUS_DECODE"):
    val arena = Arena.ofConfined()
    try
      val rt = PcHostRuntime(arenaAllocator(arena))
      val out = LsBuf.allocate(arena)
      assertEquals(rt.runResponse(out)(throw CodecException("bad")), boundary_h.STATUS_DECODE())
    finally arena.close()

  test("runResponse maps an unexpected throwable to STATUS_INTERNAL"):
    val arena = Arena.ofConfined()
    try
      val rt = PcHostRuntime(arenaAllocator(arena))
      val out = LsBuf.allocate(arena)
      assertEquals(rt.runResponse(out)(throw RuntimeException("boom")), boundary_h.STATUS_INTERNAL())
    finally arena.close()

  test("runStatus returns OK on success and typed errors on failure"):
    val arena = Arena.ofConfined()
    try
      val rt = PcHostRuntime(arenaAllocator(arena))
      assertEquals(rt.runStatus(()), boundary_h.STATUS_OK())
      assertEquals(rt.runStatus(throw CodecException("bad")), boundary_h.STATUS_DECODE())
      assertEquals(rt.runStatus(throw RuntimeException("boom")), boundary_h.STATUS_INTERNAL())
    finally arena.close()

  test("readLsStr round-trips a UTF-8 argument"):
    val arena = Arena.ofConfined()
    try
      val text = "file:///café.scala"
      val bytes = text.getBytes(UTF_8)
      val buf = arena.allocate(bytes.length.toLong)
      MemorySegment.copy(bytes, 0, buf, ValueLayout.JAVA_BYTE, 0L, bytes.length)
      val struct = LsStr.allocate(arena)
      LsStr.ptr(struct, buf)
      LsStr.len(struct, bytes.length)
      assertEquals(PcHostRuntime.readLsStr(struct), text)
    finally arena.close()

  test("readRequest round-trips a payload buffer"):
    val arena = Arena.ofConfined()
    try
      val payload = LocationsResult(Seq.empty).encode()
      val buf = arena.allocate(payload.length.toLong)
      MemorySegment.copy(payload, 0, buf, ValueLayout.JAVA_BYTE, 0L, payload.length)
      assertEquals(PcHostRuntime.readRequest(buf, payload.length).toList, payload.toList)
    finally arena.close()

  test("a signed-negative LsStr length is a typed decode error (not empty)"):
    val arena = Arena.ofConfined()
    try
      val rt = PcHostRuntime(arenaAllocator(arena))
      // A high-bit u32 length surfaces as a negative Int through jextract.
      val struct = LsStr.allocate(arena)
      LsStr.len(struct, -1)
      assertEquals(rt.runStatus { PcHostRuntime.readLsStr(struct); () }, boundary_h.STATUS_DECODE())
    finally arena.close()

  test("a null LsStr pointer with a positive length is a typed decode error"):
    val arena = Arena.ofConfined()
    try
      val rt = PcHostRuntime(arenaAllocator(arena))
      val struct = LsStr.allocate(arena)
      LsStr.len(struct, 5)
      LsStr.ptr(struct, MemorySegment.NULL)
      assertEquals(rt.runStatus { PcHostRuntime.readLsStr(struct); () }, boundary_h.STATUS_DECODE())
    finally arena.close()

  test("a negative request length is a typed decode error"):
    val arena = Arena.ofConfined()
    try
      val rt = PcHostRuntime(arenaAllocator(arena))
      assertEquals(
        rt.runStatus { PcHostRuntime.readRequest(MemorySegment.NULL, -1); () },
        boundary_h.STATUS_DECODE()
      )
    finally arena.close()

  test("a null request pointer with a positive length maps to STATUS_DECODE via runResponse"):
    val arena = Arena.ofConfined()
    try
      val rt = PcHostRuntime(arenaAllocator(arena))
      val out = LsBuf.allocate(arena)
      val status = rt.runResponse(out) {
        PcHostRuntime.readRequest(MemorySegment.NULL, 8)
        Array.emptyByteArray
      }
      assertEquals(status, boundary_h.STATUS_DECODE())
    finally arena.close()
