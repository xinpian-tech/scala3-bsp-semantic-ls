package ls.pc.host

import java.io.{FileDescriptor, FileOutputStream, PrintStream}
import java.lang.foreign.{Arena, MemorySegment, ValueLayout}
import java.lang.instrument.Instrumentation
import java.nio.charset.StandardCharsets.UTF_8

import ls.pc.host.boundary.{
  boundary_h,
  LogFn,
  PcDispatchLoopFn,
  PcQueryFn,
  PcRequestFn,
  PcResolveFn,
  PcSpawnDispatchFn,
  PcStatusOutFn,
  PcUriFn,
  PcVoidFn,
  PcVtable,
  RegisterPcVtableFn,
  RustVtable
}

/** Embedded-JVM presentation-compiler island host (Scala premain).
  *
  * Loaded via `-javaagent`, this premain fires inside `JNI_CreateJavaVM` (there
  * is no main class). Using Java FFM through the jextract-generated
  * `ls.pc.host.boundary` bindings (no JNI, no JNIEnv), it reads the Rust vtable
  * at the address passed as the agent argument, refuses to register on an ABI
  * or layout-canary mismatch, re-points `System.out` off the LSP protocol
  * stream, builds an FFM upcall stub for each of the 15 PC vtable slots,
  * registers the PC vtable, and loans two platform threads to Rust (worker 0 =
  * dispatch, worker 1 = control): they enter `pc_dispatch_loop` and never
  * return.
  *
  * The upcall stubs route to the [[PcHost]] op seam; the request/response codec
  * and the live `PcFacade` wiring behind that seam land in a subsequent slice.
  */
