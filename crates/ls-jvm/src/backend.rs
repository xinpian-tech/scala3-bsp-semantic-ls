//! The production request channel and backend over the registered `PcVtable`.
//!
//! At registration the island loans two platform threads that downcall into
//! [`IslandRuntime::enter_dispatch_worker`] (worker 0) and
//! [`IslandRuntime::enter_control_worker`] (worker 1) and never return: Java
//! loans the threads to Rust. Worker 0 pops `PcRequest`s from an mpsc channel
//! and invokes the PC vtable slots directly, serializing them exactly as
//! today's `InProcessPcWorker`; worker 1 serves control ops
//! (`restart_instances`/`shutdown`/`spawn_dispatch`/`plugin_status`) so a wedged
//! dispatch lane can be recovered while it is stuck.
//!
//! [`VtableBackend`] is the [`PcBackend`] the [`Supervisor`] drives: it routes
//! dispatch ops to worker 0 under a deadline, control ops to worker 1, and on a
//! non-cooperative wedge spawns a fresh dispatch generation via the real
//! `spawn_dispatch` slot and replays the mirrored targets/buffers into it.
//! Every vtable status is surfaced; all response memory is Rust-owned and freed.

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use ls_pc_abi::abi::{LsBuf, LsStr, PcVtable};
use ls_pc_abi::memory::abi_free;
use ls_pc_abi::payloads::{DidChangeParams, DidOpenParams};
use ls_pc_abi::{STATUS_INTERNAL, STATUS_OK, STATUS_PANIC};

use crate::mirror::ReplayPlan;
use crate::watchdog::{DispatchOutcome, PcBackend, PcRequest, QueryKind};

// ---------------------------------------------------------------------------
// Vtable-slot invocation (all response memory is Rust-owned and freed here).
// ---------------------------------------------------------------------------

/// A validated, process-lifetime handle to the registered PC vtable. The vtable
/// lives in the island's global arena and its slots are upcall stubs callable
/// from any thread, so the handle is shared across the loaned threads.
struct PcHandle(NonNull<PcVtable>);

// SAFETY: the pointee is immutable for the process lifetime and its slots are
// thread-safe upcall stubs; the handle only ever hands out `&PcVtable`.
unsafe impl Send for PcHandle {}
unsafe impl Sync for PcHandle {}

impl PcHandle {
    fn get(&self) -> &PcVtable {
        // SAFETY: the vtable outlives the process and is never mutated.
        unsafe { self.0.as_ref() }
    }
}

/// A completed vtable call: its status and any Rust-owned response bytes.
struct OpOutcome {
    status: i32,
    out: Vec<u8>,
}

impl OpOutcome {
    fn status(status: i32) -> OpOutcome {
        OpOutcome {
            status,
            out: Vec::new(),
        }
    }

    fn panicked() -> OpOutcome {
        OpOutcome::status(STATUS_PANIC)
    }
}

/// A control-lane op (worker 1).
enum ControlOp {
    RestartInstances,
    Shutdown,
    SpawnDispatch(u32),
    PluginStatus,
}

fn lsstr(s: &str) -> LsStr {
    LsStr {
        ptr: s.as_ptr(),
        len: s.len() as u32,
    }
}

fn empty_buf() -> LsBuf {
    LsBuf {
        ptr: std::ptr::null_mut(),
        len: 0,
    }
}

/// Copy out a Rust-owned response buffer (written by the island via
/// `write_response`/`abi_alloc`) and free it.
fn take_response(out: LsBuf) -> Vec<u8> {
    if out.ptr.is_null() || out.len == 0 {
        return Vec::new();
    }
    // SAFETY: `out` is a buffer the island allocated with our `abi_alloc`, valid
    // for `len` bytes; we copy it and free it exactly once.
    let bytes = unsafe { std::slice::from_raw_parts(out.ptr, out.len as usize) }.to_vec();
    // SAFETY: same allocation, freed once with its recorded size.
    unsafe { abi_free(out.ptr, out.len) };
    bytes
}

