//! Real-BSP end-to-end: the whole server driven over the framed LSP wire against
//! a REAL mill build server built from the deterministic `it/sample-workspace`.
//! Unlike the fake-BSP suite (which speaks JSON-RPC to an in-process fake), this
//! drives production discovery + launch: the workspace gets a real
//! `.bsp/mill-bsp.json` from `mill mill.bsp.BSP/install`, and bootstrap loads the
//! project model over the production `LiveBspModelSource`, which discovers the
//! connection file and launches mill, then retains the real session-backed
//! compiler. So `initialize`/`initialized`, model load, compile, diagnostics
//! forwarding, rename-through-compile, and session teardown all run through the
//! same production code an editor exercises. A port of the Scala `RealBsp*`
//! suites.
//!
//! Gating (mirrors `scripts/it-real-bsp.sh` and `crates/ls-bsp/tests/mill_smoke.rs`):
//!   * `LS_REAL_BSP_IT=1` — the whole suite needs a real mill/JVM toolchain the
//!     hermetic Nix check and ordinary `cargo test` forbid, so every scenario
//!     skips cleanly without it.
//!   * `LS_LIBJVM` + `PC_HOST_AGENT_JAR` + `LS_PC_TARGET_CLASSPATH` — the
//!     presentation-compiler scenarios (hover/signatureHelp/definition, dirty
//!     completion, forked-worker liveness) additionally need a real embedded JVM
//!     and skip when it is absent, exactly like `live_pc.rs`. The index/BSP
//!     scenarios run under `LS_REAL_BSP_IT` alone.
//!
//! Coverage of the Scala real-BSP scenario matrix:
//!   * doctor names mill-bsp + flags the no-SemanticDB module; the compile fills
//!     the index; workspace/symbol; the cross-module reference set; rename edits
//!     every module the compile touched.
//!   * SemanticDB is mandatory — completion/documentHighlight/rename on the
//!     no-SemanticDB module are hard errors (no quiet PC fallback).
//!   * a real compile error is forwarded as an Error diagnostic and the fix
//!     clears it; a save-driven compile+reingest reflects new token positions
//!     with no explicit reindex.
//!   * rename rejections: a no-SemanticDB source, an external/library symbol, a
//!     position with no occurrence.
//!   * presentation-compiler position features and index documentHighlight.
//!   * a source shared across two targets unifies references + passes the
//!     shared-source rename consistency check.
//!   * the forked worker answers a dirty completion and the doctor reports it
//!     alive (the OS-level worker-kill/respawn fault injection is proven at the
//!     island boundary by `ls-jvm`'s live boundary suite, which owns the worker
//!     pid the framed LSP wire does not expose).
//!
//! The repeated-save segment-hygiene + no-BSP warm-restart-from-recovery scenario
//! is not ported: the no-BSP warm restart is the trimmed mode carried as a
//! recorded deferral, so there is no recovered-index restart path to exercise
//! here. The ahead-of-time-trained boot scenario is out of scope for this
//! rewrite.

use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use ls_bsp::protocol::PublishDiagnosticsParams as BspPublishDiagnosticsParams;
use ls_bsp::uri::path_to_uri;
use ls_server::{
    read_frame, serve, CoreHandlers, DiagnosticRouter, IndexBootstrap, LiveBspModelSource,
    OutputSink, ServerCore,
};

// --- gating -------------------------------------------------------------------

/// The whole suite is gated on a real mill toolchain.
fn mill_enabled() -> bool {
    std::env::var_os("LS_REAL_BSP_IT").is_some()
}

/// The presentation-compiler scenarios additionally need a real embedded JVM.
fn pc_enabled() -> bool {
    std::env::var_os("LS_LIBJVM").is_some()
        && std::env::var_os("PC_HOST_AGENT_JAR").is_some()
        && std::env::var_os("LS_PC_TARGET_CLASSPATH").is_some()
}

