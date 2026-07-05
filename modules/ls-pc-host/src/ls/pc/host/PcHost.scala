package ls.pc.host

import java.lang.foreign.MemorySegment

import ls.pc.host.boundary.boundary_h

/** The presentation-compiler op seam behind the boundary upcall stubs.
  *
  * [[PcHostAgent]] exposes each of the 15 PC vtable slots to Rust as an FFM
  * upcall stub and routes the decoded call here; this object holds the op
  * bodies. The method signatures mirror the boundary function-pointer shapes
  * (`PcRequestFn`/`PcUriFn`/`PcQueryFn`/`PcResolveFn`/`PcStatusOutFn`/
  * `PcVoidFn`/`PcSpawnDispatchFn`) so the request/response codec and the live
  * `PcFacade`/`PcWorkerManager` wiring can be filled in without touching the
  * FFM boundary adapter.
  *
  * Until that wiring lands, every op reports a typed internal status rather
  * than fabricating a success: the boundary stays well-formed (a status is
  * always returned, memory is never touched) and a caller can distinguish
  * "unwired" from a real result.
  */
object PcHost:
  /** Placeholder result for an op whose facade wiring has not landed yet. */
  private def unwired: Int = boundary_h.STATUS_INTERNAL()

  // Lifecycle: encoded-payload requests (register_target/did_open/did_change).
  def registerTarget(paramsPtr: MemorySegment, paramsLen: Int): Int = unwired
  def didOpen(paramsPtr: MemorySegment, paramsLen: Int): Int = unwired
  def didChange(paramsPtr: MemorySegment, paramsLen: Int): Int = unwired

  // Lifecycle: single-uri request (did_close).
  def didClose(uri: MemorySegment): Int = unwired

  // Position queries (completion/hover/signature_help/definition/
  // type_definition/prepare_rename): write the response payload to `out`.
  def completion(uri: MemorySegment, line: Int, character: Int, out: MemorySegment): Int = unwired
  def hover(uri: MemorySegment, line: Int, character: Int, out: MemorySegment): Int = unwired
  def signatureHelp(uri: MemorySegment, line: Int, character: Int, out: MemorySegment): Int =
    unwired
  def definition(uri: MemorySegment, line: Int, character: Int, out: MemorySegment): Int = unwired
  def typeDefinition(uri: MemorySegment, line: Int, character: Int, out: MemorySegment): Int =
    unwired
  def prepareRename(uri: MemorySegment, line: Int, character: Int, out: MemorySegment): Int =
    unwired

  // Completion-item resolve.
  def completionResolve(
      targetId: MemorySegment,
      symbol: MemorySegment,
      itemPtr: MemorySegment,
      itemLen: Int,
      out: MemorySegment
  ): Int = unwired

  // No-argument status query (plugin_status).
  def pluginStatus(out: MemorySegment): Int = unwired

  // No-argument lifecycle ops (restart_instances/shutdown).
  def restartInstances(): Int = unwired
  def shutdown(): Int = unwired

  // Fresh loaned dispatch thread for the given generation (spawn_dispatch).
  def spawnDispatch(generation: Int): Int = unwired
