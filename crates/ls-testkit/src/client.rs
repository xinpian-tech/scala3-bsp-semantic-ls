//! The interactive framed-LSP wire client — the one copy of what
//! `real_bsp_common::RealServer` used to host, generalized over how the server
//! runs: [`WireClient::boot_in_process`] drives the production `serve` loop on
//! worker threads over `UnixStream` pipes with any injected [`Bootstrap`];
//! [`WireClient::spawn_binary`] spawns a real server binary and drives it over
//! its actual stdin/stdout — the editor's-eye view.

use std::collections::HashSet;
use std::io::{BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use ls_bsp::uri::path_to_uri;
use ls_server::{
    read_frame, serve, Bootstrap, CoreHandlers, CoreServices, OutputSink, RequestId, ServerCore,
};

pub const DOCTOR: &str = "scala3SemanticLs.doctor";
pub const COMPILE: &str = "scala3SemanticLs.compile";
pub const REINDEX: &str = "scala3SemanticLs.reindex";

/// The pre-boot pieces an in-process bootstrap builder may wire against: the
/// shared output sink (responses and diagnostics go through the one lock, as in
/// production) and the server core's reload flag (fired by
/// `buildTarget/didChange`).
pub struct WireParts {
    pub sink: Arc<OutputSink<UnixStream>>,
    pub reload_flag: Arc<AtomicBool>,
    /// The serve loop's cancel-set handle (`$/cancelRequest` interception), so a
    /// suite can wait until a cancel has provably been intercepted before
    /// releasing a gated in-flight request.
    pub cancel_handle: Arc<Mutex<HashSet<RequestId>>>,
}

/// An interactive client on the framed LSP wire: requests block on their
/// matching response id while notifications (diagnostics) accumulate for
/// [`WireClient::await_publish`].
pub struct WireClient {
    to_server: Option<Box<dyn Write + Send>>,
    inbound: Receiver<Value>,
    pending: Vec<Value>,
    next_id: i64,
    ws: PathBuf,
    serve_thread: Option<JoinHandle<()>>,
    reader_thread: Option<JoinHandle<()>>,
    child: Option<Child>,
    /// The in-process serve loop's cancel-set handle (`None` for a spawned
    /// binary, whose internals are not observable).
    cancel_handle: Option<Arc<Mutex<HashSet<RequestId>>>>,
    /// Fixtures the session depends on for its lifetime (e.g. the fake BSP
    /// server handle owning the temp workspace); dropped with the client.
    _keepalive: Option<Box<dyn std::any::Any + Send>>,
}

impl WireClient {
    /// Boot the production `serve` loop (with the production [`CoreHandlers`])
    /// on a worker thread over `UnixStream` pipes; `build` returns the
    /// [`Bootstrap`] the loop drives on `initialized` — the production
    /// `IndexBootstrap` over a live or fake model source, with a real island or
    /// an injected fake PC.
    pub fn boot_in_process<B>(ws: PathBuf, build: impl FnOnce(&WireParts) -> B) -> WireClient
    where
        B: Bootstrap<CoreServices> + Send + Sync + 'static,
    {
        Self::boot_in_process_with(move |parts| (ws, (), build(parts)))
    }

    /// Like [`WireClient::boot_in_process`], but the builder also yields the
    /// workspace root and a keepalive value (e.g. the [`crate::fake_bsp::FakeBsp`]
    /// handle owning the temp workspace) the client holds for the session.
    pub fn boot_in_process_with<B, K>(
        build: impl FnOnce(&WireParts) -> (PathBuf, K, B),
    ) -> WireClient
    where
        B: Bootstrap<CoreServices> + Send + Sync + 'static,
        K: Send + 'static,
    {
        // client -> server (the server reads this as its input)
        let (client_write, server_read) = UnixStream::pair().unwrap();
        // server -> client (the server writes framed output here)
        let (server_write, client_read) = UnixStream::pair().unwrap();

        let mut core = ServerCore::new();
        let sink = Arc::new(OutputSink::new(server_write));
        let cancel_handle = core.cancel_handle();
        let parts = WireParts {
            sink: Arc::clone(&sink),
            reload_flag: core.reload_flag(),
            cancel_handle: Arc::clone(&cancel_handle),
        };
        let (ws, keepalive, bootstrap) = build(&parts);
        let serve_thread = thread::spawn(move || {
            let mut reader = BufReader::new(server_read);
            let _ = serve(
                &mut reader,
                sink.as_ref(),
                &mut core,
                &CoreHandlers,
                bootstrap,
            );
        });

        let (reader_thread, inbound) = demux(client_read);
        WireClient {
            to_server: Some(Box::new(client_write)),
            inbound,
            pending: Vec::new(),
            next_id: 1,
            ws,
            serve_thread: Some(serve_thread),
            reader_thread: Some(reader_thread),
            child: None,
            cancel_handle: Some(cancel_handle),
            _keepalive: Some(Box::new(keepalive)),
        }
    }

    /// Spawn a real server binary and drive it over its actual stdin/stdout.
    /// stderr is inherited (logs land in the test output).
    pub fn spawn_binary(bin: &Path, ws: PathBuf, envs: &[(&str, &str)]) -> WireClient {
        let mut command = Command::new(bin);
        command.stdin(Stdio::piped()).stdout(Stdio::piped());
        for (key, value) in envs {
            command.env(key, value);
        }
        let mut child = command.spawn().expect("spawn server binary");
        let stdin = child.stdin.take().expect("child stdin");
        let stdout = child.stdout.take().expect("child stdout");
        let (reader_thread, inbound) = demux(stdout);
        WireClient {
            to_server: Some(Box::new(stdin)),
            inbound,
            pending: Vec::new(),
            next_id: 1,
            ws,
            serve_thread: None,
            reader_thread: Some(reader_thread),
            child: Some(child),
            cancel_handle: None,
            _keepalive: None,
        }
    }

    fn send_frame(&mut self, body: &Value) {
        let text = serde_json::to_string(body).unwrap();
        let framed = format!("Content-Length: {}\r\n\r\n{}", text.len(), text);
        let writer = self.to_server.as_mut().expect("client already shut down");
        writer.write_all(framed.as_bytes()).unwrap();
        writer.flush().unwrap();
    }

    /// Send a request and block until its response (by id) arrives, buffering
    /// any notifications seen in the meantime.
    pub fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.send_request_no_wait(method, params);
        self.await_response_for(id, method)
    }

    /// Send a request WITHOUT waiting for its response; returns the id, for
    /// [`WireClient::await_response`] (or [`WireClient::cancel`]) later.
    pub fn send_request_no_wait(&mut self, method: &str, params: Value) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        self.send_frame(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}));
        id
    }

    /// Send a `$/cancelRequest` notification for a previously sent request id.
    pub fn cancel(&mut self, id: i64) {
        self.notify("$/cancelRequest", json!({ "id": id }));
    }

    /// In-process sessions only: block until the serve loop's reader thread has
    /// intercepted the `$/cancelRequest` for `id` — the deterministic fence
    /// between sending a cancel and releasing a gated in-flight request. (The
    /// entry is consumed at dispatch, so fence BEFORE the cancelled request's
    /// turn comes.)
    pub fn await_cancel_registered(&self, id: i64) {
        let handle = self
            .cancel_handle
            .as_ref()
            .expect("cancel introspection needs an in-process session");
        let deadline = Instant::now() + Duration::from_secs(60);
        while !handle.lock().unwrap().contains(&RequestId::Number(id)) {
            assert!(
                Instant::now() < deadline,
                "the cancel for id {id} was never intercepted"
            );
            thread::sleep(Duration::from_millis(2));
        }
    }

    /// Block until the response for an already-sent request id arrives,
    /// buffering any notifications seen in the meantime.
    pub fn await_response(&mut self, id: i64) -> Value {
        self.await_response_for(id, "request")
    }

    fn await_response_for(&mut self, id: i64, method: &str) -> Value {
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

    /// Poll the doctor until the bootstrap reaches Ready, then return the ready
    /// doctor report.
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

    /// Reach Ready, then run the first-editor-session flow — compile over the
    /// BSP session, then a reindex that ingests the produced SemanticDB — and
    /// return the post-fill doctor report. (Real-build workspaces need this
    /// before index queries answer; the committed fixture corpus does not.)
    pub fn ready(&mut self) -> String {
        self.await_ready();
        let compiled = self.execute_command(COMPILE);
        assert!(
            compiled.starts_with("compile ok"),
            "BSP compile failed: {compiled}"
        );
        let reindexed = self.execute_command(REINDEX);
        assert!(
            reindexed.starts_with("ingest: segment"),
            "reindex failed: {reindexed}"
        );
        self.execute_command(DOCTOR)
    }

    /// Drain published diagnostics for `rel` until one satisfies `pred` (or
    /// time out). Consults already-buffered notifications first.
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
        let uri = self.file_uri(rel);
        self.did_open_uri(&uri, text);
    }

    /// `didOpen` with a full URI (fixture-corpus sources live outside the
    /// workspace root, so wire suites open them by URI).
    pub fn did_open_uri(&mut self, uri: &str, text: &str) {
        let uri = uri.to_string();
        self.notify(
            "textDocument/didOpen",
            json!({"textDocument": {"uri": uri, "languageId": "scala", "version": 1, "text": text}}),
        );
    }

    pub fn did_change_uri(&mut self, uri: &str, text: &str) {
        self.notify(
            "textDocument/didChange",
            json!({"textDocument": {"uri": uri, "version": 2}, "contentChanges": [{"text": text}]}),
        );
    }

    /// A RANGED `didChange` (incremental sync): one contentChanges event whose
    /// UTF-16 `[start, end)` range replaces that span with `text`.
    #[allow(clippy::too_many_arguments)]
    pub fn did_change_range_uri(
        &mut self,
        uri: &str,
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
        text: &str,
        version: i64,
    ) {
        self.notify(
            "textDocument/didChange",
            json!({
                "textDocument": {"uri": uri, "version": version},
                "contentChanges": [{
                    "range": {
                        "start": {"line": start_line, "character": start_char},
                        "end": {"line": end_line, "character": end_char},
                    },
                    "text": text,
                }],
            }),
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
        drop(self.to_server.take());
        if let Some(handle) = self.serve_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.reader_thread.take() {
            let _ = handle.join();
        }
        if let Some(mut child) = self.child.take() {
            let _ = child.wait();
        }
    }
}

impl Drop for WireClient {
    fn drop(&mut self) {
        // A test that panics mid-session must not leave a server process behind.
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Demultiplex framed server output into a channel on a reader thread.
fn demux<R: std::io::Read + Send + 'static>(stream: R) -> (JoinHandle<()>, Receiver<Value>) {
    let (tx, rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut reader = BufReader::new(stream);
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
    (handle, rx)
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