const DOCTOR: &str = "scala3SemanticLs.doctor";

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
fn prepare_workspace(
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

/// A production server driven over two `UnixStream` pipes — the framed LSP wire.
/// The `serve` loop and the real `LiveBspModelSource` bootstrap run on a worker
/// thread; a reader thread demultiplexes server→client frames into a channel so
/// requests block on their matching response id while notifications (diagnostics)
/// accumulate for `await_publish`.
struct RealServer {
    to_server: UnixStream,
    inbound: Receiver<Value>,
    pending: Vec<Value>,
    next_id: i64,
    ws: PathBuf,
    serve_thread: Option<JoinHandle<()>>,
    reader_thread: Option<JoinHandle<()>>,
    _tmp: tempfile::TempDir,
}

impl RealServer {
    fn boot(tmp: tempfile::TempDir, ws: PathBuf) -> RealServer {
        // client -> server (the server reads this as its input)
        let (client_write, server_read) = UnixStream::pair().unwrap();
        // server -> client (the server writes framed output here)
        let (server_write, client_read) = UnixStream::pair().unwrap();

        let serve_thread = thread::spawn(move || {
            let mut core = ServerCore::new();
            let router = Arc::new(Mutex::new(DiagnosticRouter::new()));
            let sink = Arc::new(OutputSink::new(server_write));
            let on_diagnostics: Arc<dyn Fn(BspPublishDiagnosticsParams) + Send + Sync> = {
                let router = Arc::clone(&router);
                let sink = Arc::clone(&sink);
                Arc::new(move |params| {
                    if let Some(publish) = router.lock().unwrap().accept(&params) {
                        let _ = sink.publish_diagnostics(&publish);
                    }
                })
            };
            let on_build_targets_changed: Arc<dyn Fn() + Send + Sync> = Arc::new(|| {});
            let source = LiveBspModelSource::new(on_build_targets_changed, on_diagnostics);
            let bootstrap = IndexBootstrap::new(source);
            let mut reader = BufReader::new(server_read);
            let _ = serve(
                &mut reader,
                sink.as_ref(),
                &mut core,
                &CoreHandlers,
                bootstrap,
            );
        });

        let (tx, rx) = mpsc::channel();
        let reader_thread = thread::spawn(move || {
            let mut reader = BufReader::new(client_read);
            while let Ok(Some(bytes)) = read_frame(&mut reader) {
                match serde_json::from_slice::<Value>(&bytes) {
                    Ok(value) => {
                        if tx.send(value).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        RealServer {
            to_server: client_write,
            inbound: rx,
            pending: Vec::new(),
            next_id: 1,
            ws,
            serve_thread: Some(serve_thread),
            reader_thread: Some(reader_thread),
            _tmp: tmp,
        }
    }

    fn send_frame(&mut self, body: &Value) {
        let text = serde_json::to_string(body).unwrap();
        let framed = format!("Content-Length: {}\r\n\r\n{}", text.len(), text);
        self.to_server.write_all(framed.as_bytes()).unwrap();
        self.to_server.flush().unwrap();
    }

    /// Send a request and block until its response (by id) arrives, buffering any
    /// notifications seen in the meantime.
    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send_frame(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));

        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            if let Some(pos) = self
                .pending
                .iter()
                .position(|m| m.get("id") == Some(&json!(id)))
            {
                return self.pending.remove(pos);
            }
            let now = Instant::now();
            assert!(now < deadline, "timeout awaiting response to {method}");
            match self.inbound.recv_timeout(deadline - now) {
                Ok(message) => {
                    if message.get("id") == Some(&json!(id)) {
                        return message;
                    }
                    self.pending.push(message);
                }
                Err(RecvTimeoutError::Timeout) => panic!("timeout awaiting response to {method}"),
                Err(RecvTimeoutError::Disconnected) => panic!("server closed awaiting {method}"),
            }
        }
    }

    fn result(&mut self, method: &str, params: Value) -> Value {
        let response = self.request(method, params);
        assert!(
            response.get("error").is_none(),
            "{method} returned an error: {response}"
        );
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    /// The typed error message of a request expected to fail.
    fn error_message(&mut self, method: &str, params: Value) -> String {
        let response = self.request(method, params);
        response
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{method} did not return an error: {response}"))
            .to_string()
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send_frame(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    fn file_uri(&self, rel: &str) -> String {
        path_to_uri(&self.ws.join(rel))
    }

    fn text_doc(&self, rel: &str) -> Value {
        json!({"uri": self.file_uri(rel)})
    }

    fn initialize(&mut self) {
        let root = path_to_uri(&self.ws);
        self.result(
            "initialize",
            json!({"processId": null, "rootUri": root, "capabilities": {}}),
        );
        self.notify("initialized", json!({}));
    }

    fn execute_command(&mut self, command: &str) -> String {
        self.result(
            "workspace/executeCommand",
            json!({"command": command, "arguments": []}),
        )
        .as_str()
        .unwrap_or("")
        .to_string()
    }

    /// Poll the doctor until the bootstrap reaches Ready (a real mill compile +
    /// ingest is slow), then return the ready doctor report.
    fn await_ready(&mut self) -> String {
        let deadline = Instant::now() + Duration::from_secs(600);
        loop {
            let report = self.execute_command(DOCTOR);
            if report.contains("state: ready") {
                return report;
            }
            assert!(
                Instant::now() < deadline,
                "bootstrap never reached ready:\n{report}"
            );
            thread::sleep(Duration::from_millis(200));
        }
    }

    /// Drain published diagnostics for `rel` until one satisfies `pred` (or time
    /// out). Consults already-buffered notifications first.
    fn await_publish(
        &mut self,
        rel: &str,
        pred: impl Fn(&[Value]) -> bool,
        what: &str,
    ) -> Vec<Value> {
        let uri = self.file_uri(rel);
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            if let Some(hit) = take_publish(&mut self.pending, &uri, &pred) {
                return hit;
            }
            let now = Instant::now();
            assert!(now < deadline, "timeout awaiting {what}");
            match self.inbound.recv_timeout(deadline - now) {
                Ok(message) => self.pending.push(message),
                Err(RecvTimeoutError::Timeout) => panic!("timeout awaiting {what}"),
                Err(RecvTimeoutError::Disconnected) => panic!("server closed awaiting {what}"),
            }
        }
    }

    fn did_open(&mut self, rel: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": self.file_uri(rel), "languageId": "scala", "version": 1, "text": text}}),
        );
    }

    fn did_close(&mut self, rel: &str) {
        self.notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": self.file_uri(rel)}}),
        );
    }

    /// Overwrite the on-disk source and fire didSave (the debounced compile +
    /// reingest pipeline keys off the save notification).
    fn save(&mut self, rel: &str, text: &str) {
        std::fs::write(self.ws.join(rel), text).unwrap();
        self.notify(
            "textDocument/didSave",
            json!({"textDocument": {"uri": self.file_uri(rel)}, "text": text}),
        );
    }

    fn shutdown(mut self) {
        self.result("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        drop(self.to_server);
        if let Some(handle) = self.serve_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
    }
}

/// Remove and return the diagnostics of the first buffered publish for `uri`
/// whose diagnostic list satisfies `pred`.
fn take_publish(
    pending: &mut Vec<Value>,
    uri: &str,
    pred: &impl Fn(&[Value]) -> bool,
) -> Option<Vec<Value>> {
    let idx = pending.iter().position(|m| {
        m.get("method").and_then(Value::as_str) == Some("textDocument/publishDiagnostics")
            && m.get("params")
                .and_then(|p| p.get("uri"))
                .and_then(Value::as_str)
                == Some(uri)
            && {
                let diags = m
                    .get("params")
                    .and_then(|p| p.get("diagnostics"))
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                pred(&diags)
            }
    })?;
    let message = pending.remove(idx);
    Some(
        message
            .get("params")
            .and_then(|p| p.get("diagnostics"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    )
}

// --- source position helpers --------------------------------------------------

fn source_text(ws: &Path, rel: &str) -> String {
    std::fs::read_to_string(ws.join(rel)).unwrap()
}

/// The 0-based (line, character) of the start of the `nth` occurrence of `token`.
fn position_of(text: &str, token: &str, nth: usize) -> (u32, u32) {
    let mut seen = 0usize;
    for (line_no, line) in text.lines().enumerate() {
        let mut from = 0usize;
        while let Some(rel) = line[from..].find(token) {
            let col = from + rel;
            if seen == nth {
                return (line_no as u32, col as u32);
            }
            seen += 1;
            from = col + token.len();
        }
    }
    panic!("token {token:?} occurrence {nth} not found");
}

fn position_json(line: u32, character: u32) -> Value {
    json!({"line": line, "character": character})
}

/// The LSP range span of the `nth` occurrence of a single-line `token`.
fn span_of(text: &str, token: &str, nth: usize) -> Value {
    let (line, col) = position_of(text, token, nth);
    json!({
        "start": {"line": line, "character": col},
        "end": {"line": line, "character": col + token.len() as u32},
    })
}

/// How many times a whole occurrence of `token` appears across the given files.
fn count_token(ws: &Path, files: &[&str], token: &str) -> usize {
    files
        .iter()
        .map(|rel| source_text(ws, rel).matches(token).count())
        .sum()
}

// The workspace-relative sources the sample build indexes (a + b carry
// -Xsemanticdb; c does not).
const GREETING: &str = "a/src/pkga/Greeting.scala";
const INSIDE: &str = "a/src/pkga/Inside.scala";
const CONSUMER: &str = "b/src/pkgb/Consumer.scala";
const OTHER: &str = "b/src/pkgb/Other.scala";
const WIDGET: &str = "c/src/pkgc/Widget.scala";
const INDEXED: [&str; 4] = [GREETING, INSIDE, CONSUMER, OTHER];

fn skip(reason: &str) {
    eprintln!("{reason}");
}

// --- scenarios: index + BSP (mill only) ---------------------------------------

#[test]
fn real_bsp_doctor_symbol_references_and_rename_over_live_mill() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let root_uri = path_to_uri(&ws);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();

    // The doctor names the real mill server, reports ready, sees the Scala 3
    // targets, and flags the no-SemanticDB module + mill's own build target as a
    // hard SemanticDB-coverage error — while the indexable module a stays clean.
    let report = server.await_ready();
    assert!(report.contains("server: mill-bsp"), "{report}");
    assert!(report.contains(&format!("{root_uri}/a")), "{report}");
    assert!(report.contains(&format!("{root_uri}/b")), "{report}");
    let coverage = report
        .lines()
        .find(|l| l.contains("SemanticDB coverage:"))
        .unwrap_or("");
    assert!(
        coverage.contains("ERROR"),
        "expected a SemanticDB error: {report}"
    );
    assert!(coverage.contains(&format!("{root_uri}/c")), "{report}");
    assert!(
        !coverage.contains(&format!("{root_uri}/a")),
        "module a must be indexable: {report}"
    );

    // workspace/symbol finds the class declared in module a.
    let symbols = server.result("workspace/symbol", json!({"query": "Greeting"}));
    let greeting_uri = server.file_uri(GREETING);
    let found = symbols.as_array().unwrap().iter().any(|s| {
        s.get("name").and_then(Value::as_str) == Some("Greeting")
            && s.pointer("/location/uri").and_then(Value::as_str) == Some(greeting_uri.as_str())
    });
    assert!(found, "Greeting not found in module a: {symbols}");

    // references on a usage in b returns the exact cross-module, cross-file set:
    // every `message` occurrence across the indexed sources.
    let consumer_text = source_text(&ws, CONSUMER);
    let (line, character) = position_of(&consumer_text, "message", 0);
    let locations = server.result(
        "textDocument/references",
        json!({
            "textDocument": server.text_doc(CONSUMER),
            "position": position_json(line, character),
            "context": {"includeDeclaration": true},
        }),
    );
    let locs = locations.as_array().unwrap();
    let expected = count_token(&ws, &INDEXED, "message");
    assert_eq!(
        locs.len(),
        expected,
        "cross-module message set: {locations}"
    );
    let mut expected_set: Vec<(String, Value)> = INDEXED
        .iter()
        .flat_map(|rel| {
            let text = source_text(&ws, rel);
            let uri = server.file_uri(rel);
            (0..text.matches("message").count())
                .map(move |n| (uri.clone(), span_of(&text, "message", n)))
        })
        .collect();
    for loc in locs {
        let uri = loc.get("uri").and_then(Value::as_str).unwrap().to_string();
        let range = loc.get("range").cloned().unwrap();
        let pos = expected_set
            .iter()
            .position(|(u, r)| *u == uri && *r == range)
            .unwrap_or_else(|| panic!("unexpected reference {loc}"));
        expected_set.remove(pos);
    }
    assert!(
        expected_set.is_empty(),
        "missing references: {expected_set:?}"
    );

    // rename compiles through the real BSP server and edits every indexed module.
    let edit = server.result(
        "textDocument/rename",
        json!({
            "textDocument": server.text_doc(CONSUMER),
            "position": position_json(line, character),
            "newName": "note",
        }),
    );
    let changes = edit.get("changes").and_then(Value::as_object).unwrap();
    for rel in INDEXED {
        let uri = server.file_uri(rel);
        let text = source_text(&ws, rel);
        let edits = changes
            .get(&uri)
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("rename should edit {rel}: {edit}"));
        assert_eq!(
            edits.len(),
            text.matches("message").count(),
            "{rel}: {edits:?}"
        );
        for e in edits {
            assert_eq!(
                e.get("newText").and_then(Value::as_str),
                Some("note"),
                "{rel}"
            );
        }
    }

    server.shutdown();
}

#[test]
fn real_bsp_semanticdb_is_mandatory_on_the_uncovered_module() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.await_ready();

    let widget_text = source_text(&ws, WIDGET);
    let (line, character) = position_of(&widget_text, "area", 0);
    let pos = position_json(line, character);

    // The no-SemanticDB module is a hard error on both the PC and index paths —
    // never a quiet fallback nor an empty result.
    let completion = server.error_message(
        "textDocument/completion",
        json!({"textDocument": server.text_doc(WIDGET), "position": pos}),
    );
    assert!(
        completion.contains("has no SemanticDB output"),
        "{completion}"
    );
    assert!(completion.contains("-Xsemanticdb"), "{completion}");

    let highlight = server.error_message(
        "textDocument/documentHighlight",
        json!({"textDocument": server.text_doc(WIDGET), "position": pos}),
    );
    assert!(
        highlight.contains("has no SemanticDB output"),
        "{highlight}"
    );

    let rename = server.error_message(
        "textDocument/rename",
        json!({"textDocument": server.text_doc(WIDGET), "position": pos, "newName": "surface"}),
    );
    assert!(rename.contains("has no SemanticDB output"), "{rename}");

    server.shutdown();
}

#[test]
fn real_bsp_rename_rejections_carry_the_typed_reason() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.await_ready();

    let greeting_text = source_text(&ws, GREETING);

    // An external/library symbol (`String` in the constructor) is outside the
    // workspace and is rejected after the fresh compile+ingest.
    let (sl, sc) = position_of(&greeting_text, "String", 0);
    let external = server.error_message(
        "textDocument/rename",
        json!({"textDocument": server.text_doc(GREETING), "position": position_json(sl, sc), "newName": "Str"}),
    );
    assert!(external.contains("rename rejected"), "{external}");
    assert!(external.contains("outside the workspace"), "{external}");

    // A cursor inside the string literal has no symbol occurrence.
    let (wl, wc) = position_of(&greeting_text, "\"world\"", 0);
    let no_symbol = server.error_message(
        "textDocument/rename",
        json!({"textDocument": server.text_doc(GREETING), "position": position_json(wl, wc + 3), "newName": "planet"}),
    );
    assert!(no_symbol.contains("no symbol occurrence"), "{no_symbol}");

    server.shutdown();
}

#[test]
fn real_bsp_documenthighlight_returns_in_file_occurrences() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.await_ready();

    // documentHighlight is served from the index — both `name` occurrences in
    // Greeting.scala, and nothing from other files.
    let greeting_text = source_text(&ws, GREETING);
    let (line, character) = position_of(&greeting_text, "name", 0);
    let highlights = server.result(
        "textDocument/documentHighlight",
        json!({"textDocument": server.text_doc(GREETING), "position": position_json(line, character)}),
    );
    let spans: Vec<Value> = highlights
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h.get("range").cloned().unwrap())
        .collect();
    let expected = greeting_text.matches("name").count();
    assert_eq!(
        spans.len(),
        expected,
        "in-file name occurrences: {highlights}"
    );

    server.shutdown();
}