fn invoke_dispatch(pc: &PcVtable, request: &PcRequest) -> OpOutcome {
    match request {
        PcRequest::RegisterTarget { config, .. } => {
            let bytes = config.encode();
            // SAFETY: `bytes` is a valid buffer for the call.
            OpOutcome::status(unsafe { (pc.register_target)(bytes.as_ptr(), bytes.len() as u32) })
        }
        PcRequest::DidOpen {
            target_id,
            uri,
            text,
        } => {
            let bytes = DidOpenParams {
                target_id: target_id.clone(),
                uri: uri.clone(),
                text: text.clone(),
            }
            .encode();
            // SAFETY: `bytes` is a valid buffer for the call.
            OpOutcome::status(unsafe { (pc.did_open)(bytes.as_ptr(), bytes.len() as u32) })
        }
        PcRequest::DidChange { uri, text } => {
            let bytes = DidChangeParams {
                uri: uri.clone(),
                text: text.clone(),
            }
            .encode();
            // SAFETY: `bytes` is a valid buffer for the call.
            OpOutcome::status(unsafe { (pc.did_change)(bytes.as_ptr(), bytes.len() as u32) })
        }
        PcRequest::DidClose { uri } => {
            // SAFETY: `uri` outlives the call via `request`.
            OpOutcome::status(unsafe { (pc.did_close)(lsstr(uri)) })
        }
        PcRequest::Query {
            kind,
            uri,
            line,
            character,
        } => {
            let slot = match kind {
                QueryKind::Completion => pc.completion,
                QueryKind::Hover => pc.hover,
                QueryKind::SignatureHelp => pc.signature_help,
                QueryKind::Definition => pc.definition,
                QueryKind::TypeDefinition => pc.type_definition,
                QueryKind::PrepareRename => pc.prepare_rename,
            };
            let mut out = empty_buf();
            // SAFETY: `uri` outlives the call; `out` is a valid out-param.
            let status = unsafe { slot(lsstr(uri), *line, *character, &mut out) };
            OpOutcome {
                status,
                out: take_response(out),
            }
        }
        PcRequest::Resolve {
            target_id,
            symbol,
            item,
        } => {
            let mut out = empty_buf();
            // SAFETY: args outlive the call; `out` is a valid out-param.
            let status = unsafe {
                (pc.completion_resolve)(
                    lsstr(target_id),
                    lsstr(symbol),
                    item.as_ptr(),
                    item.len() as u32,
                    &mut out,
                )
            };
            OpOutcome {
                status,
                out: take_response(out),
            }
        }
    }
}

fn invoke_control(pc: &PcVtable, op: &ControlOp) -> OpOutcome {
    match op {
        // SAFETY: no-argument control slots.
        ControlOp::RestartInstances => OpOutcome::status(unsafe { (pc.restart_instances)() }),
        ControlOp::Shutdown => OpOutcome::status(unsafe { (pc.shutdown)() }),
        ControlOp::SpawnDispatch(generation) => {
            OpOutcome::status(unsafe { (pc.spawn_dispatch)(*generation) })
        }
        ControlOp::PluginStatus => {
            let mut out = empty_buf();
            // SAFETY: `out` is a valid out-param.
            let status = unsafe { (pc.plugin_status)(&mut out) };
            OpOutcome {
                status,
                out: take_response(out),
            }
        }
    }
}

// ---------------------------------------------------------------------------
// The loaned-thread runtime.
// ---------------------------------------------------------------------------

struct DispatchJob {
    op: PcRequest,
    reply: Sender<OpOutcome>,
}

struct ControlJob {
    op: ControlOp,
    reply: Sender<OpOutcome>,
}

/// Owns the registered vtable and the dispatch/control channels. Constructed at
/// registration; the loaned threads enter it through `enter_*_worker`.
pub struct IslandRuntime {
    pc: PcHandle,
    dispatch_tx: Mutex<Sender<DispatchJob>>,
    staged_dispatch_rx: Mutex<Option<Receiver<DispatchJob>>>,
    dispatch_busy: AtomicBool,
    dispatch_attached: AtomicU64,
    attach_cv: Condvar,
    attach_lock: Mutex<()>,
    control_tx: Sender<ControlJob>,
    staged_control_rx: Mutex<Option<Receiver<ControlJob>>>,
}

