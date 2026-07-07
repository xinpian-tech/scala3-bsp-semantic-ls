//! Shared harness for the real-BSP end-to-end suites: an interactive LSP-over-
//! `UnixStream` client that drives the whole production server (`serve` loop +
//! `IndexBootstrap` over the production `LiveBspModelSource`) against a REAL mill
//! build server built from `it/sample-workspace`. Split into its own module so
//! each live-presentation-compiler scenario can live in its OWN integration-test
//! binary — only one embedded JVM/island can boot per process, so the index/BSP
//! rows, the position-feature PC rows, and the faulted dispatch-generation
//! recovery each run in a separate process.
#![allow(dead_code)]

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
pub fn mill_enabled() -> bool {
    std::env::var_os("LS_REAL_BSP_IT").is_some()
}

/// The presentation-compiler scenarios additionally need a real embedded JVM.
pub fn pc_enabled() -> bool {
    std::env::var_os("LS_LIBJVM").is_some()
        && std::env::var_os("PC_HOST_AGENT_JAR").is_some()
        && std::env::var_os("LS_PC_TARGET_CLASSPATH").is_some()
}

pub const DOCTOR: &str = "scala3SemanticLs.doctor";
pub const COMPILE: &str = "scala3SemanticLs.compile";
pub const REINDEX: &str = "scala3SemanticLs.reindex";

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

/// A production server driven over two `UnixStream` pipes — the framed LSP wire.
/// The `serve` loop and the real `LiveBspModelSource` bootstrap run on a worker
/// thread; a reader thread demultiplexes server→client frames into a channel so
/// requests block on their matching response id while notifications (diagnostics)
/// accumulate for `await_publish`.
pub struct RealServer {
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
    pub fn boot(tmp: tempfile::TempDir, ws: PathBuf) -> RealServer {
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
    pub fn request(&mut self, method: &str, params: Value) -> Value {
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

    pub fn result(&mut self, method: &str, params: Value) -> Value {
        let response = self.request(method, params);
        assert!(
            response.get("error").is_none(),
            "{method} returned an error: {response}"
        );
        response.get("result").cloned().unwrap_or(Value::Null)
    }

    /// The typed error message of a request expected to fail.
    pub fn error_message(&mut self, method: &str, params: Value) -> String {
        let response = self.request(method, params);
        response
            .get("error")
            .and_then(|e| e.get("message"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("{method} did not return an error: {response}"))
            .to_string()
    }

    pub fn notify(&mut self, method: &str, params: Value) {
        self.send_frame(&json!({"jsonrpc": "2.0", "method": method, "params": params}));
    }

    pub fn file_uri(&self, rel: &str) -> String {
        path_to_uri(&self.ws.join(rel))
    }

    pub fn text_doc(&self, rel: &str) -> Value {
        json!({"uri": self.file_uri(rel)})
    }

    pub fn workspace(&self) -> &Path {
        &self.ws
    }

    pub fn initialize(&mut self) {
        let root = path_to_uri(&self.ws);
        self.result(
            "initialize",
            json!({"processId": null, "rootUri": root, "capabilities": {}}),
        );
        self.notify("initialized", json!({}));
    }

    pub fn execute_command(&mut self, command: &str) -> String {
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
    pub fn await_ready(&mut self) -> String {
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

    /// Reach Ready, then drive the first-editor-session flow — a compile over the
    /// real BSP session (which produces the SemanticDB a fresh workspace has not
    /// emitted yet) then a reindex that ingests it — so the index is actually
    /// filled, and return the post-fill doctor report. This mirrors the Scala
    /// `RealBspServer.readyIndex` (compile + reindex); index queries answer empty
    /// until it runs. Neither command touches the presentation compiler, so the
    /// embedded island stays cold.
    pub fn ready(&mut self) -> String {
        self.await_ready();
        let compiled = self.execute_command(COMPILE);
        assert!(
            compiled.starts_with("compile ok"),
            "real BSP compile failed: {compiled}"
        );
        let reindexed = self.execute_command(REINDEX);
        assert!(
            reindexed.starts_with("ingest: segment"),
            "reindex failed: {reindexed}"
        );
        self.execute_command(DOCTOR)
    }

    /// Drain published diagnostics for `rel` until one satisfies `pred` (or time
    /// out). Consults already-buffered notifications first.
    pub fn await_publish(
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

    pub fn did_open(&mut self, rel: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": self.file_uri(rel), "languageId": "scala", "version": 1, "text": text}}),
        );
    }

    pub fn did_close(&mut self, rel: &str) {
        self.notify(
            "textDocument/didClose",
            json!({"textDocument": {"uri": self.file_uri(rel)}}),
        );
    }

    /// Overwrite the on-disk source and fire didSave (the debounced compile +
    /// reingest pipeline keys off the save notification).
    pub fn save(&mut self, rel: &str, text: &str) {
        std::fs::write(self.ws.join(rel), text).unwrap();
        self.notify(
            "textDocument/didSave",
            json!({"textDocument": {"uri": self.file_uri(rel)}, "text": text}),
        );
    }

    pub fn shutdown(mut self) {
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

pub fn source_text(ws: &Path, rel: &str) -> String {
    std::fs::read_to_string(ws.join(rel)).unwrap()
}

/// The 0-based (line, character) of the start of the `nth` occurrence of `token`.
pub fn position_of(text: &str, token: &str, nth: usize) -> (u32, u32) {
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

pub fn position_json(line: u32, character: u32) -> Value {
    json!({"line": line, "character": character})
}

/// The LSP range span of the `nth` occurrence of a single-line `token`.
pub fn span_of(text: &str, token: &str, nth: usize) -> Value {
    let (line, col) = position_of(text, token, nth);
    json!({
        "start": {"line": line, "character": col},
        "end": {"line": line, "character": col + token.len() as u32},
    })
}

/// How many times a whole occurrence of `token` appears across the given files.
pub fn count_token(ws: &Path, files: &[&str], token: &str) -> usize {
    files
        .iter()
        .map(|rel| source_text(ws, rel).matches(token).count())
        .sum()
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
