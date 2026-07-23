//! Lifecycle logging over the fake-BSP harness: the boot narrative a stuck
//! user reads on stderr, asserted on a CAPTURED `log`-facade sink (the
//! testkit's `logcap` logger stands in for the production stderr sink, so the
//! assertions pin WHICH lines are emitted and in WHAT ORDER — the line
//! *format* is pinned by the `logging` module's unit tests).
//!
//! Two suites share this binary and serialize through `logcap::exclusive()`
//! (one process-global logger; parallel capture tests would interleave):
//!
//! - the handshake-progress heartbeat — a `build/initialize` the fake answers
//!   only after a configured delay emits the "still waiting for
//!   build/initialize" line while the caller blocks (the restart-while-mill-
//!   is-busy breadcrumb), and the request still succeeds afterwards;
//! - the full boot narrative IN ORDER over the real serve loop + real
//!   `IndexBootstrap` against the fake build server: initialize → bootstrap
//!   spawned → BSP handshake → model summary → store open → initial ingest →
//!   READY → adoption → shutdown ladder → exit.

use std::io::Cursor;
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use ls_bsp::uri::path_to_uri;
use ls_bsp::{BspClientHandlers, BspSession, BspSessionConfig};
use ls_server::{serve, CoreHandlers, CoreServices, IndexBootstrap, OutputSink, ServerCore};
use ls_testkit::fake_bsp::{serve_fake, FakeBsp, FakeBuildServer};
use ls_testkit::logcap;
use ls_testkit::wire::{notification, request, split_after_initialized};

/// While a bootstrap-handshake request has no response past the heartbeat
/// interval, the waiting line is emitted (every interval) and the request
/// still completes normally once the server answers.
#[test]
fn a_delayed_build_initialize_emits_the_still_waiting_heartbeat() {
    logcap::install();
    let _serialized = logcap::exclusive();

    let dir = tempfile::tempdir().unwrap();
    let server = Arc::new(FakeBuildServer::new(dir.path().join("NoSdb.scala")));
    // ~4 heartbeat intervals before build/initialize is answered.
    server.set_initialize_delay(Duration::from_millis(450));
    let (client, server_stream) = UnixStream::pair().unwrap();
    let _server_thread = serve_fake(Arc::clone(&server), server_stream);

    let session = BspSession::connect(
        dir.path().to_path_buf(),
        Box::new(client.try_clone().unwrap()),
        Box::new(client),
        BspClientHandlers::new(),
        BspSessionConfig {
            request_timeout: Duration::from_secs(10),
            shutdown_timeout: Duration::from_secs(2),
            handshake_heartbeat: Duration::from_millis(100),
            ..BspSessionConfig::default()
        },
    );
    let init = session
        .initialize()
        .expect("initialize succeeds after the delay");
    assert_eq!(init.display_name, "fake-bsp-server");

    let waiting: Vec<String> = logcap::lines()
        .into_iter()
        .filter(|line| line.contains("still waiting for build/initialize"))
        .collect();
    assert!(
        !waiting.is_empty(),
        "no heartbeat line was captured; lines:\n{}",
        logcap::lines().join("\n")
    );
    // The line carries the workspace-lock hint the docs table references.
    assert!(
        waiting[0].contains("blocked on another mill/sbt holding the workspace lock"),
        "{}",
        waiting[0]
    );
    // The handshake completion line lands after the waiting line(s).
    logcap::assert_in_order(&["still waiting for build/initialize", "build/initialize ok"]);
    session.shutdown();
}

/// The whole boot narrative, in order, over the real serve loop and the real
/// `IndexBootstrap` against the fake build server — the exact breadcrumb
/// sequence `docs/deployment.md` documents for a healthy start, followed by
/// the shutdown ladder and the clean exit.
#[test]
fn the_boot_narrative_lines_occur_in_order_on_a_captured_sink() {
    logcap::install();
    let _serialized = logcap::exclusive();

    let sink_flag = Arc::new(AtomicBool::new(false));
    let (fake, source) = {
        let sink = Arc::new(OutputSink::new(Vec::new()));
        FakeBsp::start(Arc::clone(&sink_flag), sink)
    };
    let root = fake.workspace_root.clone();

    let mut core: ServerCore<CoreServices> = ServerCore::new();
    let input = [
        request(1, "initialize", json!({ "rootUri": path_to_uri(&root) })),
        notification("initialized", json!({})),
        // A pre-ready request typically races bootstrap here; the once-per-
        // method info line is covered by unit behavior, not asserted in order.
        request(2, "shutdown", json!({})),
        notification("exit", json!({})),
    ]
    .concat();
    // Split after `initialized` so the first pass block-drains the bootstrap
    // worker at loop end (pump-until-ready), then the second pass shuts down.
    let split = split_after_initialized(&input);
    for chunk in [input[..split].to_vec(), input[split..].to_vec()] {
        let mut reader = Cursor::new(chunk);
        serve(
            &mut reader,
            source.sink.as_ref(),
            &mut core,
            &CoreHandlers,
            IndexBootstrap::new(source.clone()),
        )
        .unwrap();
    }

    logcap::assert_in_order(&[
        "initialize received",
        "initialized received — bootstrap spawned",
        "bootstrap started for workspace",
        "build/initialize ok: server 'fake-bsp-server'",
        "build model loaded:",
        "store opened at",
        "initial ingest complete — ingest: segment",
        "READY in",
        "bootstrap result adopted: workspace READY",
        "shutdown received",
        "session shutdown: sending build/shutdown",
        "exit received — leaving the serve loop (clean exit)",
    ]);

    // The model summary counts the fixture corpus: 4 Scala 3 targets, 3
    // indexable, 1 without SemanticDB (fixture-nosdb) — the list a user needs.
    let summary = logcap::lines()
        .into_iter()
        .find(|line| line.contains("build model loaded:"))
        .expect("model summary line");
    assert!(
        summary.contains("4 Scala 3 target(s), 3 indexable, 1 without SemanticDB"),
        "{summary}"
    );
    assert!(summary.contains("fixture-nosdb"), "{summary}");
}
