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
use ls_pc_abi::memory::{abi_alloc, abi_free, write_response};
use ls_pc_abi::payloads::{LocationsResult, MethodHitsResult};
use ls_pc_abi::{
    PcVtable, RustVtable, ABI_VERSION, LAYOUT_CANARY, STATUS_ABI_MISMATCH, STATUS_ALLOC,
    STATUS_BAD_ARG, STATUS_INTERNAL, STATUS_OK, STATUS_PANIC,
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

/// The island's cross-file go-to-definition resolver: `(semanticdb_symbol,
/// requesting_file_uri) -> definition locations`. This is the read-only,
/// snapshot-backed callback behind the PC `SymbolSearch.definition` seam; the
/// server installs one over the immutable index snapshot before boot.
pub type SymbolDefinitionResolver = dyn Fn(&str, &str) -> LocationsResult + Send + Sync;

static SYMBOL_DEFINITION_RESOLVER: OnceLock<Box<SymbolDefinitionResolver>> = OnceLock::new();

/// Install the resolver the island downcalls for cross-file go-to-definition.
/// Idempotent per process: call once before [`boot_island`]; a later call is
/// ignored (only one JVM/island boots per process). Without one, the callback
/// answers an empty locations buffer.
pub fn install_symbol_definition_resolver(resolver: Box<SymbolDefinitionResolver>) {
    let _ = SYMBOL_DEFINITION_RESOLVER.set(resolver);
}

/// The island's workspace method search resolver: `(query, bsp_target_id) ->
/// method hits`. This is the read-only, snapshot-backed callback behind the PC
/// `SymbolSearch.searchMethods` seam (member-mode workspace extension-method
/// discovery); the server installs one over the immutable index snapshot before
/// boot, next to the [`SymbolDefinitionResolver`].
pub type SearchMethodsResolver = dyn Fn(&str, &str) -> MethodHitsResult + Send + Sync;

static SEARCH_METHODS_RESOLVER: OnceLock<Box<SearchMethodsResolver>> = OnceLock::new();

/// Install the resolver the island downcalls for workspace method search.
/// Idempotent per process, exactly like
/// [`install_symbol_definition_resolver`]. Without one, the callback answers an
/// empty method-hits buffer.
pub fn install_search_methods_resolver(resolver: Box<SearchMethodsResolver>) {
    let _ = SEARCH_METHODS_RESOLVER.set(resolver);
}

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

/// Index-backed cross-file go-to-definition callback. Decodes the incoming
/// SemanticDB symbol + requesting `file://` uri, runs the installed resolver
/// (an empty response when none is wired), and writes an encoded
/// `LocationsResult` into `out`. Rust owns the response buffer; the island frees
/// it through the `free` slot.
unsafe extern "C" fn vt_symbol_definition(symbol: LsStr, from_uri: LsStr, out: *mut LsBuf) -> i32 {
    match std::panic::catch_unwind(|| {
        let symbol = match read_utf8_strict(symbol.ptr, symbol.len) {
            Ok(s) => s,
            Err(status) => return status,
        };
        let from_uri = match read_utf8_strict(from_uri.ptr, from_uri.len) {
            Ok(s) => s,
            Err(status) => return status,
        };
        run_resolver(
            SYMBOL_DEFINITION_RESOLVER.get().map(|r| r.as_ref()),
            &symbol,
            &from_uri,
            out,
        )
    }) {
        Ok(status) => status,
        Err(_) => STATUS_PANIC,
    }
}

/// Index-backed workspace method search callback. Decodes the incoming query +
/// requesting build-target id, runs the installed resolver (an empty response
/// when none is wired), and writes an encoded `MethodHitsResult` into `out`.
/// Rust owns the response buffer; the island frees it through the `free` slot.
unsafe extern "C" fn vt_search_methods(query: LsStr, bsp_target_id: LsStr, out: *mut LsBuf) -> i32 {
    match std::panic::catch_unwind(|| {
        let query = match read_utf8_strict(query.ptr, query.len) {
            Ok(s) => s,
            Err(status) => return status,
        };
        let bsp_target_id = match read_utf8_strict(bsp_target_id.ptr, bsp_target_id.len) {
            Ok(s) => s,
            Err(status) => return status,
        };
        run_search_resolver(
            SEARCH_METHODS_RESOLVER.get().map(|r| r.as_ref()),
            &query,
            &bsp_target_id,
            out,
        )
    }) {
        Ok(status) => status,
        Err(_) => STATUS_PANIC,
    }
}

/// Validates an incoming PC vtable registration: non-null pointer, matching ABI
/// version, and every op slot a non-null function pointer. Split out so the
/// registration contract is unit-tested directly.
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
    if !all_pc_slots_non_null(pc) {
        return STATUS_BAD_ARG;
    }
    STATUS_OK
}

