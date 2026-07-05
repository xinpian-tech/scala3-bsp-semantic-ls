//! M0 embedded-JVM boundary-viability spike.
//!
//! Boots an in-process JVM through the single JNI Invocation-API boot symbol,
//! `JNI_CreateJavaVM`, resolved by `dlopen`/`dlsym` — no `jni` crate, no
//! `jni.h`, no bindgen, and no JNIEnv usage. The three argument structs are the
//! only JNI surface and are hand-declared `#[repr(C)]` below.
//!
//! What this proves (the M0 unknowns):
//! * a `-javaagent` premain fires under `JNI_CreateJavaVM` with no main class;
//! * the premain can build Java FFM downcalls from the raw Rust vtable address
//!   and an upcall stub for a PC (echo) vtable, register it, and spawn platform
//!   threads that are *loaned* to Rust (they enter `pc_dispatch_loop` and never
//!   return);
//! * an echo payload round-trips through the registered vtable on a loaned
//!   dispatch thread, with Rust-owned response memory;
//! * a Java `Throwable` in an upcall and a Rust panic in a callback are both
//!   contained (status error, VM + process stay alive);
//! * a premain that never registers makes the Rust rendezvous time out with the
//!   captured island log.

#![forbid(unsafe_op_in_unsafe_fn)]

use std::ffi::{c_char, c_void, CString};
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// JNI Invocation API (the only JNI surface — hand-declared, no jni.h/bindgen).
// ---------------------------------------------------------------------------

/// `JavaVMOption` — one VM option string.
#[repr(C)]
struct JavaVMOption {
    option_string: *const c_char,
    extra_info: *mut c_void,
}

/// `JavaVMInitArgs` — the argument block for `JNI_CreateJavaVM`.
#[repr(C)]
struct JavaVMInitArgs {
    version: i32,
    n_options: i32,
    options: *mut JavaVMOption,
    ignore_unrecognized: u8,
}

/// The one boot symbol we resolve: `JNI_CreateJavaVM(JavaVM**, void**, void*)`.
type JniCreateJavaVm =
    unsafe extern "C" fn(p_vm: *mut *mut c_void, p_env: *mut *mut c_void, args: *mut c_void) -> i32;

/// JNI version requested in `JavaVMInitArgs` (JDK 21+ invocation semantics).
const JNI_VERSION_21: i32 = 0x0015_0000;

// ---------------------------------------------------------------------------
// The C-ABI boundary vtables (flat `#[repr(C)]`, mirrored by the Java agent).
// ---------------------------------------------------------------------------

/// Boundary contract version, checked at registration.
pub const ABI_VERSION: u64 = 1;

const STATUS_OK: i32 = 0;
const STATUS_PANIC: i32 = -1;
const STATUS_BAD_ARG: i32 = -2;
const STATUS_ABI_MISMATCH: i32 = -3;

/// Island → Rust structured logging.
pub type LogFn = extern "C" fn(level: i32, ptr: *const u8, len: i32);
/// Island → Rust registration of the PC vtable; returns a status code.
pub type RegisterPcVtableFn = extern "C" fn(pc: *const PcVtable) -> i32;
/// Entry point a Java-loaned thread enters and never returns from.
pub type PcDispatchLoopFn = extern "C" fn(worker_index: i32);
/// The PC-side echo op, exposed by the island as an FFM upcall stub:
/// `echo(in_ptr, in_len, out_ptr, out_cap) -> written_len | negative status`.
pub type EchoFn =
    unsafe extern "C" fn(in_ptr: *const u8, in_len: i32, out_ptr: *mut u8, out_cap: i32) -> i32;

/// The Rust vtable handed to the premain (its address is the agent argument).
/// The island mirrors this layout through jextract-generated FFM bindings.
#[repr(C)]
pub struct RustVtable {
    pub abi_version: u64,
    pub log: LogFn,
    pub register_pc_vtable: RegisterPcVtableFn,
    pub pc_dispatch_loop: PcDispatchLoopFn,
}

/// The PC vtable the island registers, built as FFM upcall stubs.
#[repr(C)]
pub struct PcVtable {
    pub abi_version: u64,
    pub echo: EchoFn,
}

