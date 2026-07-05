package spike

import java.lang.foreign.{Arena, MemorySegment, ValueLayout}
import java.lang.instrument.Instrumentation
import java.nio.charset.StandardCharsets.UTF_8

import spike.boundary.{EchoFn, LogFn, PcDispatchLoopFn, PcVtable, RegisterPcVtableFn, RustVtable, boundary_h}

/** Embedded-JVM boundary-viability island (Scala premain).
  *
  * Loaded via `-javaagent`, this premain fires inside `JNI_CreateJavaVM` (no
  * main class). Using Java FFM through the jextract-generated `spike.boundary`
  * bindings (no JNI, no JNIEnv), it reads the Rust vtable at the address passed
  * as the agent argument, recomputes the layout canary and refuses to register
  * on a mismatch, otherwise builds the echo upcall stub, registers a PC vtable,
  * and loans two platform threads to Rust: they enter `pc_dispatch_loop` and
  * never return. The whole boundary is exercised end-to-end by the Rust spike.
  */
object SpikeAgent:
  private val AbiVersion: Long = boundary_h.ABI_VERSION().toLong
  private var arena: Arena = null
  private var vtable: MemorySegment = null

  def premain(args: String, inst: Instrumentation): Unit =
    try
      val vtableAddr = java.lang.Long.decode(args.trim).longValue()
      val scenario = System.getProperty("spike.scenario", "normal")
      arena = Arena.global()
      vtable = RustVtable.reinterpret(MemorySegment.ofAddress(vtableAddr), arena, null)

      val abi = RustVtable.abi_version(vtable)
      if abi != AbiVersion then
        System.err.println(s"[spike-agent] ABI mismatch rust=$abi java=$AbiVersion")
        return
      log(s"premain: FFM bindings ready, scenario=$scenario")

      // Bootstrap layout-canary check: recompute the canary from this side's
      // struct sizes/offsets and compare to the one the Rust side embedded. A
      // mismatch means the two sides disagree on the binary layout, so refuse
      // to register (boot is refused via the Rust rendezvous timeout).
      val expectedCanary = RustVtable.layout_canary(vtable)
      val computedCanary = layoutCanary()
      if computedCanary != expectedCanary then
        log(
          f"layout canary mismatch: rust=0x$expectedCanary%016x island=0x$computedCanary%016x; refusing to register"
        )
        return

      if scenario == "timeout" then
        // Deliberately never register: exercise the Rust rendezvous timeout
        // with a captured island-log line.
        log("premain: skipping registration to force rendezvous timeout")
        return

      val echoStub = EchoFn.allocate(echoFunction, arena)
      val pcVtable = PcVtable.allocate(arena)
      PcVtable.abi_version(pcVtable, AbiVersion)
      PcVtable.echo(pcVtable, echoStub)

      val rc = RegisterPcVtableFn.invoke(RustVtable.register_pc_vtable(vtable), pcVtable)
      log(s"premain: register_pc_vtable rc=$rc")
      if rc != 0 then return

      // Loan two platform threads to Rust; each enters pc_dispatch_loop and
      // never returns.
      startLoanedThread("pc-dispatch", 0)
      startLoanedThread("pc-control", 1)
    catch
      case t: Throwable =>
        System.err.println(s"[spike-agent] premain failure: $t")
        t.printStackTrace()

  /** The PC echo op, exposed to Rust as an FFM upcall stub. Copies the
    * caller-owned request bytes into the Rust-owned response buffer and returns
    * the written length. A Java `Throwable` is contained to a negative status so
    * it never escapes across the native boundary.
    */
  private val echoFunction: EchoFn.Function = (inPtr, inLen, outPtr, outCap) =>
    try
      if inLen < 0 || outCap < 0 then -2
      else
        val src = inPtr.reinterpret(inLen.toLong)
        val bytes = src.toArray(ValueLayout.JAVA_BYTE)
        if String(bytes, UTF_8) == "__throw__" then
          throw RuntimeException("injected Java throwable in echo upcall")
        val n = math.min(inLen, outCap)
        val dst = outPtr.reinterpret(outCap.toLong)
        MemorySegment.copy(src, 0L, dst, 0L, n.toLong)
        n
    catch
      case t: Throwable =>
        System.err.println(s"[spike-agent] echo contained: $t")
        -1

  private def startLoanedThread(name: String, worker: Int): Unit =
    val thread = Thread(
      () =>
        try PcDispatchLoopFn.invoke(RustVtable.pc_dispatch_loop(vtable), worker)
        catch
          case t: Throwable =>
            System.err.println(s"[spike-agent] loaned thread $name returned: $t"),
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
        System.err.println(s"[spike-agent] log failed: $t")

  // FNV-1a (64-bit). Must stay byte-for-byte identical to the Rust side: the
  // same nine layout facts, in the same order, hashed as little-endian u64s.
  private val FnvOffset: Long = 0xcbf29ce484222325L
  private val FnvPrime: Long = 0x100000001b3L

  private def fnv1a(values: Array[Long]): Long =
    var hash = FnvOffset
    var i = 0
    while i < values.length do
      val value = values(i)
      var b = 0
      while b < 8 do
        hash ^= (value >>> (b * 8)) & 0xffL
        hash *= FnvPrime
        b += 1
      i += 1
    hash

  /** Recompute the boundary layout canary from this side's jextract
    * sizes/offsets — the cross-language contract with `compute_layout_canary`
    * in the Rust crate.
    */
  private def layoutCanary(): Long =
    fnv1a(
      Array[Long](
        RustVtable.sizeof(),
        RustVtable.`abi_version$offset`(),
        RustVtable.`layout_canary$offset`(),
        RustVtable.`log$offset`(),
        RustVtable.`register_pc_vtable$offset`(),
        RustVtable.`pc_dispatch_loop$offset`(),
        PcVtable.sizeof(),
        PcVtable.`abi_version$offset`(),
        PcVtable.`echo$offset`()
      )
    )