#[test]
fn real_bsp_compile_error_is_forwarded_then_cleared_by_the_fix() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.await_ready();

    let original = source_text(&ws, CONSUMER);
    // `message` is a String, so re-typing `text` as Int fails to compile.
    let broken = original.replace("val text: String =", "val text: Int =");
    assert_ne!(broken, original, "fixture text changed; update the edit");

    server.save(CONSUMER, &broken);
    let errors = server.await_publish(
        CONSUMER,
        |diags| {
            diags
                .iter()
                .any(|d| d.get("severity").and_then(Value::as_i64) == Some(1))
        },
        "an error diagnostic on the broken save",
    );
    assert!(
        errors
            .iter()
            .any(|d| d.get("severity").and_then(Value::as_i64) == Some(1)),
        "expected an error-severity diagnostic: {errors:?}"
    );

    // Fix and save — the file republishes an error-free diagnostic list.
    server.save(CONSUMER, &original);
    server.await_publish(
        CONSUMER,
        |diags| {
            diags
                .iter()
                .all(|d| d.get("severity").and_then(Value::as_i64) != Some(1))
        },
        "the cleared diagnostics after the fix",
    );

    server.shutdown();
}

#[test]
fn real_bsp_save_driven_reingest_reflects_new_token_positions() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.await_ready();

    let original = source_text(&ws, CONSUMER);
    // Insert a pad line ABOVE the usage, shifting `greeting.message` one line down.
    // The pad line must not contain `message`, or the span search would match it.
    let moved = original.replace(
        "  val text: String = greeting.message",
        "  // pad line to shift the usage down\n  val text: String = greeting.message",
    );
    assert_ne!(moved, original, "fixture text changed; update the edit");
    let old_span = span_of(&original, "message", 0);
    let new_span = span_of(&moved, "message", 0);
    assert_ne!(old_span, new_span, "the edit must shift the token");

    server.save(CONSUMER, &moved);
    let consumer_uri = server.file_uri(CONSUMER);
    let (line, character) = position_of(&moved, "message", 0);
    let query = json!({
        "textDocument": server.text_doc(CONSUMER),
        "position": position_json(line, character),
        "context": {"includeDeclaration": true},
    });

    // The debounced pipeline compiles then re-ingests with NO explicit reindex;
    // poll references (the wire-observable effect) until the moved span appears.
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let locations = server.result("textDocument/references", query.clone());
        let here: Vec<Value> = locations
            .as_array()
            .unwrap()
            .iter()
            .filter(|l| l.get("uri").and_then(Value::as_str) == Some(consumer_uri.as_str()))
            .map(|l| l.get("range").cloned().unwrap())
            .collect();
        if here.contains(&new_span) {
            assert!(!here.contains(&old_span), "stale span survived: {here:?}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "reingest never reflected the moved span"
        );
        thread::sleep(Duration::from_millis(250));
    }

    server.shutdown();
}

