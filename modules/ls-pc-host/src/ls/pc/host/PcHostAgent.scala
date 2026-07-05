package ls.pc.host

import java.io.{FileDescriptor, FileOutputStream, PrintStream}
import java.lang.foreign.{Arena, MemorySegment, ValueLayout}
import java.lang.instrument.Instrumentation
import java.nio.charset.StandardCharsets.UTF_8
import java.nio.file.Path

import ls.pc.{PcFacade, PcPluginInitContext, PcPluginManager, PcSettings}
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
  * The upcall stubs route to a [[PcHost]] instance that decodes each request,
  * calls the live in-process `PcFacade`, converts the result, and writes the
  * flat response through the boundary-marshalling runtime.
  */
object PcHostAgent:
  private val AbiVersion: Long = boundary_h.ABI_VERSION().toLong
  private var arena: Arena = null
  private var vtable: MemorySegment = null

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
    // The op seam: the boundary-marshalling runtime (built from the registered
    // vtable's `alloc` slot), the live in-process facade, and the fresh-thread
    // loaner `spawn_dispatch` uses for a recovery generation.
    val runtime = PcHostRuntime.fromVtable(vtable)
    val host = PcHost(
      runtime,
      FacadePcOps(buildFacade()),
      // A recovery generation loans a fresh DISPATCH thread (worker role 0, like
      // the boot dispatch lane); the generation identifies the Rust-side staged
      // channel it will pick up, not the worker role. Passing the generation as
      // the worker index would route it to the control worker and wedge boot.
      generation => startLoanedThread(s"pc-dispatch-gen-$generation", 0)
    )

    val pc = PcVtable.allocate(arena)
    PcVtable.abi_version(pc, AbiVersion)

    PcVtable.register_target(
      pc,
      PcRequestFn.allocate((p, n) => contained("register_target")(host.registerTarget(p, n)), arena)
    )
    PcVtable.did_open(
      pc,
      PcRequestFn.allocate((p, n) => contained("did_open")(host.didOpen(p, n)), arena)
    )
    PcVtable.did_change(
      pc,
      PcRequestFn.allocate((p, n) => contained("did_change")(host.didChange(p, n)), arena)
    )
    PcVtable.did_close(
      pc,
      PcUriFn.allocate(uri => contained("did_close")(host.didClose(uri)), arena)
    )
    PcVtable.completion(
      pc,
      PcQueryFn.allocate((u, l, c, o) => contained("completion")(host.completion(u, l, c, o)), arena)
    )
    PcVtable.completion_resolve(
      pc,
      PcResolveFn.allocate(
        (t, s, ip, il, o) => contained("completion_resolve")(host.completionResolve(t, s, ip, il, o)),
        arena
      )
    )
    PcVtable.hover(
      pc,
      PcQueryFn.allocate((u, l, c, o) => contained("hover")(host.hover(u, l, c, o)), arena)
    )
    PcVtable.signature_help(
      pc,
      PcQueryFn.allocate(
        (u, l, c, o) => contained("signature_help")(host.signatureHelp(u, l, c, o)),
        arena
      )
    )
    PcVtable.definition(
      pc,
      PcQueryFn.allocate((u, l, c, o) => contained("definition")(host.definition(u, l, c, o)), arena)
    )
    PcVtable.type_definition(
      pc,
      PcQueryFn.allocate(
        (u, l, c, o) => contained("type_definition")(host.typeDefinition(u, l, c, o)),
        arena
      )
    )
    PcVtable.prepare_rename(
      pc,
      PcQueryFn.allocate(
        (u, l, c, o) => contained("prepare_rename")(host.prepareRename(u, l, c, o)),
        arena
      )
    )
    PcVtable.plugin_status(
      pc,
      PcStatusOutFn.allocate(o => contained("plugin_status")(host.pluginStatus(o)), arena)
    )
    PcVtable.restart_instances(
      pc,
      PcVoidFn.allocate(() => contained("restart_instances")(host.restartInstances()), arena)
    )
    PcVtable.shutdown(
      pc,
      PcVoidFn.allocate(() => contained("shutdown")(host.shutdown()), arena)
    )
    PcVtable.spawn_dispatch(
      pc,
      PcSpawnDispatchFn.allocate(gen => contained("spawn_dispatch")(host.spawnDispatch(gen)), arena)
    )

    val rc = RegisterPcVtableFn.invoke(RustVtable.register_pc_vtable(vtable), pc)
    log(s"premain: register_pc_vtable rc=$rc")
    if rc != 0 then return

    startLoanedThread("pc-dispatch", 0)
    startLoanedThread("pc-control", 1)

  /** Builds the in-process presentation-compiler facade the op seam drives. The
    * workspace root comes from the `ls.pc.host.workspace` system property the
    * Rust host sets before boot; without it, ephemeral (workspace-less)
    * settings are used.
    */
  private def buildFacade(): PcFacade =
    val settings = Option(System.getProperty("ls.pc.host.workspace")) match
      case Some(root) => PcSettings.forWorkspace(Path.of(root))
      case None => PcSettings.ephemeral()
    val pluginManager = PcPluginManager(
      PcPluginInitContext(settings.workspaceRoot, settings.generatedSourcesRoot, m => log(s"pc-plugin: $m"))
    )
    // Load `<root>/.scala3-bsp-semantic-ls/pc-plugins.json` (compiler + service
    // plugins) into the manager before the facade reads it, matching the
    // retained worker; a bad config is logged, not fatal.
    settings.workspaceRoot.foreach(root => PcHostConfig.applyWorkspacePlugins(pluginManager, root, log))
    PcFacade(pluginManager, settings)

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
