//! The Rust side of the embedded-JVM presentation-compiler island lifecycle.
//!
//! Boot is the plan's zero-JNIEnv protocol: the first PC request `dlopen`s
//! libjvm and calls the single boot symbol `JNI_CreateJavaVM` ([`boot`]) with
//! the [`RustVtable`] address as the `-javaagent` argument. The premain fires
//! inside that call, mirrors the ABI layout, and downcalls `register_pc_vtable`
//! plus enters the loaned dispatch threads; the Rust side rendezvouses on that
//! registration under a deadline, and on timeout fails with a typed boot error
//! carrying the captured island log for the doctor.
//!
//! Steady state is driven by the [`watchdog::Supervisor`]: PC requests serialize
//! on the loaned dispatch lane under a per-request deadline, a wedge fails the
//! request typed and escalates the recovery ladder (cancel → restart_instances
//! → a fresh dispatch generation with the [`mirror`]ed targets/buffers replayed
//! → island-fatal past the abandoned-generation cap). Before any of this the
//! [`stdout_guard`] keeps island/plugin writes to fd 1 off the LSP stream.

#![forbid(unsafe_op_in_unsafe_fn)]

pub mod boot;
pub mod dispatch;
mod jni;
pub mod mirror;
pub mod stdout_guard;
pub mod watchdog;

use std::path::{Path, PathBuf};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use ls_pc_abi::abi::{LsBuf, LsStr};
use ls_pc_abi::memory::{abi_alloc, abi_free};
use ls_pc_abi::{
    PcVtable, RustVtable, ABI_VERSION, LAYOUT_CANARY, STATUS_ABI_MISMATCH, STATUS_BAD_ARG,
    STATUS_OK,
};

pub use boot::{libjvm_mapped, BootError};
pub use stdout_guard::StdoutGuard;

// ---------------------------------------------------------------------------
// Boot rendezvous (the premain registers the PC vtable and enters the loaned
// dispatch threads; the boot caller blocks here until both land, or times out).
// ---------------------------------------------------------------------------

struct RendezvousState {
    pc_vtable_addr: Option<usize>,
    dispatch_ready: bool,
    island_log: Vec<String>,
}

struct Rendezvous {
    state: Mutex<RendezvousState>,
    cv: Condvar,
}

static RENDEZVOUS: OnceLock<Rendezvous> = OnceLock::new();

fn rendezvous() -> &'static Rendezvous {
    RENDEZVOUS.get_or_init(|| Rendezvous {
        state: Mutex::new(RendezvousState {
            pc_vtable_addr: None,
            dispatch_ready: false,
            island_log: Vec::new(),
        }),
        cv: Condvar::new(),
    })
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
/// dispatch lane and signals dispatch readiness; the live op pump attaches
/// through the [`watchdog::Supervisor`] backend.
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
    if status != STATUS_OK {
        return status;
    }
    let reg = rendezvous();
    let mut st = reg.state.lock().expect("rendezvous state lock");
    st.pc_vtable_addr = Some(pc as usize);
    reg.cv.notify_all();
    STATUS_OK
}

fn dispatch_loop_inner(worker_index: i32) {
    if worker_index == 0 {
        let reg = rendezvous();
        if let Ok(mut st) = reg.state.lock() {
            st.dispatch_ready = true;
            reg.cv.notify_all();
        }
    }
    // The loaned thread is owned by the island for the process lifetime.
    loop {
        std::thread::park();
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

/// Where the embedded JVM and PC-host assembly live, and how long to wait for
/// the premain to register.
pub struct IslandConfig<'a> {
    /// `$JAVA_HOME/lib/server/libjvm.so`.
    pub libjvm: &'a Path,
    /// The PC-host agent jar (carries the premain; also on the class path).
    pub agent_jar: &'a Path,
    /// Any additional class-path entries (the PC-host assembly).
    pub extra_classpath: &'a [PathBuf],
    /// Deadline for the premain to complete `register_pc_vtable` + dispatch.
    pub rendezvous_timeout: Duration,
}

/// Boot the embedded JVM and block until the premain registers the PC vtable and
/// the loaned dispatch lane is ready, or the rendezvous deadline elapses.
///
/// The [`StdoutGuard`] must already be installed by the caller (the LSP server),
/// so fd 1 is aliased to stderr before the JVM can grab it and the server keeps
/// the private stdout for the protocol stream.
pub fn boot_island(config: &IslandConfig) -> Result<(), BootError> {
    let vtable_addr = std::ptr::addr_of!(RUST_VTABLE) as usize;
    let options = boot::boot_options(config.agent_jar, config.extra_classpath, vtable_addr);
    boot::create_java_vm(config.libjvm, &options).map_err(BootError::Boot)?;
    wait_for_registration(config.rendezvous_timeout)
}

fn wait_for_registration(timeout: Duration) -> Result<(), BootError> {
    let reg = rendezvous();
    let deadline = Instant::now() + timeout;
    let mut st = reg.state.lock().expect("rendezvous state lock");
    loop {
        if st.pc_vtable_addr.is_some() && st.dispatch_ready {
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(BootError::RendezvousTimeout {
                island_log: st.island_log.clone(),
            });
        }
        let (guard, res) = reg.cv.wait_timeout(st, deadline - now).expect("cv wait");
        st = guard;
        if res.timed_out() && !(st.pc_vtable_addr.is_some() && st.dispatch_ready) {
            return Err(BootError::RendezvousTimeout {
                island_log: st.island_log.clone(),
            });
        }
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

        // A vtable whose ABI version does not match is refused.
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
    fn rust_vtable_carries_the_versioned_layout_contract() {
        // The island validates these two words before mirroring the layout.
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
        use ls_pc_abi::abi::LsStr;

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