#[test]
fn real_bsp_shared_source_unifies_references_and_passes_rename_consistency() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let shared_rel = "shared/src/pkgshared/Shared.scala";
    let shared_source =
        "package pkgshared\n\nobject Shared:\n  def marker: String = \"shared-marker\"\n";
    // A build where a and d BOTH compile shared/src, so Shared.scala is a source
    // shared across two targets and the index holds two documents for its uri.
    let shared_build = r#"//| mill-version: 1.1.2
//| mill-jvm-version: system
package build

import mill.*
import mill.scalalib.*

trait SampleModule extends ScalaModule {
  def scalaVersion = "3.8.4"
  def scalacOptions = Seq(
    "-Xsemanticdb",
    "-sourceroot",
    mill.api.BuildCtx.workspaceRoot.toString
  )
}

object a extends SampleModule {
  def sources = Task.Sources(
    mill.api.BuildCtx.workspaceRoot / "a" / "src",
    mill.api.BuildCtx.workspaceRoot / "shared" / "src"
  )
}

object d extends SampleModule {
  def sources = Task.Sources(mill.api.BuildCtx.workspaceRoot / "shared" / "src")
}

object b extends SampleModule {
  def moduleDeps = Seq(a)
}

object c extends ScalaModule {
  def scalaVersion = "3.8.4"
}
"#;
    let (tmp, ws) = prepare_workspace(&[(shared_rel, shared_source)], Some(shared_build));
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.await_ready();

    let shared_text = source_text(&ws, shared_rel);
    let shared_uri = server.file_uri(shared_rel);
    let (line, character) = position_of(&shared_text, "marker", 0);
    let marker_span = span_of(&shared_text, "marker", 0);

    // references on the shared symbol unify to ONE location for the shared uri,
    // not one per compiling target.
    let locations = server.result(
        "textDocument/references",
        json!({
            "textDocument": server.text_doc(shared_rel),
            "position": position_json(line, character),
            "context": {"includeDeclaration": true},
        }),
    );
    let here: Vec<Value> = locations
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| {
            l.get("uri").and_then(Value::as_str) == Some(shared_uri.as_str())
                && l.get("range") == Some(&marker_span)
        })
        .cloned()
        .collect();
    assert_eq!(here.len(), 1, "shared occurrence must unify: {locations}");

    // rename runs the shared-source consistency check across both target views;
    // the views agree, so it succeeds and edits the shared file.
    let edit = server.result(
        "textDocument/rename",
        json!({
            "textDocument": server.text_doc(shared_rel),
            "position": position_json(line, character),
            "newName": "flag",
        }),
    );
    let edits = edit
        .pointer("/changes")
        .and_then(Value::as_object)
        .and_then(|c| c.get(&shared_uri))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("rename should edit the shared source: {edit}"));
    assert!(
        edits.iter().any(|e| e.get("range") == Some(&marker_span)),
        "{edits:?}"
    );
    assert!(
        edits
            .iter()
            .all(|e| e.get("newText").and_then(Value::as_str) == Some("flag")),
        "{edits:?}"
    );

    server.shutdown();
}

