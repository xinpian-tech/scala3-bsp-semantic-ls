//! Boundary scenario tests.
//!
//! `JNI_CreateJavaVM` is a process-global singleton, so each scenario runs in
//! its own `ls-jvm-spike` process. These tests spawn that binary once per
//! scenario and assert on its `SPIKE_OK:` line. They require the island agent
//! jar (`SPIKE_AGENT_JAR`, built by `mill pcHostSpike.assembly`) and the
//! embedded `libjvm` (`LS_LIBJVM`); when either is unset (e.g. a bare `cargo
//! test` outside the nix dev shell) the test skips rather than fails.

use std::process::Command;

fn run(scenario: &str) -> Option<(String, bool)> {
    let jar = std::env::var("SPIKE_AGENT_JAR").ok()?;
    let libjvm = std::env::var("LS_LIBJVM").ok()?;
    let bin = env!("CARGO_BIN_EXE_ls-jvm-spike");
    let out = Command::new(bin)
        .arg(scenario)
        .env("SPIKE_AGENT_JAR", jar)
        .env("LS_LIBJVM", libjvm)
        .output()
        .expect("spawn ls-jvm-spike");
    Some((
        String::from_utf8_lossy(&out.stdout).into_owned(),
        out.status.success(),
    ))
}

fn check(scenario: &str) {
    match run(scenario) {
        None => eprintln!("skip {scenario}: SPIKE_AGENT_JAR / LS_LIBJVM unset"),
        Some((out, ok)) => assert!(
            ok && out.contains("SPIKE_OK"),
            "scenario {scenario} did not pass: {out}"
        ),
    }
}

/// Happy path: an echo payload round-trips on the loaned dispatch thread.
#[test]
fn echo_round_trips_on_loaned_thread() {
    check("echo");
}

/// A Java `Throwable` in the upcall is contained to a status error; VM stays up.
#[test]
fn java_throwable_is_contained() {
    check("java-throw");
}

/// A Rust panic in a callback is contained via `catch_unwind`; process stays up.
#[test]
fn rust_panic_is_contained() {
    check("rust-panic");
}

/// A premain that never registers makes the rendezvous time out with a log.
#[test]
fn rendezvous_timeout_reports_island_log() {
    check("timeout");
}