/// Every PC vtable op slot must be a non-null function pointer. The island builds
/// all 15 upcall stubs, so a null slot means a malformed registration; reject it
/// here rather than reading a null `fn`-typed field later (which is UB). Slots
/// are read as raw pointer bits — never materialized as an invalid `fn` value.
fn all_pc_slots_non_null(pc: *const PcVtable) -> bool {
    use std::mem::size_of;
    // `#[repr(C)]`, 64-bit ABI (`assert!(size_of::<*const c_void>() == 8)` in
    // `ls_pc_abi`): `abi_version` (u64) is the first pointer-sized word, followed
    // by the pointer-sized op slots with no padding. Counting words keeps this
    // correct if a slot is added (the whole struct is covered).
    let words = size_of::<PcVtable>() / size_of::<usize>();
    let base = pc as *const usize;
    // SAFETY: `pc` is non-null and points at a `PcVtable` for the call; word `i`
    // for `i in 1..words` is one op slot, read as raw bits.
    (1..words).all(|i| unsafe { base.add(i).read_unaligned() } != 0)
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

/// Runs the resolver (if any) and writes an encoded `LocationsResult` into
/// `out`: a missing resolver answers empty locations, a panicking resolver is
/// contained to `STATUS_PANIC`, a null `out` is `STATUS_BAD_ARG`, and an
/// allocation failure is `STATUS_ALLOC`. Split out so the containment contract
/// is unit-tested without booting a JVM.
fn run_resolver(
    resolver: Option<&SymbolDefinitionResolver>,
    symbol: &str,
    from_uri: &str,
    out: *mut LsBuf,
) -> i32 {
    if out.is_null() {
        return STATUS_BAD_ARG;
    }
    let result = match resolver {
        None => LocationsResult {
            locations: Vec::new(),
        },
        Some(resolve) => {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                resolve(symbol, from_uri)
            })) {
                Ok(result) => result,
                Err(_) => return STATUS_PANIC,
            }
        }
    };
    let payload = match result.encode() {
        Ok(bytes) => bytes,
        // A response too large to represent in the ABI is an internal failure,
        // not a truncated buffer handed across the boundary.
        Err(_) => return STATUS_INTERNAL,
    };
    // SAFETY: `out` is a valid writable `LsBuf` for the call (checked non-null).
    if unsafe { write_response(&payload, out) } {
        STATUS_OK
    } else {
        STATUS_ALLOC
    }
}

/// Runs the search-methods resolver (if any) and writes an encoded
/// `MethodHitsResult` into `out`, with the same containment contract as
/// [`run_resolver`]: a missing resolver answers empty hits, a panicking
/// resolver is contained to `STATUS_PANIC`, a null `out` is `STATUS_BAD_ARG`,
/// and an allocation failure is `STATUS_ALLOC`. Split out so the containment
/// contract is unit-tested without booting a JVM.
fn run_search_resolver(
    resolver: Option<&SearchMethodsResolver>,
    query: &str,
    bsp_target_id: &str,
    out: *mut LsBuf,
) -> i32 {
    if out.is_null() {
        return STATUS_BAD_ARG;
    }
    let result = match resolver {
        None => MethodHitsResult { hits: Vec::new() },
        Some(resolve) => {
            match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                resolve(query, bsp_target_id)
            })) {
                Ok(result) => result,
                Err(_) => return STATUS_PANIC,
            }
        }
    };
    let payload = match result.encode() {
        Ok(bytes) => bytes,
        // A response too large to represent in the ABI is an internal failure,
        // not a truncated buffer handed across the boundary.
        Err(_) => return STATUS_INTERNAL,
    };
    // SAFETY: `out` is a valid writable `LsBuf` for the call (checked non-null).
    if unsafe { write_response(&payload, out) } {
        STATUS_OK
    } else {
        STATUS_ALLOC
    }
}