// --- scenarios: presentation compiler (mill + JVM) ----------------------------

#[test]
fn real_bsp_presentation_compiler_answers_position_features() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    if !pc_enabled() {
        return skip(
            "real_bsp: skipping PC features — set LS_LIBJVM + PC_HOST_AGENT_JAR + \
             LS_PC_TARGET_CLASSPATH to run them",
        );
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.await_ready();

    let greeting_text = source_text(&ws, GREETING);
    server.did_open(GREETING, &greeting_text);

    // hover (PC) answers on an indexed symbol.
    let (ml, mc) = position_of(&greeting_text, "message", 0);
    let hover = server.result(
        "textDocument/hover",
        json!({"textDocument": server.text_doc(GREETING), "position": position_json(ml, mc)}),
    );
    assert!(!hover.is_null(), "expected a non-null hover for message");

    // signatureHelp (PC) answers at the constructor call site.
    let (cl, cc) = position_of(&greeting_text, "new Greeting(", 0);
    let sig = server.result(
        "textDocument/signatureHelp",
        json!({"textDocument": server.text_doc(GREETING), "position": position_json(cl, cc + "new Greeting(".len() as u32)}),
    );
    let signatures = sig.get("signatures").and_then(Value::as_array);
    assert!(
        signatures.is_some_and(|s| !s.is_empty()),
        "expected a signature: {sig}"
    );

    // definition (PC) resolves the `Greeting` in `new Greeting("world")` (the 4th
    // whole occurrence) to its declaration in the same file.
    let (dl, dc) = position_of(&greeting_text, "Greeting", 3);
    let definition = server.result(
        "textDocument/definition",
        json!({"textDocument": server.text_doc(GREETING), "position": position_json(dl, dc)}),
    );
    let greeting_uri = server.file_uri(GREETING);
    let resolved = definition
        .as_array()
        .map(|locs| {
            locs.iter()
                .any(|l| l.get("uri").and_then(Value::as_str) == Some(greeting_uri.as_str()))
        })
        .unwrap_or(false);
    assert!(
        resolved,
        "expected the definition in Greeting.scala: {definition}"
    );

    server.did_close(GREETING);
    server.shutdown();
}

