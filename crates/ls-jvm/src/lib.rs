//! The Rust side of the embedded-JVM presentation-compiler island lifecycle.
//!
//! Boot is the plan's zero-JNIEnv protocol: the first PC request `dlopen`s
//! libjvm and calls the single boot symbol `JNI_CreateJavaVM` ([`boot`]) with
//! the [`RustVtable`] address as the `-javaagent` argument. The premain fires
//! inside that call, mirrors the ABI layout, downcalls `register_pc_vtable`,
//! and enters the loaned dispatch threads; the Rust side rendezvouses on that
//! registration under a deadline. A bad-ABI registration fails fast with a
//! typed error; a silent premain times out with the captured island log.
//!
//! Steady state is driven by the [`watchdog::Supervisor`] over the
//! [`backend::VtableBackend`]: PC requests serialize on the loaned dispatch
//! lane (worker 0) under a per-request deadline, control ops run on worker 1,
//! and a wedge escalates the recovery ladder (restart_instances → a fresh
//! dispatch generation via `spawn_dispatch` with the [`mirror`]ed
//! targets/buffers replayed → island-fatal past the abandoned-generation cap).
//! Before any of this the [`stdout_guard`] keeps island/plugin writes to fd 1
//! off the LSP stream.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod backend;
pub mod boot;
pub mod dispatch;
mod jni;
pub mod mirror;
pub mod stdout_guard;
pub mod watchdog;

use std::path::{Path, PathBuf};
use std::ptr::NonNull;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ls_pc_abi::abi::{LsBuf, LsStr};
use ls_pc_abi::memory::{abi_alloc, abi_free};
use ls_pc_abi::{
    PcVtable, RustVtable, ABI_VERSION, LAYOUT_CANARY, STATUS_ABI_MISMATCH, STATUS_BAD_ARG,
    STATUS_OK,
};

use backend::{IslandRuntime, VtableBackend};
use watchdog::Supervisor;

pub use boot::{libjvm_mapped, BootError};
pub use stdout_guard::StdoutGuard;

// ---------------------------------------------------------------------------
// Boot rendezvous (the premain registers the PC vtable and enters the loaned
// dispatch threads; the boot caller blocks here until both land, the
// registration fails terminally, or the deadline elapses).
// ---------------------------------------------------------------------------

struct RendezvousState {
    registered: bool,
    dispatch_ready: bool,
    /// A terminal registration failure status (e.g. an ABI mismatch), so the
    /// boot caller fails fast rather than waiting out the rendezvous deadline.
    terminal_failure: Option<i32>,
    island_log: Vec<String>,
}

struct Rendezvous {
    state: Mutex<RendezvousState>,
    cv: Condvar,
}

static RENDEZVOUS: OnceLock<Rendezvous> = OnceLock::new();
static ISLAND_RUNTIME: OnceLock<Arc<IslandRuntime>> = OnceLock::new();

fn rendezvous() -> &'static Rendezvous {
    RENDEZVOUS.get_or_init(|| Rendezvous {
        state: Mutex::new(RendezvousState {
            registered: false,
            dispatch_ready: false,
            terminal_failure: None,
            island_log: Vec::new(),
        }),
        cv: Condvar::new(),
    })
}

fn island_runtime() -> &'static Arc<IslandRuntime> {
    ISLAND_RUNTIME.get().expect("island runtime not registered")
}