fn read_utf8(ptr: *const u8, len: u32) -> String {
    if ptr.is_null() || len == 0 {
        return String::new();
    }
    // SAFETY: the Java FFM caller passes a valid `ptr`/`len` for the call.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    String::from_utf8_lossy(bytes).into_owned()
}

/// Like [`read_utf8`] but rejects a null pointer paired with a positive length
/// (a malformed borrowed string) as `STATUS_BAD_ARG` rather than silently
/// treating it as empty. Used for the `symbol_definition` and `search_methods`
/// arguments, which must be well-formed; the lenient [`read_utf8`] stays for
/// best-effort log capture.
/// This mirrors the island-side `readLsStr`, which rejects the same shape.
fn read_utf8_strict(ptr: *const u8, len: u32) -> Result<String, i32> {
    if len != 0 && ptr.is_null() {
        return Err(STATUS_BAD_ARG);
    }
    Ok(read_utf8(ptr, len))
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
    search_methods: vt_search_methods,
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
    /// Extra `-D`/JVM options for the boot (tuning, or a test fault property).
    /// Inserted before the `-javaagent`; empty for the ordinary boot.
    pub extra_jvm_options: &'a [String],
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
        config.extra_jvm_options,
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
    fn read_utf8_strict_rejects_a_null_pointer_with_positive_length() {
        // A null pointer paired with a positive length is malformed, not empty.
        assert_eq!(read_utf8_strict(std::ptr::null(), 5), Err(STATUS_BAD_ARG));
        // (null, 0) is a valid empty string; real bytes decode normally.
        assert_eq!(read_utf8_strict(std::ptr::null(), 0), Ok(String::new()));
        let b = b"pkg/A#";
        assert_eq!(
            read_utf8_strict(b.as_ptr(), b.len() as u32),
            Ok("pkg/A#".to_string())
        );
    }

    #[test]
    fn vt_search_methods_rejects_a_null_argument_pointer() {
        // Same containment contract as the definition slot: a borrowed argument
        // with a null pointer + positive length maps to a typed bad-arg status.
        let mut out = LsBuf {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let bad = LsStr {
            ptr: std::ptr::null(),
            len: 5,
        };
        let empty = LsStr {
            ptr: std::ptr::null(),
            len: 0,
        };
        // SAFETY: `out` is a valid writable LsBuf; the args are borrowed structs.
        let status = unsafe { vt_search_methods(bad, empty, std::ptr::addr_of_mut!(out)) };
        assert_eq!(status, STATUS_BAD_ARG);
    }

    #[test]
    fn vt_symbol_definition_rejects_a_null_argument_pointer() {
        // A borrowed argument with a null pointer + positive length must map to a
        // typed bad-arg status, not silently resolve for an empty symbol.
        let mut out = LsBuf {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        let bad = LsStr {
            ptr: std::ptr::null(),
            len: 5,
        };
        let empty = LsStr {
            ptr: std::ptr::null(),
            len: 0,
        };
        // SAFETY: `out` is a valid writable LsBuf; the args are borrowed structs.
        let status = unsafe { vt_symbol_definition(bad, empty, std::ptr::addr_of_mut!(out)) };
        assert_eq!(status, STATUS_BAD_ARG);
    }

    #[test]
    fn registration_rejects_a_null_pc_slot() {
        // A vtable with the correct ABI version but a null op slot must be
        // refused — reading/calling a null `fn` slot later would be UB.
        let words = std::mem::size_of::<PcVtable>() / std::mem::size_of::<usize>();
        let mut raw = vec![7usize; words]; // non-null filler in every slot
        raw[0] = ABI_VERSION as usize; // the abi_version word
        assert_eq!(
            validate_pc_registration(raw.as_ptr() as *const PcVtable),
            STATUS_OK
        );
        raw[words - 1] = 0; // null the last op slot (spawn_dispatch)
        assert_eq!(
            validate_pc_registration(raw.as_ptr() as *const PcVtable),
            STATUS_BAD_ARG
        );
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

    fn decode_out(out: &LsBuf) -> LocationsResult {
        let bytes = if out.ptr.is_null() || out.len == 0 {
            Vec::new()
        } else {
            // SAFETY: a successful `write_response` left `ptr`/`len` valid.
            unsafe { std::slice::from_raw_parts(out.ptr, out.len as usize).to_vec() }
        };
        LocationsResult::decode(&bytes).expect("decode locations")
    }

    fn free_out(out: &mut LsBuf) {
        if !out.ptr.is_null() && out.len > 0 {
            // SAFETY: `ptr`/`len` came from `abi_alloc` via `write_response`.
            unsafe { abi_free(out.ptr, out.len) };
            out.ptr = std::ptr::null_mut();
            out.len = 0;
        }
    }

    #[test]
    fn run_resolver_rejects_null_out_and_answers_empty_without_a_resolver() {
        assert_eq!(
            run_resolver(None, "sym", "uri", std::ptr::null_mut()),
            STATUS_BAD_ARG
        );
        let mut out = LsBuf {
            ptr: std::ptr::dangling_mut::<u8>(),
            len: 7,
        };
        assert_eq!(
            run_resolver(None, "sym", "uri", std::ptr::addr_of_mut!(out)),
            STATUS_OK
        );
        // With no resolver the response is a decodable empty locations buffer.
        assert!(decode_out(&out).locations.is_empty());
        free_out(&mut out);
    }

    #[test]
    fn run_resolver_writes_the_resolved_locations_and_frees_clean() {
        use ls_pc_abi::payloads::{origin, Location, Rng};

        let seen = Arc::new(Mutex::new(None));
        let captured = seen.clone();
        let resolver: Box<SymbolDefinitionResolver> =
            Box::new(move |symbol: &str, from_uri: &str| {
                *captured.lock().unwrap() = Some((symbol.to_string(), from_uri.to_string()));
                LocationsResult {
                    locations: vec![Location {
                        uri: "file:///w/A.scala".to_string(),
                        range: Rng {
                            start_line: 1,
                            start_character: 2,
                            end_line: 1,
                            end_character: 5,
                        },
                        origin: origin::WORKSPACE,
                    }],
                }
            });

        let before = ls_pc_abi::memory::live_allocations();
        let mut out = LsBuf {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        assert_eq!(
            run_resolver(
                Some(resolver.as_ref()),
                "pkg/A#",
                "file:///w/B.scala",
                std::ptr::addr_of_mut!(out),
            ),
            STATUS_OK
        );
        // The resolver saw exactly the decoded arguments.
        assert_eq!(
            *seen.lock().unwrap(),
            Some(("pkg/A#".to_string(), "file:///w/B.scala".to_string()))
        );
        let decoded = decode_out(&out);
        assert_eq!(decoded.locations.len(), 1);
        assert_eq!(decoded.locations[0].uri, "file:///w/A.scala");
        assert_eq!(decoded.locations[0].range.start_line, 1);
        assert_eq!(decoded.locations[0].origin, origin::WORKSPACE);
        free_out(&mut out);
        assert_eq!(
            ls_pc_abi::memory::live_allocations(),
            before,
            "the response buffer is freed"
        );
    }

    fn decode_hits_out(out: &LsBuf) -> MethodHitsResult {
        let bytes = if out.ptr.is_null() || out.len == 0 {
            Vec::new()
        } else {
            // SAFETY: a successful `write_response` left `ptr`/`len` valid.
            unsafe { std::slice::from_raw_parts(out.ptr, out.len as usize).to_vec() }
        };
        MethodHitsResult::decode(&bytes).expect("decode method hits")
    }

    #[test]
    fn run_search_resolver_rejects_null_out_and_answers_empty_without_a_resolver() {
        assert_eq!(
            run_search_resolver(None, "incr", "root/t", std::ptr::null_mut()),
            STATUS_BAD_ARG
        );
        let mut out = LsBuf {
            ptr: std::ptr::dangling_mut::<u8>(),
            len: 7,
        };
        assert_eq!(
            run_search_resolver(None, "incr", "root/t", std::ptr::addr_of_mut!(out)),
            STATUS_OK
        );
        // With no resolver the response is a decodable empty method-hits buffer.
        assert!(decode_hits_out(&out).hits.is_empty());
        free_out(&mut out);
    }

    #[test]
    fn run_search_resolver_writes_the_resolved_hits_and_frees_clean() {
        use ls_pc_abi::payloads::{MethodHit, Rng};

        let seen = Arc::new(Mutex::new(None));
        let captured = seen.clone();
        let resolver: Box<SearchMethodsResolver> = Box::new(move |query: &str, target: &str| {
            *captured.lock().unwrap() = Some((query.to_string(), target.to_string()));
            MethodHitsResult {
                hits: vec![MethodHit {
                    uri: "file:///w/Enrichments.scala".to_string(),
                    symbol: "a/b/A$package.incr().".to_string(),
                    kind: 3,
                    range: Rng {
                        start_line: 1,
                        start_character: 6,
                        end_line: 1,
                        end_character: 10,
                    },
                }],
            }
        });

        let before = ls_pc_abi::memory::live_allocations();
        let mut out = LsBuf {
            ptr: std::ptr::null_mut(),
            len: 0,
        };
        assert_eq!(
            run_search_resolver(
                Some(resolver.as_ref()),
                "incr",
                "root/t",
                std::ptr::addr_of_mut!(out),
            ),
            STATUS_OK
        );
        // The resolver saw exactly the decoded arguments.
        assert_eq!(
            *seen.lock().unwrap(),
            Some(("incr".to_string(), "root/t".to_string()))
        );
        let decoded = decode_hits_out(&out);
        assert_eq!(decoded.hits.len(), 1);
        assert_eq!(decoded.hits[0].symbol, "a/b/A$package.incr().");
        assert_eq!(decoded.hits[0].kind, 3);
        assert_eq!(decoded.hits[0].range.start_character, 6);
        free_out(&mut out);
        assert_eq!(
            ls_pc_abi::memory::live_allocations(),
            before,
            "the response buffer is freed"
        );
    }

    #[test]
    fn run_search_resolver_contains_a_panicking_resolver() {
        let resolver: Box<SearchMethodsResolver> =
            Box::new(|_: &str, _: &str| panic!("boom in the search resolver"));
        let mut out = LsBuf {
            ptr: std::ptr::dangling_mut::<u8>(),
            len: 9,
        };
        assert_eq!(
            run_search_resolver(
                Some(resolver.as_ref()),
                "q",
                "t",
                std::ptr::addr_of_mut!(out),
            ),
            STATUS_PANIC
        );
    }

    #[test]
    fn run_resolver_contains_a_panicking_resolver() {
        let resolver: Box<SymbolDefinitionResolver> =
            Box::new(|_: &str, _: &str| panic!("boom in the resolver"));
        let mut out = LsBuf {
            ptr: std::ptr::dangling_mut::<u8>(),
            len: 9,
        };
        assert_eq!(
            run_resolver(
                Some(resolver.as_ref()),
                "sym",
                "uri",
                std::ptr::addr_of_mut!(out),
            ),
            STATUS_PANIC
        );
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