// The island reads slots at fixed offsets, so the layout must not drift.
const _: () = {
    assert!(std::mem::size_of::<RustVtable>() == 32);
    assert!(std::mem::size_of::<PcVtable>() == 16);
    assert!(std::mem::size_of::<*const c_void>() == 8);
};

static RUST_VTABLE: RustVtable = RustVtable {
    abi_version: ABI_VERSION,
    log: vt_log,
    register_pc_vtable: vt_register_pc_vtable,
    pc_dispatch_loop: vt_pc_dispatch_loop,
};

// ---------------------------------------------------------------------------
// Boundary registry (global — the vtable fns are plain `extern "C"` items).
// ---------------------------------------------------------------------------

struct EchoJob {
    input: Vec<u8>,
    reply: Sender<Result<Vec<u8>, String>>,
}

struct State {
    echo: Option<EchoFn>,
    dispatch_ready: bool,
    log: Vec<String>,
}

struct Registry {
    state: Mutex<State>,
    cv: Condvar,
    job_tx: Sender<EchoJob>,
    job_rx: Mutex<Option<Receiver<EchoJob>>>,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY
        .get()
        .expect("registry not initialized before boot")
}

// ---------------------------------------------------------------------------
// Rust vtable implementations (every export is `catch_unwind`-wrapped so a Rust
// panic never unwinds across the FFI boundary).
// ---------------------------------------------------------------------------

extern "C" fn vt_log(level: i32, ptr: *const u8, len: i32) {
    let _ = std::panic::catch_unwind(|| {
        let msg = read_utf8(ptr, len);
        if let Some(reg) = REGISTRY.get() {
            if let Ok(mut st) = reg.state.lock() {
                st.log.push(format!("[{level}] {msg}"));
            }
        }
    });
}

extern "C" fn vt_register_pc_vtable(pc: *const PcVtable) -> i32 {
    match std::panic::catch_unwind(|| register_pc_vtable_inner(pc)) {
        Ok(rc) => rc,
        Err(_) => STATUS_PANIC,
    }
}

extern "C" fn vt_pc_dispatch_loop(worker_index: i32) {
    let _ = std::panic::catch_unwind(|| dispatch_loop(worker_index));
}

fn register_pc_vtable_inner(pc: *const PcVtable) -> i32 {
    if pc.is_null() {
        return STATUS_BAD_ARG;
    }
    // SAFETY: the island passes a valid `PcVtable` matching this layout for the
    // duration of the call.
    let abi = unsafe { std::ptr::addr_of!((*pc).abi_version).read_unaligned() };
    if abi != ABI_VERSION {
        return STATUS_ABI_MISMATCH;
    }
    // SAFETY: same; the echo slot is a live FFM upcall stub matching `EchoFn`.
    let echo = unsafe { std::ptr::addr_of!((*pc).echo).read_unaligned() };
    let reg = registry();
    let mut st = reg.state.lock().expect("state lock");
    st.echo = Some(echo);
    reg.cv.notify_all();
    STATUS_OK
}

fn dispatch_loop(worker_index: i32) {
    let reg = registry();
    if worker_index != 0 {
        // Control thread: the spike exercises only the dispatch lane, so the
        // control thread simply parks (still proving a second loaned thread).
        loop {
            std::thread::park();
        }
    }

    let rx = reg
        .job_rx
        .lock()
        .expect("job_rx lock")
        .take()
        .expect("dispatch receiver already taken");
    {
        let mut st = reg.state.lock().expect("state lock");
        st.dispatch_ready = true;
        reg.cv.notify_all();
    }

    while let Ok(job) = rx.recv() {
        // Per-request containment: a panic in the request (or in the upcall it
        // makes) becomes a status error and the dispatch lane keeps serving.
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_echo(&job.input)));
        let reply =
            outcome.unwrap_or_else(|_| Err("rust callback panicked (contained)".to_string()));
        let _ = job.reply.send(reply);
    }
}

