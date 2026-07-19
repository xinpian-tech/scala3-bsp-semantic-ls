package ls.pc.host

import java.lang.foreign.MemorySegment

import ls.pc.host.boundary.boundary_h
import ls.pc.host.codec.Payloads

/** The presentation-compiler op seam behind the boundary upcall stubs.
  *
  * [[PcHostAgent]] exposes each of the 22 PC vtable slots to Rust as an FFM
  * upcall stub and routes the decoded call here. Each op decodes its request
  * (flat [[Payloads]]), calls the live [[PcOps]] facade, converts the lsp4j /
  * `spi` result with [[Marshal]], and writes the flat response through
  * [[PcHostRuntime]] (lifecycle ops via `runStatus`, response-bearing ops via
  * the alloc-once `runResponse`). The method signatures mirror the boundary
  * function-pointer shapes (`PcRequestFn`/`PcUriFn`/`PcQueryFn`/`PcResolveFn`/
  * `PcStatusOutFn`/`PcVoidFn`/`PcSpawnDispatchFn`/`PcPayloadQueryFn`).
  *
  * `loanDispatch(generation)` loans a fresh Java platform thread into the Rust
  * `pc_dispatch_loop` for that generation; the facade seam and the loaning
  * function are injected so the routing is unit-testable without a live
  * compiler or a booted JVM.
  */
