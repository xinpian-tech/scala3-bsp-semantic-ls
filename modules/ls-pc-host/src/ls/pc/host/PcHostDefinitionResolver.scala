package ls.pc.host

import java.lang.foreign.{Arena, MemorySegment, ValueLayout}
import java.nio.charset.StandardCharsets.UTF_8

import ls.pc.PcDefinitionResolver
import ls.pc.host.boundary.{boundary_h, FreeFn, LsBuf, LsStr, RustVtable, SymbolDefinitionFn}
import ls.pc.host.codec.Payloads
import org.eclipse.lsp4j.{Location, Position, Range}

/** [[PcDefinitionResolver]] that downcalls the Rust `symbol_definition` vtable
  * slot. The presentation compiler asks (through `SymbolSearch.definition`) for
  * the cross-file definition of a SemanticDB symbol; this marshals
  * `(semanticdbSymbol, fromFileUri)` across the FFM boundary, and Rust answers
  * from the immutable index snapshot (forward-closure pruned by the requesting
  * buffer's target). The response buffer is Rust-owned: we copy it out and free
  * it through the vtable `free` slot.
  *
  * Never throws — a non-`STATUS_OK` status or any failure answers empty, exactly
  * like [[PcDefinitionResolver.Empty]]. Runs on PC executor threads inside a
  * dispatch upcall; the Rust callback is a pure snapshot read that returns
  * without waiting on any PC/worker response, so the nested downcall cannot
  * deadlock the dispatch lane.
  */
final class PcHostDefinitionResolver(vtable: MemorySegment, log: String => Unit)
    extends PcDefinitionResolver:

  override def definition(semanticdbSymbol: String, fromFileUri: String): Vector[Location] =
    if semanticdbSymbol == null || semanticdbSymbol.isEmpty then Vector.empty
    else
      // A confined arena holds the borrowed argument structs for the call only;
      // the Rust response lives in a separate Rust-owned buffer we free below.
      val arena = Arena.ofConfined()
      try
        val out = LsBuf.allocate(arena)
        val status = SymbolDefinitionFn.invoke(
          RustVtable.symbol_definition(vtable),
          lsStr(arena, semanticdbSymbol),
          lsStr(arena, if fromFileUri == null then "" else fromFileUri),
          out
        )
        if status != boundary_h.STATUS_OK() then Vector.empty
        else
          val ptr = LsBuf.ptr(out)
          val len = LsBuf.len(out)
          // Copy the payload out of Rust memory, THEN hand the buffer back.
          val bytes = readResponse(ptr, len)
          if ptr.address() != 0 && len > 0 then
            FreeFn.invoke(RustVtable.free(vtable), ptr, len)
          if bytes.isEmpty then Vector.empty
          else Payloads.LocationsResult.decode(bytes).locations.iterator.map(toLsp).toVector
      catch
        case t: Throwable =>
          log(s"symbol_definition downcall failed: $t")
          Vector.empty
      finally arena.close()

  /** Fills a borrowed `LsStr` (UTF-8, no NUL) in `arena`. An empty string is a
    * null pointer with zero length.
    */
  private def lsStr(arena: Arena, s: String): MemorySegment =
    val struct = LsStr.allocate(arena)
    val bytes = s.getBytes(UTF_8)
    if bytes.isEmpty then
      LsStr.ptr(struct, MemorySegment.NULL)
      LsStr.len(struct, 0)
    else
      val seg = arena.allocate(bytes.length.toLong)
      MemorySegment.copy(bytes, 0, seg, ValueLayout.JAVA_BYTE, 0L, bytes.length)
      LsStr.ptr(struct, seg)
      LsStr.len(struct, bytes.length)
    struct

  private def readResponse(ptr: MemorySegment, len: Int): Array[Byte] =
    if len <= 0 || ptr.address() == 0 then Array.emptyByteArray
    else ptr.reinterpret(len.toLong).toArray(ValueLayout.JAVA_BYTE)

  private def toLsp(loc: Payloads.Location): Location =
    val r = loc.range
    Location(
      loc.uri,
      Range(Position(r.startLine, r.startCharacter), Position(r.endLine, r.endCharacter))
    )