/// The island log captured through the `log` downcall (surfaced by the doctor
/// on a boot failure).
pub fn island_log() -> Vec<String> {
    rendezvous()
        .state
        .lock()
        .map(|st| st.island_log.clone())
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// The Rust vtable handed to the premain. Every export is `catch_unwind`-wrapped
// so a Rust panic never unwinds across the FFI boundary.
// ---------------------------------------------------------------------------

/// Island → Rust structured log capture.
unsafe extern "C" fn vt_log(level: i32, ptr: *const u8, len: u32) {
    let _ = std::panic::catch_unwind(|| {
        let msg = read_utf8(ptr, len);
        if let Ok(mut st) = rendezvous().state.lock() {
            st.island_log.push(format!("[{level}] {msg}"));
        }
    });
}

/// Island → Rust registration of the PC vtable; signals the rendezvous.
unsafe extern "C" fn vt_register_pc_vtable(pc: *const PcVtable) -> i32 {
    match std::panic::catch_unwind(|| register_pc_vtable_inner(pc)) {
        Ok(status) => status,
        Err(_) => ls_pc_abi::STATUS_PANIC,
    }
}

/// Entry point for a Java-loaned thread; it never returns. Worker 0 is the
/// dispatch lane, worker 1 the control lane.
unsafe extern "C" fn vt_pc_dispatch_loop(worker_index: i32) {
    let _ = std::panic::catch_unwind(|| dispatch_loop_inner(worker_index));
}

/// Index-backed cross-file go-to-definition callback. Without a resolver wired
/// the response is an empty locations buffer (no cross-file definition).
unsafe extern "C" fn vt_symbol_definition(
    _symbol: LsStr,
    _from_uri: LsStr,
    out: *mut LsBuf,
) -> i32 {
    match std::panic::catch_unwind(|| symbol_definition_inner(out)) {
        Ok(status) => status,
        Err(_) => ls_pc_abi::STATUS_PANIC,
    }
}

/// Validates an incoming PC vtable registration: non-null and matching ABI
/// version. Split out so the registration contract is unit-tested directly.
fn validate_pc_registration(pc: *const PcVtable) -> i32 {
    if pc.is_null() {
        return STATUS_BAD_ARG;
    }
    // SAFETY: the island passes a valid `PcVtable` for the duration of the call;
    // `abi_version` is the first field, read unaligned to avoid any assumption.
    let abi = unsafe { std::ptr::addr_of!((*pc).abi_version).read_unaligned() };
    if abi != ABI_VERSION {
        return STATUS_ABI_MISMATCH;
    }
    STATUS_OK
}

fn register_pc_vtable_inner(pc: *const PcVtable) -> i32 {
    let status = validate_pc_registration(pc);
    let reg = rendezvous();
    if status != STATUS_OK {
        // Record a terminal failure so the boot caller returns immediately.
        let mut st = reg.state.lock().expect("rendezvous state lock");
        st.terminal_failure = Some(status);
        reg.cv.notify_all();
        return status;
    }
    // Build the runtime from the validated non-null vtable (its address is
    // process-stable; the island owns the memory for the process lifetime).
    let handle = NonNull::new(pc as *mut PcVtable).expect("validated non-null");
    let _ = ISLAND_RUNTIME.set(IslandRuntime::new(handle));

    let mut st = reg.state.lock().expect("rendezvous state lock");
    st.registered = true;
    reg.cv.notify_all();
    STATUS_OK
}

fn dispatch_loop_inner(worker_index: i32) {
    let rt = island_runtime();
    if worker_index == 0 {
        if let Ok(mut st) = rendezvous().state.lock() {
            st.dispatch_ready = true;
            rendezvous().cv.notify_all();
        }
        rt.enter_dispatch_worker();
    } else {
        rt.enter_control_worker();
    }
}

fn symbol_definition_inner(out: *mut LsBuf) -> i32 {
    if out.is_null() {
        return STATUS_BAD_ARG;
    }
    // SAFETY: `out` is a valid `LsBuf` out-param for the call; an empty buffer
    // (null ptr, zero len) is the "no cross-file definition" response.
    unsafe {
        (*out).ptr = std::ptr::null_mut();
        (*out).len = 0;
    }
    STATUS_OK
}

fn read_utf8(ptr: *const u8, len: u32) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    // SAFETY: the Java FFM caller passes a valid `ptr`/`len` for the call.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// The process's Rust vtable. Its address is the `-javaagent` argument; the
/// island mirrors this layout and recomputes `layout_canary`, refusing to
/// register on a mismatch (which surfaces as a rendezvous timeout).
static RUST_VTABLE: RustVtable = RustVtable {
    abi_version: ABI_VERSION,
    layout_canary: LAYOUT_CANARY,
    alloc: abi_alloc,
    free: abi_free,
    log: vt_log,
    register_pc_vtable: vt_register_pc_vtable,
    pc_dispatch_loop: vt_pc_dispatch_loop,
    symbol_definition: vt_symbol_definition,
};

// ---------------------------------------------------------------------------
// Boot orchestration.
// ---------------------------------------------------------------------------

/// Where the embedded JVM and PC-host assembly live, the rendezvous deadline,
/// and the steady-state supervisor tuning.
pub struct IslandConfig<'a> {
    /// `$JAVA_HOME/lib/server/libjvm.so`.
    pub libjvm: &'a Path,
    /// The PC-host agent jar (carries the premain; also on the class path).
    pub agent_jar: &'a Path,
    /// Any additional class-path entries (the PC-host assembly).
    pub extra_classpath: &'a [PathBuf],
    /// The workspace root, when the LS runs inside one; handed to the island as
    /// `-Dls.pc.host.workspace` so the premain loads the per-workspace PC
    /// plugin config. `None` runs the island with config-less settings.
    pub workspace_root: Option<&'a Path>,
    /// Deadline for the premain to complete `register_pc_vtable` + dispatch.
    pub rendezvous_timeout: Duration,
    /// Abandoned dispatch generations tolerated before the island is fatal.
    pub max_abandoned_generations: u32,
    /// Per-request dispatch deadline (the `orTimeout` semantics).
    pub request_deadline: Duration,
    /// Grace period after `restart_instances` to see the dispatch lane free.
    pub cancel_grace: Duration,
}