impl IslandRuntime {
    /// Build the runtime around the registered PC vtable, with the initial
    /// (generation-0) dispatch + control channels staged for the first two
    /// loaned threads.
    pub fn new(pc: NonNull<PcVtable>) -> Arc<IslandRuntime> {
        let (dispatch_tx, dispatch_rx) = channel();
        let (control_tx, control_rx) = channel();
        Arc::new(IslandRuntime {
            pc: PcHandle(pc),
            dispatch_tx: Mutex::new(dispatch_tx),
            staged_dispatch_rx: Mutex::new(Some(dispatch_rx)),
            dispatch_busy: AtomicBool::new(false),
            dispatch_attached: AtomicU64::new(0),
            attach_cv: Condvar::new(),
            attach_lock: Mutex::new(()),
            control_tx,
            staged_control_rx: Mutex::new(Some(control_rx)),
        })
    }

    /// Entered by the loaned dispatch thread (worker 0). Takes the staged
    /// receiver for the current generation, signals its attachment, then
    /// serializes PC ops until the channel closes (a superseded generation) or
    /// it wedges (an abandoned generation).
    pub fn enter_dispatch_worker(self: &Arc<IslandRuntime>) {
        let rx = self
            .staged_dispatch_rx
            .lock()
            .expect("staged dispatch rx lock")
            .take()
            .expect("no staged dispatch receiver");
        self.signal_attached();
        while let Ok(job) = rx.recv() {
            self.dispatch_busy.store(true, Ordering::SeqCst);
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                invoke_dispatch(self.pc.get(), &job.op)
            }))
            .unwrap_or_else(|_| OpOutcome::panicked());
            self.dispatch_busy.store(false, Ordering::SeqCst);
            let _ = job.reply.send(outcome);
        }
    }

    /// Entered by the loaned control thread (worker 1).
    pub fn enter_control_worker(self: &Arc<IslandRuntime>) {
        let rx = self
            .staged_control_rx
            .lock()
            .expect("staged control rx lock")
            .take()
            .expect("no staged control receiver");
        while let Ok(job) = rx.recv() {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                invoke_control(self.pc.get(), &job.op)
            }))
            .unwrap_or_else(|_| OpOutcome::panicked());
            let _ = job.reply.send(outcome);
        }
    }

    fn signal_attached(&self) {
        let _guard = self.attach_lock.lock().expect("attach lock");
        self.dispatch_attached.fetch_add(1, Ordering::SeqCst);
        self.attach_cv.notify_all();
    }

    fn attach_count(&self) -> u64 {
        self.dispatch_attached.load(Ordering::SeqCst)
    }

    fn wait_attached_after(&self, before: u64, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let mut guard = self.attach_lock.lock().expect("attach lock");
        while self.dispatch_attached.load(Ordering::SeqCst) <= before {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            let (g, res) = self
                .attach_cv
                .wait_timeout(guard, deadline - now)
                .expect("attach cv wait");
            guard = g;
            if res.timed_out() && self.dispatch_attached.load(Ordering::SeqCst) <= before {
                return false;
            }
        }
        true
    }

    fn stage_new_dispatch_channel(&self) {
        let (tx, rx) = channel();
        *self.dispatch_tx.lock().expect("dispatch tx lock") = tx;
        *self
            .staged_dispatch_rx
            .lock()
            .expect("staged dispatch rx lock") = Some(rx);
    }

    fn dispatch_sender(&self) -> Sender<DispatchJob> {
        self.dispatch_tx.lock().expect("dispatch tx lock").clone()
    }

    fn submit_control(&self, op: ControlOp, deadline: Duration) -> OpOutcome {
        let (reply_tx, reply_rx) = channel();
        if self
            .control_tx
            .send(ControlJob {
                op,
                reply: reply_tx,
            })
            .is_err()
        {
            return OpOutcome::status(STATUS_INTERNAL);
        }
        reply_rx
            .recv_timeout(deadline)
            .unwrap_or_else(|_| OpOutcome::status(STATUS_INTERNAL))
    }
}

// ---------------------------------------------------------------------------
// The PcBackend the Supervisor drives.
// ---------------------------------------------------------------------------