fn run_echo(input: &[u8]) -> Result<Vec<u8>, String> {
    // Fault injection: drive the Rust-panic-containment scenario.
    if input == b"__rustpanic__" {
        panic!("injected Rust panic in the echo path");
    }

    let echo = registry().state.lock().expect("state lock").echo;
    let echo = echo.ok_or("echo not registered")?;

    // Response memory is Rust-owned (caller measures, allocates, and frees).
    let mut out = vec![0u8; input.len()];
    // SAFETY: `echo` is a live upcall stub; `input`/`out` are valid buffers
    // owned by this thread for the duration of the call.
    let written = unsafe {
        echo(
            input.as_ptr(),
            input.len() as i32,
            out.as_mut_ptr(),
            out.len() as i32,
        )
    };
    if written < 0 {
        return Err(format!("echo upcall returned status {written}"));
    }
    let written = written as usize;
    if written > out.len() {
        return Err(format!("echo wrote {written} > capacity {}", out.len()));
    }
    out.truncate(written);
    Ok(out)
}

fn read_utf8(ptr: *const u8, len: i32) -> String {
    if ptr.is_null() || len <= 0 {
        return String::new();
    }
    // SAFETY: the Java FFM caller passes a valid `ptr`/`len` for the call.
    let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    String::from_utf8_lossy(bytes).into_owned()
}

// ---------------------------------------------------------------------------
// Boot + echo API.
// ---------------------------------------------------------------------------

/// A boundary boot failure.
#[derive(Debug)]
pub enum BootError {
    /// `dlopen`/`dlsym`/`JNI_CreateJavaVM` failed.
    Boot(String),
    /// The premain never completed registration before the deadline.
    RendezvousTimeout { island_log: Vec<String> },
}

impl std::fmt::Display for BootError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BootError::Boot(m) => write!(f, "boot failed: {m}"),
            BootError::RendezvousTimeout { island_log } => {
                write!(
                    f,
                    "rendezvous timed out; island log: [{}]",
                    island_log.join(" | ")
                )
            }
        }
    }
}

impl std::error::Error for BootError {}

/// Does this process currently have `libjvm` mapped? Read from
/// `/proc/self/maps`, so it reflects the real cold-start property.
pub fn libjvm_mapped() -> bool {
    std::fs::read_to_string("/proc/self/maps")
        .map(|maps| maps.contains("libjvm"))
        .unwrap_or(false)
}

/// Boot the embedded JVM, hand the premain this process's Rust vtable address,
/// and block until the premain registers the PC vtable and the loaned dispatch
/// thread is ready — or until `rendezvous_timeout` elapses.
pub fn boot(
    libjvm: &Path,
    agent_jar: &Path,
    scenario: &str,
    rendezvous_timeout: Duration,
) -> Result<(), BootError> {
    let (job_tx, job_rx) = channel();
    let _ = REGISTRY.set(Registry {
        state: Mutex::new(State {
            echo: None,
            dispatch_ready: false,
            log: Vec::new(),
        }),
        cv: Condvar::new(),
        job_tx,
        job_rx: Mutex::new(Some(job_rx)),
    });

    let vtable_addr = std::ptr::addr_of!(RUST_VTABLE) as usize;
    let agent_arg = format!("0x{vtable_addr:x}");
    boot_with_agent(libjvm, agent_jar, &agent_arg, scenario).map_err(BootError::Boot)?;

    let reg = registry();
    let deadline = Instant::now() + rendezvous_timeout;
    let mut st = reg.state.lock().expect("state lock");
    loop {
        if st.echo.is_some() && st.dispatch_ready {
            return Ok(());
        }
        let now = Instant::now();
        if now >= deadline {
            return Err(BootError::RendezvousTimeout {
                island_log: st.log.clone(),
            });
        }
        let (guard, res) = reg.cv.wait_timeout(st, deadline - now).expect("cv wait");
        st = guard;
        if res.timed_out() && !(st.echo.is_some() && st.dispatch_ready) {
            return Err(BootError::RendezvousTimeout {
                island_log: st.log.clone(),
            });
        }
    }
}

