//! The capturing test logger: a `log` facade sink that records every emitted
//! record as `"target LEVEL message"` into a process-global buffer, so a test
//! can assert WHICH lifecycle lines were logged and in WHAT ORDER without
//! scraping stderr.
//!
//! The `log` crate allows exactly one global logger per process, so [`install`]
//! is idempotent and the buffer is shared by every test in the binary. Tests
//! that assert on captured lines serialize themselves through [`exclusive`]
//! (clearing the buffer under the guard) so parallel tests in the same binary
//! cannot interleave lines into each other's assertions.

use std::sync::{Mutex, MutexGuard, OnceLock};

use log::{LevelFilter, Metadata, Record};

fn buffer() -> &'static Mutex<Vec<String>> {
    static BUFFER: OnceLock<Mutex<Vec<String>>> = OnceLock::new();
    BUFFER.get_or_init(|| Mutex::new(Vec::new()))
}

struct CaptureLogger;

impl log::Log for CaptureLogger {
    fn enabled(&self, _metadata: &Metadata) -> bool {
        true
    }

    fn log(&self, record: &Record) {
        buffer()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(format!(
                "{} {} {}",
                record.target(),
                record.level(),
                record.args()
            ));
    }

    fn flush(&self) {}
}

/// Installs the capturing logger (idempotent; the whole test binary shares
/// it) at `Debug` max level, so every facade line is captured.
pub fn install() {
    static LOGGER: CaptureLogger = CaptureLogger;
    let _ = log::set_logger(&LOGGER);
    log::set_max_level(LevelFilter::Debug);
}

/// Serializes capture-asserting tests within one binary and starts them from
/// an empty buffer. Hold the guard for the whole test.
pub fn exclusive() -> MutexGuard<'static, ()> {
    static GATE: OnceLock<Mutex<()>> = OnceLock::new();
    let guard = GATE
        .get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    clear();
    guard
}

/// Drops everything captured so far.
pub fn clear() {
    buffer()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clear();
}

/// A snapshot of the captured lines, each `"target LEVEL message"`.
pub fn lines() -> Vec<String> {
    buffer()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
}

/// Asserts that `needles` occur among the captured lines as an ORDERED
/// subsequence (each needle a substring of some line strictly after the
/// previous match). Panics with the full capture on failure.
pub fn assert_in_order(needles: &[&str]) {
    let captured = lines();
    let mut from = 0usize;
    for needle in needles {
        match captured[from..]
            .iter()
            .position(|line| line.contains(needle))
        {
            Some(offset) => from += offset + 1,
            None => panic!(
                "log line containing {needle:?} not found (in order) after index {from};\n\
                 captured lines:\n{}",
                captured.join("\n")
            ),
        }
    }
}
