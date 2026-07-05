//! Scenario driver for the M0 spike.
//!
//! Each JVM-boot scenario runs in its own process (`JNI_CreateJavaVM` is a
//! process-global singleton that cannot be re-created), so the integration
//! tests spawn this binary once per scenario and assert on its `SPIKE_OK:` /
//! `SPIKE_FAIL:` line and exit code.

use ls_jvm_spike::BootError;
use std::path::PathBuf;
use std::time::Duration;

fn main() {
    let scenario = std::env::args().nth(1).unwrap_or_default();
    let result = match scenario.as_str() {
        "echo" => run_echo(),
        "java-throw" => run_java_throw(),
        "rust-panic" => run_rust_panic(),
        "timeout" => run_timeout(),
        other => Err(format!("unknown scenario: {other:?}")),
    };
    match result {
        Ok(detail) => {
            println!("SPIKE_OK: {detail}");
            std::process::exit(0);
        }
        Err(detail) => {
            println!("SPIKE_FAIL: {detail}");
            std::process::exit(1);
        }
    }
}

fn libjvm() -> PathBuf {
    PathBuf::from(std::env::var("LS_LIBJVM").expect("LS_LIBJVM must point at libjvm.so"))
}

fn agent_jar() -> PathBuf {
    PathBuf::from(
        std::env::var("SPIKE_AGENT_JAR")
            .expect("SPIKE_AGENT_JAR must point at the ls-pc-host-spike agent jar"),
    )
}

/// Boot the JVM and assert the cold-start property: no libjvm before boot, a
/// libjvm mapping after.
fn boot(scenario: &str, rendezvous: Duration) -> Result<(), BootError> {
    assert!(
        !ls_jvm_spike::libjvm_mapped(),
        "libjvm must not be mapped before boot"
    );
    ls_jvm_spike::boot(&libjvm(), &agent_jar(), scenario, rendezvous)?;
    assert!(
        ls_jvm_spike::libjvm_mapped(),
        "libjvm must be mapped after boot"
    );
    Ok(())
}

/// Happy path: an echo payload round-trips on the loaned dispatch thread.
fn run_echo() -> Result<String, String> {
    boot("normal", Duration::from_secs(15)).map_err(|e| e.to_string())?;
    let payload = b"hello m0 boundary";
    let echoed = ls_jvm_spike::echo(payload).map_err(|e| format!("echo failed: {e}"))?;
    if echoed == payload {
        Ok(format!(
            "echoed {} bytes on the loaned thread",
            echoed.len()
        ))
    } else {
        Err(format!("echo mismatch: sent {payload:?}, got {echoed:?}"))
    }
}

/// A Java `Throwable` in the upcall is contained to a status error, and the VM
/// stays alive to serve a subsequent echo.
fn run_java_throw() -> Result<String, String> {
    boot("normal", Duration::from_secs(15)).map_err(|e| e.to_string())?;
    match ls_jvm_spike::echo(b"__throw__") {
        Ok(v) => return Err(format!("expected contained throw, echo returned {v:?}")),
        Err(e) => eprintln!("[driver] contained Java throw: {e}"),
    }
    let payload = b"after-throw";
    let echoed = ls_jvm_spike::echo(payload).map_err(|e| format!("post-throw echo failed: {e}"))?;
    if echoed == payload {
        Ok("Java throw contained; VM alive and echoing".to_string())
    } else {
        Err(format!("post-throw echo mismatch: {echoed:?}"))
    }
}

/// A Rust panic in a callback is contained via `catch_unwind`; the process and
/// dispatch lane stay alive to serve a subsequent echo.
fn run_rust_panic() -> Result<String, String> {
    boot("normal", Duration::from_secs(15)).map_err(|e| e.to_string())?;
    match ls_jvm_spike::echo(b"__rustpanic__") {
        Ok(v) => return Err(format!("expected contained panic, echo returned {v:?}")),
        Err(e) => eprintln!("[driver] contained Rust panic: {e}"),
    }
    let payload = b"after-panic";
    let echoed = ls_jvm_spike::echo(payload).map_err(|e| format!("post-panic echo failed: {e}"))?;
    if echoed == payload {
        Ok("Rust panic contained; process and dispatch lane alive".to_string())
    } else {
        Err(format!("post-panic echo mismatch: {echoed:?}"))
    }
}

/// A premain that never registers makes the Rust rendezvous time out with the
/// captured island log.
fn run_timeout() -> Result<String, String> {
    match boot("timeout", Duration::from_secs(3)) {
        Ok(()) => Err("expected rendezvous timeout, but boot succeeded".to_string()),
        Err(BootError::RendezvousTimeout { island_log }) => {
            if island_log.is_empty() {
                Err("timed out but captured no island log".to_string())
            } else {
                Ok(format!("rendezvous timed out; island log: {island_log:?}"))
            }
        }
        Err(other) => Err(format!("expected rendezvous timeout, got {other}")),
    }
}
