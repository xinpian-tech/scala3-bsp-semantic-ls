//! Shared harness for the real-BSP end-to-end suites: gating, sample-workspace
//! preparation, and the interactive client. The wire plumbing is the testkit's
//! [`ls_testkit::client::WireClient`] (re-exported as [`RealServer`]), booted
//! here over the whole production server (`serve` loop + `IndexBootstrap` over
//! the production `LiveBspModelSource`) against a REAL mill build server built
//! from `it/sample-workspace`. Split into its own module so each
//! live-presentation-compiler scenario can live in its OWN integration-test
//! binary — only one embedded JVM/island can boot per process, so the index/BSP
//! rows, the position-feature PC rows, and the faulted dispatch-generation
//! recovery each run in a separate process.
//!
//! Each suite binary uses a different subset of the shared items and testkit
//! re-exports, hence the module-wide allows.
#![allow(dead_code)]
#![allow(unused_imports)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use ls_bsp::protocol::PublishDiagnosticsParams as BspPublishDiagnosticsParams;
use ls_server::{DiagnosticRouter, IndexBootstrap, LiveBspModelSource};

pub use ls_testkit::client::WireClient as RealServer;
pub use ls_testkit::client::{COMPILE, DOCTOR, REINDEX};
pub use ls_testkit::positions::{count_token, position_json, position_of, source_text, span_of};

// --- gating -------------------------------------------------------------------

/// The whole suite is gated on a real mill toolchain.
pub fn mill_enabled() -> bool {
    std::env::var_os("LS_REAL_BSP_IT").is_some()
}

/// The presentation-compiler scenarios additionally need a real embedded JVM.
pub fn pc_enabled() -> bool {
    std::env::var_os("LS_LIBJVM").is_some()
        && std::env::var_os("PC_HOST_AGENT_JAR").is_some()
        && std::env::var_os("LS_PC_TARGET_CLASSPATH").is_some()
}

// --- workspace preparation ----------------------------------------------------

fn repo_root() -> PathBuf {
    if let Ok(root) = std::env::var("LS_REPO_ROOT") {
        return PathBuf::from(root);
    }
    // crates/ls-server -> repo root
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}

fn copy_dir(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if from.is_dir() {
            copy_dir(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

/// Copy `it/sample-workspace` into a fresh temp dir, apply any extra sources /
/// build-file replacement, run `mill BSP/install` to write the real
/// `.bsp/mill-bsp.json`, and return the isolated workspace root.
pub fn prepare_workspace(
    extra_sources: &[(&str, &str)],
    build_mill: Option<&str>,
) -> (tempfile::TempDir, PathBuf) {
    let sample = repo_root().join("it").join("sample-workspace");
    assert!(sample.is_dir(), "sample workspace not found at {sample:?}");

    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("sample-workspace");
    copy_dir(&sample, &ws);

    for (rel, text) in extra_sources {
        let path = ws.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
    }
    if let Some(mill) = build_mill {
        std::fs::write(ws.join("build.mill"), mill).unwrap();
    }

    let install = Command::new("mill")
        .args(["--no-daemon", "mill.bsp.BSP/install"])
        .current_dir(&ws)
        .status()
        .expect("run mill BSP/install");
    assert!(install.success(), "mill BSP/install failed");

    (tmp, ws)
}

// --- the interactive server harness -------------------------------------------

/// The suites' `RealServer::boot(tmp, ws)` entry point over the re-exported
/// [`RealServer`] ([`ls_testkit::client::WireClient`]).
pub trait BootRealServer {
    /// Boot the whole production server over the testkit's in-process wire:
    /// the `serve` loop + `IndexBootstrap` over the production
    /// `LiveBspModelSource`, with the session's `build/publishDiagnostics`
    /// routed through a `DiagnosticRouter` and written straight to the shared
    /// output sink (the same sink the loop writes responses to), exactly as
    /// production wires it. `tmp` (the isolated workspace's temp dir) is held
    /// by the client for the session's lifetime.
    fn boot(tmp: tempfile::TempDir, ws: PathBuf) -> RealServer;
}

impl BootRealServer for RealServer {
    fn boot(tmp: tempfile::TempDir, ws: PathBuf) -> RealServer {
        RealServer::boot_in_process_with(move |parts| {
            let router = Arc::new(Mutex::new(DiagnosticRouter::new()));
            let sink = Arc::clone(&parts.sink);
            let on_diagnostics: Arc<dyn Fn(BspPublishDiagnosticsParams) + Send + Sync> =
                Arc::new(move |params| {
                    if let Some(publish) = router.lock().unwrap().accept(&params) {
                        let _ = sink.publish_diagnostics(&publish);
                    }
                });
            let on_build_targets_changed: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
            let source = LiveBspModelSource::new(on_build_targets_changed, on_diagnostics);
            (ws, tmp, IndexBootstrap::new(source))
        })
    }
}

// The workspace-relative sources the sample build indexes (a + b carry
// -Xsemanticdb; c does not).
pub const GREETING: &str = "a/src/pkga/Greeting.scala";
pub const INSIDE: &str = "a/src/pkga/Inside.scala";
pub const CONSUMER: &str = "b/src/pkgb/Consumer.scala";
pub const OTHER: &str = "b/src/pkgb/Other.scala";
pub const WIDGET: &str = "c/src/pkgc/Widget.scala";
pub const INDEXED: [&str; 4] = [GREETING, INSIDE, CONSUMER, OTHER];

pub fn skip(reason: &str) {
    eprintln!("{reason}");
}