/// Boot the embedded JVM, block until the premain registers the PC vtable and
/// the loaned dispatch lane is ready, and return the [`Supervisor`] the LSP
/// server drives. Fails fast on a bad-ABI registration; times out (with the
/// captured island log) on a silent premain.
///
/// The [`StdoutGuard`] must already be installed by the caller (the LSP server),
/// so fd 1 is aliased to stderr before the JVM can grab it and the server keeps
/// the private stdout for the protocol stream.
pub fn boot_island(config: &IslandConfig) -> Result<Supervisor<VtableBackend>, BootError> {
    let vtable_addr = std::ptr::addr_of!(RUST_VTABLE) as usize;
    let options = boot::boot_options(
        config.agent_jar,
        config.extra_classpath,
        vtable_addr,
        config.workspace_root,
    );
    boot::create_java_vm(config.libjvm, &options).map_err(BootError::Boot)?;
    wait_for_registration(config.rendezvous_timeout)?;

    let backend = VtableBackend::new(Arc::clone(island_runtime()));
    Ok(Supervisor::new(
        backend,
        config.max_abandoned_generations,
        config.request_deadline,
        config.cancel_grace,
    ))
}

fn terminal_boot_error(status: i32) -> BootError {
    if status == STATUS_ABI_MISMATCH {
        BootError::AbiMismatch
    } else {
        BootError::Boot(format!("island refused registration: status {status}"))
    }
}