/// Timeouts for the control lane and generation spawn/replay (distinct from the
/// per-request dispatch deadline the [`Supervisor`] enforces).
#[derive(Clone, Copy)]
pub struct BackendTimeouts {
    pub control: Duration,
    pub attach: Duration,
    pub replay: Duration,
}

impl Default for BackendTimeouts {
    fn default() -> BackendTimeouts {
        BackendTimeouts {
            control: Duration::from_secs(5),
            attach: Duration::from_secs(5),
            replay: Duration::from_secs(30),
        }
    }
}

/// How a fresh dispatch generation's loaned thread comes to enter the worker
/// loop. In production this is a no-op: the real `spawn_dispatch` slot makes the
/// island loan the thread. Tests inject a hook that spawns a stand-in thread.
pub type SpawnHook = Box<dyn Fn(&Arc<IslandRuntime>) + Send>;

/// The production backend over the registered vtable.
pub struct VtableBackend {
    rt: Arc<IslandRuntime>,
    on_spawn_dispatch: SpawnHook,
    timeouts: BackendTimeouts,
}

impl VtableBackend {
    /// Production backend: the island loans the new dispatch thread in response
    /// to the real `spawn_dispatch` slot.
    pub fn new(rt: Arc<IslandRuntime>) -> VtableBackend {
        VtableBackend {
            rt,
            on_spawn_dispatch: Box::new(|_| {}),
            timeouts: BackendTimeouts::default(),
        }
    }

    /// Backend with an injected spawn hook + timeouts (for tests that stand in
    /// for the island's thread loan).
    pub fn with_spawn_hook(
        rt: Arc<IslandRuntime>,
        timeouts: BackendTimeouts,
        on_spawn_dispatch: SpawnHook,
    ) -> VtableBackend {
        VtableBackend {
            rt,
            on_spawn_dispatch,
            timeouts,
        }
    }

    /// Control lane: shut down the PC instances (orderly teardown before the
    /// process exits). Returns the vtable status.
    pub fn shutdown(&self) -> i32 {
        self.rt
            .submit_control(ControlOp::Shutdown, self.timeouts.control)
            .status
    }

    /// Control lane: fetch the plugin-status report while dispatch is busy.
    pub fn plugin_status(&self) -> Result<Vec<u8>, i32> {
        let outcome = self
            .rt
            .submit_control(ControlOp::PluginStatus, self.timeouts.control);
        if outcome.status == STATUS_OK {
            Ok(outcome.out)
        } else {
            Err(outcome.status)
        }
    }
}

impl PcBackend for VtableBackend {
    fn dispatch(&mut self, request: &PcRequest, deadline: Duration) -> DispatchOutcome {
        let (reply_tx, reply_rx) = channel();
        let job = DispatchJob {
            op: request.clone(),
            reply: reply_tx,
        };
        if self.rt.dispatch_sender().send(job).is_err() {
            return DispatchOutcome::Wedged;
        }
        match reply_rx.recv_timeout(deadline) {
            Ok(outcome) if outcome.status == STATUS_OK => DispatchOutcome::Done(outcome.out),
            Ok(outcome) => DispatchOutcome::Status(outcome.status),
            Err(_) => DispatchOutcome::Wedged,
        }
    }

    fn restart_instances(&mut self) -> i32 {
        self.rt
            .submit_control(ControlOp::RestartInstances, self.timeouts.control)
            .status
    }

    fn lane_idle(&mut self) -> bool {
        !self.rt.dispatch_busy.load(Ordering::SeqCst)
    }

    fn spawn_generation(&mut self, generation: u32, replay: &ReplayPlan) -> i32 {
        // Stage the next generation's channel before asking the island to loan a
        // thread, so the fresh worker picks up the right receiver.
        self.rt.stage_new_dispatch_channel();
        let before = self.rt.attach_count();

        // The real spawn_dispatch slot (over the control lane, so it runs while
        // dispatch is wedged).
        let status = self
            .rt
            .submit_control(ControlOp::SpawnDispatch(generation), self.timeouts.control)
            .status;
        if status != STATUS_OK {
            return status;
        }

        // Production: no-op (the island loaned the thread). Tests: spawn a
        // stand-in dispatch thread.
        (self.on_spawn_dispatch)(&self.rt);
        if !self.rt.wait_attached_after(before, self.timeouts.attach) {
            return STATUS_INTERNAL;
        }

        // Replay the mirrored state through the new dispatch lane before
        // accepting the generation.
        for op in replay_requests(replay) {
            match self.dispatch(&op, self.timeouts.replay) {
                DispatchOutcome::Done(_) => {}
                DispatchOutcome::Status(code) => return code,
                DispatchOutcome::Wedged => return STATUS_INTERNAL,
            }
        }
        STATUS_OK
    }
}