final class PcHost(runtime: PcHostRuntime, ops: PcOps, loanDispatch: Int => Unit):
  import PcHostRuntime.{readLsStr, readRequest}

  // Lifecycle: encoded-payload requests (register_target/did_open/did_change).
  def registerTarget(paramsPtr: MemorySegment, paramsLen: Int): Int =
    runtime.runStatus {
      val config = Payloads.TargetConfig.decode(readRequest(paramsPtr, paramsLen))
      ops.registerTarget(Marshal.targetConfig(config))
    }

  def didOpen(paramsPtr: MemorySegment, paramsLen: Int): Int =
    runtime.runStatus {
      val p = Payloads.DidOpenParams.decode(readRequest(paramsPtr, paramsLen))
      ops.didOpen(p.targetId, p.uri, p.text)
    }

  def didChange(paramsPtr: MemorySegment, paramsLen: Int): Int =
    runtime.runStatus {
      val p = Payloads.DidChangeParams.decode(readRequest(paramsPtr, paramsLen))
      ops.didChange(p.uri, p.text)
    }

  // Lifecycle: single-uri request (did_close).
  def didClose(uri: MemorySegment): Int =
    runtime.runStatus(ops.didClose(readLsStr(uri)))

  // Position queries (completion/hover/signature_help/definition/
  // type_definition/prepare_rename): write the response payload to `out`.
  def completion(uri: MemorySegment, line: Int, character: Int, out: MemorySegment): Int =
    runtime.runResponse(out)(Marshal.completionList(ops.completion(readLsStr(uri), line, character)).encode())

  def hover(uri: MemorySegment, line: Int, character: Int, out: MemorySegment): Int =
    runtime.runResponse(out)(Marshal.hover(ops.hover(readLsStr(uri), line, character)).encode())

  def signatureHelp(uri: MemorySegment, line: Int, character: Int, out: MemorySegment): Int =
    runtime.runResponse(out)(Marshal.signatureHelp(ops.signatureHelp(readLsStr(uri), line, character)).encode())

  def definition(uri: MemorySegment, line: Int, character: Int, out: MemorySegment): Int =
    runtime.runResponse(out)(Marshal.definition(ops.definition(readLsStr(uri), line, character)).encode())

  def typeDefinition(uri: MemorySegment, line: Int, character: Int, out: MemorySegment): Int =
    runtime.runResponse(out)(Marshal.definition(ops.typeDefinition(readLsStr(uri), line, character)).encode())

  def prepareRename(uri: MemorySegment, line: Int, character: Int, out: MemorySegment): Int =
    runtime.runResponse(out)(Marshal.prepareRename(ops.prepareRename(readLsStr(uri), line, character)).encode())

  // Completion-item resolve.
  def completionResolve(
      targetId: MemorySegment,
      symbol: MemorySegment,
      itemPtr: MemorySegment,
      itemLen: Int,
      out: MemorySegment
  ): Int =
    runtime.runResponse(out) {
      val item = Payloads.CompletionItem.decode(readRequest(itemPtr, itemLen))
      val resolved = ops.completionItemResolve(readLsStr(targetId), Marshal.toLsp4jItem(item), readLsStr(symbol))
      Marshal.completionItem(resolved).encode()
    }

  // Payload-in/payload-out queries (ABI v2: inlay_hints/semantic_tokens/
  // selection_range/code_action/auto_imports/pc_diagnostics/folding_range).
  // Each decodes its params payload, calls the op seam, and encodes the flat
  // response; a not-yet-provided op surfaces `STATUS_NOT_YET` through the
  // runtime's typed mapping.
  def inlayHints(paramsPtr: MemorySegment, paramsLen: Int, out: MemorySegment): Int =
    runtime.runResponse(out) {
      val p = Payloads.InlayHintParams.decode(readRequest(paramsPtr, paramsLen))
      Marshal.inlayHints(ops.inlayHints(p.uri, Marshal.toLspRange(p.range), p.flags)).encode()
    }

  def semanticTokens(paramsPtr: MemorySegment, paramsLen: Int, out: MemorySegment): Int =
    runtime.runResponse(out) {
      val p = Payloads.UriParams.decode(readRequest(paramsPtr, paramsLen))
      Marshal.semanticTokens(ops.semanticTokens(p.uri)).encode()
    }

  def selectionRange(paramsPtr: MemorySegment, paramsLen: Int, out: MemorySegment): Int =
    runtime.runResponse(out) {
      val p = Payloads.SelectionRangeParams.decode(readRequest(paramsPtr, paramsLen))
      val positions = p.positions.iterator.map(Marshal.toLspPosition).toVector
      Marshal.selectionRanges(ops.selectionRanges(p.uri, positions)).encode()
    }

  def codeAction(paramsPtr: MemorySegment, paramsLen: Int, out: MemorySegment): Int =
    runtime.runResponse(out) {
      val p = Payloads.CodeActionParams.decode(readRequest(paramsPtr, paramsLen))
      val result = ops.codeAction(
        p.uri,
        p.action,
        Marshal.toLspPosition(p.position),
        p.extractionEnd.map(Marshal.toLspPosition),
        p.argIndices.map(_.toVector)
      )
      Marshal.codeActionResult(result).encode()
    }

  def autoImports(paramsPtr: MemorySegment, paramsLen: Int, out: MemorySegment): Int =
    runtime.runResponse(out) {
      val p = Payloads.AutoImportParams.decode(readRequest(paramsPtr, paramsLen))
      val imports = ops.autoImports(p.uri, Marshal.toLspPosition(p.position), p.name, p.isExtension)
      Marshal.autoImports(imports).encode()
    }

  def pcDiagnostics(paramsPtr: MemorySegment, paramsLen: Int, out: MemorySegment): Int =
    runtime.runResponse(out) {
      val p = Payloads.UriParams.decode(readRequest(paramsPtr, paramsLen))
      Marshal.pcDiagnostics(ops.pcDiagnostics(p.uri)).encode()
    }

  def foldingRange(paramsPtr: MemorySegment, paramsLen: Int, out: MemorySegment): Int =
    runtime.runResponse(out) {
      val p = Payloads.UriParams.decode(readRequest(paramsPtr, paramsLen))
      Marshal.foldingRanges(ops.foldingRanges(p.uri)).encode()
    }

  // No-argument status query (plugin_status).
  def pluginStatus(out: MemorySegment): Int =
    runtime.runResponse(out)(Marshal.pluginStatus(ops.pluginStatus).encode())

  // No-argument lifecycle ops (restart_instances/shutdown).
  def restartInstances(): Int = runtime.runStatus(ops.restartInstances())
  def shutdown(): Int = runtime.runStatus(ops.shutdown())

  // Fresh loaned dispatch thread for the given generation (spawn_dispatch).
  def spawnDispatch(generation: Int): Int =
    try
      loanDispatch(generation)
      boundary_h.STATUS_OK()
    catch case _: Throwable => boundary_h.STATUS_INTERNAL()