/// Round-trip `payload` through the registered echo op on the loaned dispatch
/// thread. `Err` on a contained fault (Java `Throwable` or Rust panic) or on a
/// dispatch/reply failure.
pub fn echo(payload: &[u8]) -> Result<Vec<u8>, String> {
    let (reply_tx, reply_rx) = channel();
    registry()
        .job_tx
        .send(EchoJob {
            input: payload.to_vec(),
            reply: reply_tx,
        })
        .map_err(|_| "dispatch channel closed".to_string())?;
    reply_rx
        .recv_timeout(Duration::from_secs(10))
        .map_err(|e| format!("echo reply: {e}"))?
}

/// The island log captured via the `log` downcall.
pub fn island_log() -> Vec<String> {
    REGISTRY
        .get()
        .and_then(|r| r.state.lock().ok().map(|s| s.log.clone()))
        .unwrap_or_default()
}

/// Boot the JVM with exactly the plan's boot options: the agent jar on the
/// class path, `--enable-native-access=ALL-UNNAMED`,
/// `-XX:+UseCompactObjectHeaders`, the scenario system property, and
/// `-javaagent:<jar>=<agent_arg>`.
fn boot_with_agent(
    libjvm: &Path,
    agent_jar: &Path,
    agent_arg: &str,
    scenario: &str,
) -> Result<(), String> {
    let class_path = CString::new(format!("-Djava.class.path={}", agent_jar.display()))
        .map_err(|e| format!("class path: {e}"))?;
    let native_access =
        CString::new("--enable-native-access=ALL-UNNAMED").expect("static option is NUL-free");
    let compact_headers =
        CString::new("-XX:+UseCompactObjectHeaders").expect("static option is NUL-free");
    let scenario_prop = CString::new(format!("-Dspike.scenario={scenario}"))
        .map_err(|e| format!("scenario: {e}"))?;
    let javaagent = CString::new(format!("-javaagent:{}={}", agent_jar.display(), agent_arg))
        .map_err(|e| format!("javaagent: {e}"))?;

    // SAFETY: `dlopen`/`dlsym` of a real libjvm; the option strings and the
    // options array outlive the `JNI_CreateJavaVM` call; the arg pointers are
    // valid `#[repr(C)]` structs matching the JNI ABI.
    unsafe {
        let lib =
            libloading::Library::new(libjvm).map_err(|e| format!("dlopen {libjvm:?}: {e}"))?;
        let create: libloading::Symbol<JniCreateJavaVm> = lib
            .get(b"JNI_CreateJavaVM\0")
            .map_err(|e| format!("dlsym JNI_CreateJavaVM: {e}"))?;

        let mut options = [
            JavaVMOption {
                option_string: class_path.as_ptr(),
                extra_info: std::ptr::null_mut(),
            },
            JavaVMOption {
                option_string: native_access.as_ptr(),
                extra_info: std::ptr::null_mut(),
            },
            JavaVMOption {
                option_string: compact_headers.as_ptr(),
                extra_info: std::ptr::null_mut(),
            },
            JavaVMOption {
                option_string: scenario_prop.as_ptr(),
                extra_info: std::ptr::null_mut(),
            },
            JavaVMOption {
                option_string: javaagent.as_ptr(),
                extra_info: std::ptr::null_mut(),
            },
        ];

        let mut args = JavaVMInitArgs {
            version: JNI_VERSION_21,
            n_options: options.len() as i32,
            options: options.as_mut_ptr(),
            ignore_unrecognized: 0,
        };

        let mut jvm: *mut c_void = std::ptr::null_mut();
        let mut env: *mut c_void = std::ptr::null_mut();
        let rc = create(
            &mut jvm,
            &mut env,
            (&mut args as *mut JavaVMInitArgs).cast::<c_void>(),
        );
        if rc != 0 {
            return Err(format!("JNI_CreateJavaVM failed rc={rc}"));
        }

        // The JVM lives for the process lifetime; keep libjvm loaded.
        std::mem::forget(lib);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_utf8_handles_null_and_bytes() {
        assert_eq!(read_utf8(std::ptr::null(), 5), "");
        let b = b"hello";
        assert_eq!(read_utf8(b.as_ptr(), b.len() as i32), "hello");
        assert_eq!(read_utf8(b.as_ptr(), 0), "");
    }

    #[test]
    fn vtable_abi_is_registered_version() {
        assert_eq!(RUST_VTABLE.abi_version, ABI_VERSION);
    }
}