/// The replay stream as concrete requests: re-register every target, then
/// re-open every buffer with its latest text.
fn replay_requests(replay: &ReplayPlan) -> Vec<PcRequest> {
    let mut requests = Vec::with_capacity(replay.targets.len() + replay.buffers.len());
    for (id, config) in &replay.targets {
        requests.push(PcRequest::RegisterTarget {
            id: id.clone(),
            config: config.clone(),
        });
    }
    for buffer in &replay.buffers {
        requests.push(PcRequest::DidOpen {
            target_id: buffer.target_id.clone(),
            uri: buffer.uri.clone(),
            text: buffer.text.clone(),
        });
    }
    requests
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watchdog::{PcError, Supervisor};
    use ls_pc_abi::memory::abi_alloc;
    use ls_pc_abi::payloads::TargetConfig;
    use std::sync::OnceLock;

    // A single global stub PC vtable + recorder. One integration test uses it,
    // serialized behind `serial()`, so there is no cross-test contention.
    struct StubState {
        calls: Vec<&'static str>,
        completion_response: Vec<u8>,
        completion_status: i32,
        wedge_completion: bool,
        unwedged: bool,
        restart_unwedges: bool,
        restart_status: i32,
        spawn_status: i32,
    }

    impl StubState {
        fn reset() -> StubState {
            StubState {
                calls: Vec::new(),
                completion_response: Vec::new(),
                completion_status: STATUS_OK,
                wedge_completion: false,
                unwedged: false,
                restart_unwedges: false,
                restart_status: STATUS_OK,
                spawn_status: STATUS_OK,
            }
        }
    }

    fn stub() -> &'static Mutex<StubState> {
        static STUB: OnceLock<Mutex<StubState>> = OnceLock::new();
        STUB.get_or_init(|| Mutex::new(StubState::reset()))
    }

    fn serial() -> &'static Mutex<()> {
        static SERIAL: OnceLock<Mutex<()>> = OnceLock::new();
        SERIAL.get_or_init(|| Mutex::new(()))
    }

    /// Poison-resilient stub lock: a failed assertion elsewhere may have
    /// panicked while holding the guard, and that must not wedge sibling tests.
    fn guard() -> std::sync::MutexGuard<'static, StubState> {
        stub().lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn record(name: &'static str) {
        guard().calls.push(name);
    }

    fn write_out(out: *mut LsBuf, bytes: &[u8]) {
        if out.is_null() {
            return;
        }
        if bytes.is_empty() {
            // SAFETY: `out` is a valid out-param.
            unsafe {
                (*out).ptr = std::ptr::null_mut();
                (*out).len = 0;
            }
            return;
        }
        // SAFETY: `abi_alloc` returns a buffer of `bytes.len()` we own and copy into.
        let ptr = unsafe { abi_alloc(bytes.len() as u32) };
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr, bytes.len());
            (*out).ptr = ptr;
            (*out).len = bytes.len() as u32;
        }
    }

    unsafe extern "C" fn s_register_target(_p: *const u8, _l: u32) -> i32 {
        record("register_target");
        STATUS_OK
    }
    unsafe extern "C" fn s_did_open(_p: *const u8, _l: u32) -> i32 {
        record("did_open");
        STATUS_OK
    }
    unsafe extern "C" fn s_did_change(_p: *const u8, _l: u32) -> i32 {
        record("did_change");
        STATUS_OK
    }
    unsafe extern "C" fn s_did_close(_u: LsStr) -> i32 {
        record("did_close");
        STATUS_OK
    }
    unsafe extern "C" fn s_completion(_u: LsStr, _l: u32, _c: u32, out: *mut LsBuf) -> i32 {
        let (wedge, response, status) = {
            let mut s = guard();
            s.calls.push("completion");
            (
                s.wedge_completion,
                s.completion_response.clone(),
                s.completion_status,
            )
        };
        if wedge {
            // Block until a cooperative restart unwedges us, or a safety cap. A
            // non-cooperative wedge stays abandoned well past the recovery
            // window (the request deadline + cancel grace).
            let start = Instant::now();
            while start.elapsed() < Duration::from_secs(3) {
                if guard().unwedged {
                    break;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        write_out(out, &response);
        status
    }
    unsafe extern "C" fn s_query(_u: LsStr, _l: u32, _c: u32, _o: *mut LsBuf) -> i32 {
        record("query");
        STATUS_OK
    }
    unsafe extern "C" fn s_resolve(
        _t: LsStr,
        _s: LsStr,
        _ip: *const u8,
        _il: u32,
        _o: *mut LsBuf,
    ) -> i32 {
        record("completion_resolve");
        STATUS_OK
    }
    unsafe extern "C" fn s_plugin_status(_o: *mut LsBuf) -> i32 {
        record("plugin_status");
        STATUS_OK
    }
    unsafe extern "C" fn s_restart_instances() -> i32 {
        let mut s = guard();
        s.calls.push("restart_instances");
        if s.restart_unwedges {
            s.unwedged = true;
        }
        s.restart_status
    }
    unsafe extern "C" fn s_shutdown() -> i32 {
        record("shutdown");
        STATUS_OK
    }
    unsafe extern "C" fn s_spawn_dispatch(_g: u32) -> i32 {
        record("spawn_dispatch");
        guard().spawn_status
    }

    fn stub_vtable() -> NonNull<PcVtable> {
        let vt = PcVtable {
            abi_version: ls_pc_abi::ABI_VERSION,
            register_target: s_register_target,
            did_open: s_did_open,
            did_change: s_did_change,
            did_close: s_did_close,
            completion: s_completion,
            completion_resolve: s_resolve,
            hover: s_query,
            signature_help: s_query,
            definition: s_query,
            type_definition: s_query,
            prepare_rename: s_query,
            plugin_status: s_plugin_status,
            restart_instances: s_restart_instances,
            shutdown: s_shutdown,
            spawn_dispatch: s_spawn_dispatch,
        };
        NonNull::from(Box::leak(Box::new(vt)))
    }

    fn start_runtime() -> Arc<IslandRuntime> {
        let rt = IslandRuntime::new(stub_vtable());
        let d = Arc::clone(&rt);
        std::thread::spawn(move || d.enter_dispatch_worker());
        let c = Arc::clone(&rt);
        std::thread::spawn(move || c.enter_control_worker());
        // Let the initial workers attach.
        rt.wait_attached_after(0, Duration::from_secs(2));
        rt
    }

    fn test_timeouts() -> BackendTimeouts {
        BackendTimeouts {
            control: Duration::from_secs(2),
            attach: Duration::from_secs(2),
            replay: Duration::from_secs(2),
        }
    }

    fn supervisor(rt: Arc<IslandRuntime>) -> Supervisor<VtableBackend> {
        let hook: SpawnHook = Box::new(|rt: &Arc<IslandRuntime>| {
            let w = Arc::clone(rt);
            std::thread::spawn(move || w.enter_dispatch_worker());
        });
        let backend = VtableBackend::with_spawn_hook(rt, test_timeouts(), hook);
        // The request deadline + cancel grace are well under the wedge safety
        // cap, so a non-cooperative wedge is still stuck when recovery probes.
        Supervisor::new(
            backend,
            4,
            Duration::from_millis(60),
            Duration::from_millis(50),
        )
    }

    /// Poison-resilient serialization of the stub-using integration tests.
    fn serial_guard() -> std::sync::MutexGuard<'static, ()> {
        serial().lock().unwrap_or_else(|poison| poison.into_inner())
    }

    fn calls() -> Vec<&'static str> {
        guard().calls.clone()
    }

    fn query() -> PcRequest {
        PcRequest::Query {
            kind: QueryKind::Completion,
            uri: "file:///a.scala".to_string(),
            line: 1,
            character: 2,
        }
    }

    fn config() -> TargetConfig {
        TargetConfig {
            bsp_id: "a".to_string(),
            scala_version: "3.8.4".to_string(),
            classpath: vec![],
            scalac_options: vec![],
            source_dirs: vec![],
        }
    }

    #[test]
    fn concrete_backend_invokes_slots_serializes_and_propagates_status() {
        let _serial = serial_guard();
        *guard() = StubState::reset();
        guard().completion_response = b"items".to_vec();

        let mut sup = supervisor(start_runtime());

        // Lifecycle + query ops invoke the matching slots in order, serialized
        // (single dispatch worker), and the Rust-owned response is returned.
        sup.request(PcRequest::RegisterTarget {
            id: "a".to_string(),
            config: config(),
        })
        .unwrap();
        sup.request(PcRequest::DidOpen {
            target_id: "a".to_string(),
            uri: "file:///a.scala".to_string(),
            text: "package a".to_string(),
        })
        .unwrap();
        assert_eq!(sup.request(query()), Ok(b"items".to_vec()));

        assert_eq!(calls(), vec!["register_target", "did_open", "completion"]);

        // A nonzero PC status is a typed error, not a wedge.
        guard().completion_status = -4;
        assert_eq!(sup.request(query()), Err(PcError::Backend(-4)));
        assert_eq!(sup.generation(), 0);
        assert!(!sup.is_fatal());
    }

    #[test]
    fn non_cooperative_wedge_spawns_generation_replays_and_recovers() {
        let _serial = serial_guard();
        *guard() = StubState::reset();

        let mut sup = supervisor(start_runtime());

        sup.request(PcRequest::RegisterTarget {
            id: "a".to_string(),
            config: config(),
        })
        .unwrap();
        sup.request(PcRequest::DidOpen {
            target_id: "a".to_string(),
            uri: "file:///a.scala".to_string(),
            text: "package a".to_string(),
        })
        .unwrap();

        // A non-cooperative wedge: completion blocks and restart does not free it.
        guard().wedge_completion = true;
        assert_eq!(sup.request(query()), Err(PcError::RequestTimeout));

        // A fresh generation was spawned via the real spawn_dispatch slot and the
        // mirror replayed (register_target + did_open) on the new dispatch lane.
        assert_eq!(sup.generation(), 1);
        assert!(!sup.is_fatal());
        let recorded = calls();
        assert!(recorded.contains(&"restart_instances"));
        assert!(recorded.contains(&"spawn_dispatch"));

        // A subsequent completion works on the new generation without reopening
        // the buffer.
        {
            let mut s = guard();
            s.wedge_completion = false;
            s.completion_response = b"ok".to_vec();
        }
        assert_eq!(sup.request(query()), Ok(b"ok".to_vec()));
    }

    #[test]
    fn control_ops_route_to_the_control_lane_while_dispatch_is_wedged() {
        let _serial = serial_guard();
        {
            let mut s = guard();
            *s = StubState::reset();
            // A cooperative wedge: restart_instances (control lane) frees the
            // wedged dispatch op, proving control is served while dispatch busy.
            s.wedge_completion = true;
            s.restart_unwedges = true;
        }

        let mut sup = supervisor(start_runtime());
        assert_eq!(sup.request(query()), Err(PcError::RequestTimeout));
        // Recovered without a new generation: restart alone freed the lane.
        assert_eq!(sup.generation(), 0);
        assert!(!sup.is_fatal());
        let recorded = calls();
        assert!(recorded.contains(&"restart_instances"));
        assert!(!recorded.contains(&"spawn_dispatch"));
    }

    #[test]
    fn failed_spawn_dispatch_status_is_fatal() {
        let _serial = serial_guard();
        {
            let mut s = guard();
            *s = StubState::reset();
            s.wedge_completion = true;
            s.spawn_status = STATUS_INTERNAL;
        }

        let mut sup = supervisor(start_runtime());
        assert_eq!(sup.request(query()), Err(PcError::RequestTimeout));
        // spawn_dispatch returned nonzero → island-fatal.
        assert!(sup.is_fatal());
    }
}