fn wait_for_registration(timeout: Duration) -> Result<(), BootError> {
    let reg = rendezvous();
    let deadline = Instant::now() + timeout;
    let mut st = reg.state.lock().expect("rendezvous state lock");
    loop {
        if let Some(status) = st.terminal_failure {
            return Err(terminal_boot_error(status));
        }
        if st.registered && st.dispatch_ready {
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(BootError::RendezvousTimeout {
                island_log: st.island_log.clone(),
            });
        }
        let (guard, _res) = reg.cv.wait_timeout(st, deadline - now).expect("cv wait");
        st = guard;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_utf8_handles_null_and_bytes() {
        assert_eq!(read_utf8(std::ptr::null(), 5), "");
        let b = b"island up";
        assert_eq!(read_utf8(b.as_ptr(), b.len() as u32), "island up");
        assert_eq!(read_utf8(b.as_ptr(), 0), "");
    }

    #[test]
    fn registration_rejects_null_and_bad_abi() {
        assert_eq!(validate_pc_registration(std::ptr::null()), STATUS_BAD_ARG);

        let mut bad = pc_vtable_stub();
        bad.abi_version = ABI_VERSION + 1;
        assert_eq!(
            validate_pc_registration(std::ptr::addr_of!(bad)),
            STATUS_ABI_MISMATCH
        );

        let good = pc_vtable_stub();
        assert_eq!(
            validate_pc_registration(std::ptr::addr_of!(good)),
            STATUS_OK
        );
    }

    #[test]
    fn bad_abi_registration_fails_fast_without_timeout() {
        // A bad-ABI registration records a terminal failure and notifies the
        // rendezvous, so the boot caller returns `AbiMismatch` immediately
        // rather than waiting out the (here 10s) rendezvous deadline.
        let mut bad = pc_vtable_stub();
        bad.abi_version = ABI_VERSION + 1;
        assert_eq!(
            register_pc_vtable_inner(std::ptr::addr_of!(bad)),
            STATUS_ABI_MISMATCH
        );

        let start = Instant::now();
        let err = wait_for_registration(Duration::from_secs(10)).unwrap_err();
        assert!(matches!(err, BootError::AbiMismatch));
        assert!(start.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn rust_vtable_carries_the_versioned_layout_contract() {
        assert_eq!(RUST_VTABLE.abi_version, ABI_VERSION);
        assert_eq!(RUST_VTABLE.layout_canary, LAYOUT_CANARY);
    }

    #[test]
    fn symbol_definition_writes_an_empty_buffer_without_a_resolver() {
        let mut out = LsBuf {
            ptr: std::ptr::dangling_mut::<u8>(),
            len: 7,
        };
        assert_eq!(
            symbol_definition_inner(std::ptr::null_mut()),
            STATUS_BAD_ARG
        );
        assert_eq!(
            symbol_definition_inner(std::ptr::addr_of_mut!(out)),
            STATUS_OK
        );
        assert!(out.ptr.is_null());
        assert_eq!(out.len, 0);
    }

    /// A minimal valid `PcVtable` for registration-contract tests: correct ABI
    /// version, every op pointed at a trivial stub.
    fn pc_vtable_stub() -> PcVtable {
        unsafe extern "C" fn req(_p: *const u8, _l: u32) -> i32 {
            STATUS_OK
        }
        unsafe extern "C" fn uri(_u: LsStr) -> i32 {
            STATUS_OK
        }
        unsafe extern "C" fn query(_u: LsStr, _l: u32, _c: u32, _o: *mut LsBuf) -> i32 {
            STATUS_OK
        }
        unsafe extern "C" fn resolve(
            _t: LsStr,
            _s: LsStr,
            _ip: *const u8,
            _il: u32,
            _o: *mut LsBuf,
        ) -> i32 {
            STATUS_OK
        }
        unsafe extern "C" fn status_out(_o: *mut LsBuf) -> i32 {
            STATUS_OK
        }
        unsafe extern "C" fn void_op() -> i32 {
            STATUS_OK
        }
        unsafe extern "C" fn spawn(_g: u32) -> i32 {
            STATUS_OK
        }

        PcVtable {
            abi_version: ABI_VERSION,
            register_target: req,
            did_open: req,
            did_change: req,
            did_close: uri,
            completion: query,
            completion_resolve: resolve,
            hover: query,
            signature_help: query,
            definition: query,
            type_definition: query,
            prepare_rename: query,
            plugin_status: status_out,
            restart_instances: void_op,
            shutdown: void_op,
            spawn_dispatch: spawn,
        }
    }
}