object PcHostAgent:
  private val AbiVersion: Long = boundary_h.ABI_VERSION().toLong
  private var arena: Arena = null
  private var vtable: MemorySegment = null
  // The boundary-marshalling runtime, built from the registered vtable's `alloc`
  // slot. Held for the op wiring (a later slice) that routes each request/
  // response through it.
  private var runtime: PcHostRuntime = null

  def premain(args: String, inst: Instrumentation): Unit =
    try
      val vtableAddr = java.lang.Long.decode(args.trim).longValue()
      arena = Arena.global()
      vtable = RustVtable.reinterpret(MemorySegment.ofAddress(vtableAddr), arena, null)

      val abi = RustVtable.abi_version(vtable)
      if abi != AbiVersion then
        System.err.println(
          s"[pc-host] ABI mismatch rust=$abi java=$AbiVersion; refusing to register"
        )
        return

      // Bootstrap layout-canary check: recompute the canary from this side's
      // jextract sizes/offsets and compare to the one the Rust side embedded. A
      // mismatch means the two sides disagree on the binary layout, so refuse
      // to register (boot is then refused via the Rust rendezvous timeout).
      val expectedCanary = RustVtable.layout_canary(vtable)
      val computedCanary = LayoutCanary.compute()
      if computedCanary != expectedCanary then
        log(
          f"layout canary mismatch: rust=0x$expectedCanary%016x island=0x$computedCanary%016x; refusing to register"
        )
        return

      // Keep island/plugin/compiler prints off the LSP protocol stream. The
      // Rust boot guard already dup2'd fd 1 to stderr before JNI_CreateJavaVM;
      // this closes the Java-level gap by pointing System.out at real stderr.
      redirectStdout()

      registerAndLoan()
    catch
      case t: Throwable =>
        System.err.println(s"[pc-host] premain failure: $t")
        t.printStackTrace()

  private def redirectStdout(): Unit =
    System.setOut(PrintStream(FileOutputStream(FileDescriptor.err), true, UTF_8))

  private def registerAndLoan(): Unit =
    val pc = PcVtable.allocate(arena)
    PcVtable.abi_version(pc, AbiVersion)

    PcVtable.register_target(
      pc,
      PcRequestFn.allocate((p, n) => contained("register_target")(PcHost.registerTarget(p, n)), arena)
    )
    PcVtable.did_open(
      pc,
      PcRequestFn.allocate((p, n) => contained("did_open")(PcHost.didOpen(p, n)), arena)
    )
    PcVtable.did_change(
      pc,
      PcRequestFn.allocate((p, n) => contained("did_change")(PcHost.didChange(p, n)), arena)
    )
    PcVtable.did_close(
      pc,
      PcUriFn.allocate(uri => contained("did_close")(PcHost.didClose(uri)), arena)
    )
    PcVtable.completion(
      pc,
      PcQueryFn.allocate((u, l, c, o) => contained("completion")(PcHost.completion(u, l, c, o)), arena)
    )
    PcVtable.completion_resolve(
      pc,
      PcResolveFn.allocate(
        (t, s, ip, il, o) => contained("completion_resolve")(PcHost.completionResolve(t, s, ip, il, o)),
        arena
      )
    )
    PcVtable.hover(
      pc,
      PcQueryFn.allocate((u, l, c, o) => contained("hover")(PcHost.hover(u, l, c, o)), arena)
    )
    PcVtable.signature_help(
      pc,
      PcQueryFn.allocate(
        (u, l, c, o) => contained("signature_help")(PcHost.signatureHelp(u, l, c, o)),
        arena
      )
    )
    PcVtable.definition(
      pc,
      PcQueryFn.allocate((u, l, c, o) => contained("definition")(PcHost.definition(u, l, c, o)), arena)
    )
    PcVtable.type_definition(
      pc,
      PcQueryFn.allocate(
        (u, l, c, o) => contained("type_definition")(PcHost.typeDefinition(u, l, c, o)),
        arena
      )
    )
    PcVtable.prepare_rename(
      pc,
      PcQueryFn.allocate(
        (u, l, c, o) => contained("prepare_rename")(PcHost.prepareRename(u, l, c, o)),
        arena
      )
    )
    PcVtable.plugin_status(
      pc,
      PcStatusOutFn.allocate(o => contained("plugin_status")(PcHost.pluginStatus(o)), arena)
    )
    PcVtable.restart_instances(
      pc,
      PcVoidFn.allocate(() => contained("restart_instances")(PcHost.restartInstances()), arena)
    )
    PcVtable.shutdown(
      pc,
      PcVoidFn.allocate(() => contained("shutdown")(PcHost.shutdown()), arena)
    )
    PcVtable.spawn_dispatch(
      pc,
      PcSpawnDispatchFn.allocate(gen => contained("spawn_dispatch")(PcHost.spawnDispatch(gen)), arena)
    )

    val rc = RegisterPcVtableFn.invoke(RustVtable.register_pc_vtable(vtable), pc)
    log(s"premain: register_pc_vtable rc=$rc")
    if rc != 0 then return

    // Build the boundary-marshalling runtime from the registered vtable's
    // `alloc` slot, ready for the op wiring to move payloads across the boundary.
    runtime = PcHostRuntime.fromVtable(vtable)

    startLoanedThread("pc-dispatch", 0)
    startLoanedThread("pc-control", 1)

  /** Runs an upcall body, containing any Java `Throwable` to a status code so it
    * never escapes across the native boundary and unwinds into Rust.
    */
  private def contained(op: String)(body: => Int): Int =
    try body
    catch
      case t: Throwable =>
        log(s"upcall $op contained: $t")
        boundary_h.STATUS_PANIC()

  private def startLoanedThread(name: String, worker: Int): Unit =
    val thread = Thread(
      () =>
        try PcDispatchLoopFn.invoke(RustVtable.pc_dispatch_loop(vtable), worker)
        catch
          case t: Throwable =>
            System.err.println(s"[pc-host] loaned thread $name returned: $t"),
      name
    )
    thread.setDaemon(true)
    thread.start()

  private def log(message: String): Unit =
    try
      val bytes = message.getBytes(UTF_8)
      val seg = arena.allocate(bytes.length.toLong)
      MemorySegment.copy(bytes, 0, seg, ValueLayout.JAVA_BYTE, 0L, bytes.length)
      LogFn.invoke(RustVtable.log(vtable), 0, seg, bytes.length)
    catch
      case t: Throwable =>
        System.err.println(s"[pc-host] log failed: $t")