#[test]
fn real_bsp_forked_completion_answers_on_a_dirty_buffer() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    if !pc_enabled() {
        return skip(
            "real_bsp: skipping forked completion — set LS_LIBJVM + PC_HOST_AGENT_JAR + \
             LS_PC_TARGET_CLASSPATH to run it",
        );
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.await_ready();

    // A dirty buffer with a member-select the forked worker must complete against
    // the real classpath. The OS-level worker-kill/respawn fault injection is
    // owned by the island boundary suite (which has the worker pid); here the
    // forked worker answers over the live BSP classpath and the doctor reports it
    // alive.
    let probe = "  val probe = greeting.mess";
    let dirty = format!("{}{probe}\n", source_text(&ws, CONSUMER));
    server.did_open(CONSUMER, &dirty);
    let line = dirty.lines().count() as u32 - 1;
    let character = probe.len() as u32;
    let completion = server.result(
        "textDocument/completion",
        json!({"textDocument": server.text_doc(CONSUMER), "position": position_json(line, character)}),
    );
    let items = completion
        .get("items")
        .or(Some(&completion))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    assert!(
        items.iter().any(|i| i
            .get("label")
            .and_then(Value::as_str)
            .is_some_and(|l| l.starts_with("message"))),
        "forked completion should offer message: {completion}"
    );
    assert!(
        server
            .execute_command(DOCTOR)
            .contains("forked worker alive"),
        "doctor should report a live forked worker"
    );

    server.did_close(CONSUMER);
    server.shutdown();
}

// --- cold start: index-only queries leave the process JVM-free ----------------

#[test]
fn real_bsp_cold_start_serves_index_queries_with_no_jvm() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    // `libjvm_mapped()` reads /proc/self/maps and is process-global + sticky, and
    // the server here runs on a worker thread of THIS test process — so a sibling
    // PC scenario (`..._answers_position_features` / `..._forked_completion...`)
    // that boots the in-process island under the full PC env would map libjvm and
    // false-fail this assertion regardless of test ordering. Assert the cold-start
    // property only in the mill-only config, where nothing in the binary can boot
    // the JVM (the same guard the fake-BSP cold-start sibling uses).
    if pc_enabled() {
        return skip(
            "real_bsp: skipping the cold-start JVM check — a concurrent PC scenario may boot the JVM",
        );
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.await_ready();

    // Index-only queries (symbol + references) over the live mill model, and NO
    // presentation-compiler request — the embedded island must stay unbooted.
    server.result("workspace/symbol", json!({"query": "Greeting"}));
    let consumer_text = source_text(&ws, CONSUMER);
    let (line, character) = position_of(&consumer_text, "message", 0);
    server.result(
        "textDocument/references",
        json!({
            "textDocument": server.text_doc(CONSUMER),
            "position": position_json(line, character),
            "context": {"includeDeclaration": true},
        }),
    );
    assert!(
        !ls_server::libjvm_mapped(),
        "an index-only session over live mill must not map libjvm"
    );

    server.shutdown();
    assert!(
        !ls_server::libjvm_mapped(),
        "no PC request ran, so the JVM must never have booted"
    );
}
