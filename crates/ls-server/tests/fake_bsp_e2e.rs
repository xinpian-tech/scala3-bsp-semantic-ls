//! Fake-BSP end-to-end: the whole server driven over the framed LSP wire against
//! an in-process Rust-hosted fake BSP server. Unlike the fixture-model suites,
//! bootstrap here loads the project model over the REAL BSP client (a real
//! `BspSession` speaking JSON-RPC to the fake server over a `UnixStream`) and
//! retains it in the REAL session-backed compiler, so `initialize`/`initialized`
//! handshake, model load, compile, diagnostics forwarding, and session teardown
//! all run through production code. A port of the Scala `LsEndToEndTest`.
//!
//! The fake server describes the committed `ls-engine` SemanticDB fixture corpus
//! (targets `fixture-a`/`fixture-b`/`fixture-c` with real `.semanticdb` under
//! `out-a`/`out-b`/`out-c`) plus one `fixture-nosdb` target compiled WITHOUT
//! `-Xsemanticdb`, so its source stays a hard `NoSemanticdb` error (SemanticDB
//! is mandatory — a source in such a target is a hard error, never empty). It
//! drives capabilities + the real BSP handshake, index queries, compile-diagnostic
//! forwarding (publish / clear / clear-once suppression), rename over the retained
//! compiler (success + compile failure), `buildTarget/didChange` reload, the
//! dirty-buffer PC-only surface (JVM-free), and LSP shutdown teardown.
//!
//! The presentation-compiler completion scenario needs a real embedded JVM and is
//! env-gated exactly like `live_pc.rs` (skips cleanly when the JVM env is absent).

use std::collections::VecDeque;
use std::io::{BufReader, Cursor, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use ls_bsp::protocol::PublishDiagnosticsParams as BspPublishDiagnosticsParams;
use ls_bsp::uri::path_to_uri;
use ls_bsp::wire::{read_message, write_message};
use ls_bsp::{BspClientHandlers, BspSession, BspSessionConfig};
use ls_server::{
    read_frame, ready_model_from_session, serve, CoreHandlers, DiagnosticRouter, IndexBootstrap,
    LoadOutcome, ModelSource, OutputSink, ServerCore,
};

// --- fixture corpus geometry --------------------------------------------------

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ls-engine/tests/fixtures")
}

fn target_id(name: &str) -> String {
    format!("bsp://workspace/{name}")
}

// --- in-process fake BSP server -----------------------------------------------

/// How the fake should react to the next `buildTarget/compile`: which status to
/// return, which `build/publishDiagnostics` notifications to emit first, and
/// whether to fire a `buildTarget/didChange` (swapping the advertised targets so a
/// subsequent model refetch sees the reloaded set).
#[derive(Clone)]
struct CompileScript {
    status: i64,
    diagnostics: Vec<Value>,
    reload_to: Option<Vec<Value>>,
}

impl CompileScript {
    fn ok() -> CompileScript {
        CompileScript {
            status: 1,
            diagnostics: Vec::new(),
            reload_to: None,
        }
    }
}

/// A fake BSP server over a `UnixStream`, describing the fixture corpus. The three
/// indexable targets carry `-Xsemanticdb` pointing at the committed `out-*`
/// targetroots; `fixture-nosdb` carries no SemanticDB flag. Its reaction to
/// `buildTarget/compile` (status, diagnostics, reload) is scriptable per test.
struct FakeBuildServer {
    sources_root: PathBuf,
    nosdb_source: PathBuf,
    initialize_received: AtomicBool,
    initialized_notified: AtomicBool,
    shutdown_requested: AtomicBool,
    exit_received: AtomicBool,
    /// The build targets advertised for `workspace/buildTargets`; a `didChange`
    /// reload swaps this so a refetch observes the new set.
    current_targets: Mutex<Vec<Value>>,
    /// Scripts consumed one per `buildTarget/compile`; empties to a plain success.
    compile_scripts: Mutex<VecDeque<CompileScript>>,
    /// The target ids seen across all compile requests (rename asserts a compile ran).
    compiled_targets: Mutex<Vec<String>>,
}

impl FakeBuildServer {
    fn new(nosdb_source: PathBuf) -> FakeBuildServer {
        FakeBuildServer {
            sources_root: fixtures_root().join("sources"),
            nosdb_source,
            initialize_received: AtomicBool::new(false),
            initialized_notified: AtomicBool::new(false),
            shutdown_requested: AtomicBool::new(false),
            exit_received: AtomicBool::new(false),
            current_targets: Mutex::new(default_targets()),
            compile_scripts: Mutex::new(VecDeque::new()),
            compiled_targets: Mutex::new(Vec::new()),
        }
    }

    /// Advertise a restricted initial target set (for the reload scenario).
    fn set_targets(&self, targets: Vec<Value>) {
        *self.current_targets.lock().unwrap() = targets;
    }

    /// Queue the fake's reaction to the next compile.
    fn script_compile(&self, script: CompileScript) {
        self.compile_scripts.lock().unwrap().push_back(script);
    }

    /// Returns false to stop serving (on `build/exit`).
    fn handle(&self, msg: &Value, writer: &mut UnixStream) -> bool {
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let id = msg.get("id").cloned();
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        match method {
            "build/initialize" => {
                self.initialize_received.store(true, Ordering::SeqCst);
                reply(writer, id, self.initialize_result());
            }
            "build/initialized" => self.initialized_notified.store(true, Ordering::SeqCst),
            "build/shutdown" => {
                self.shutdown_requested.store(true, Ordering::SeqCst);
                reply(writer, id, Value::Null);
            }
            "build/exit" => {
                self.exit_received.store(true, Ordering::SeqCst);
                return false;
            }
            "workspace/buildTargets" => reply(writer, id, self.build_targets()),
            "buildTarget/sources" => reply(writer, id, self.sources(&params)),
            "buildTarget/scalacOptions" => reply(writer, id, self.scalac_options(&params)),
            "buildTarget/compile" => self.compile(writer, id, &params),
            _ => {
                if let Some(id) = id {
                    reply_error(writer, id, -32601, &format!("method not found: {method}"));
                }
            }
        }
        true
    }

    fn initialize_result(&self) -> Value {
        json!({
            "displayName": "fake-bsp-server",
            "version": "0.0.1",
            "bspVersion": "2.1.0",
            "capabilities": { "compileProvider": { "languageIds": ["scala"] } }
        })
    }

    fn build_targets(&self) -> Value {
        json!({ "targets": self.current_targets.lock().unwrap().clone() })
    }

    /// Runs the queued compile script (or a plain success): records the requested
    /// target ids, emits any scripted `build/publishDiagnostics`, optionally fires a
    /// `buildTarget/didChange` + swaps the advertised targets, then replies status.
    fn compile(&self, writer: &mut UnixStream, id: Option<Value>, params: &Value) {
        self.compiled_targets
            .lock()
            .unwrap()
            .extend(requested_names(params));
        let script = self
            .compile_scripts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(CompileScript::ok);
        for diagnostic in &script.diagnostics {
            notify(writer, "build/publishDiagnostics", diagnostic.clone());
        }
        if let Some(reloaded) = script.reload_to {
            *self.current_targets.lock().unwrap() = reloaded;
            notify(writer, "buildTarget/didChange", json!({ "changes": [] }));
        }
        reply(writer, id, json!({ "statusCode": script.status }));
    }

    fn sources(&self, params: &Value) -> Value {
        let items: Vec<Value> = requested_names(params)
            .iter()
            .map(|name| self.source_item(name))
            .collect();
        json!({ "items": items })
    }

    fn source_item(&self, name: &str) -> Value {
        // `dir` items (kind 2) are expanded to their `.scala` files by the loader.
        let dir = |rel: &str| json!({ "uri": path_to_uri(&self.sources_root.join(rel)), "kind": 2, "generated": false });
        let sources = match name {
            // `fixture-a` primary-owns `a`, `shared`, and `dep`.
            "fixture-a" => vec![dir("a"), dir("shared"), dir("dep")],
            "fixture-b" => vec![dir("b")],
            "fixture-c" => vec![dir("c")],
            // A single file compiled without SemanticDB.
            "fixture-nosdb" => vec![json!({
                "uri": path_to_uri(&self.nosdb_source), "kind": 1, "generated": false
            })],
            _ => vec![],
        };
        json!({ "target": { "uri": target_id(name) }, "sources": sources })
    }

    fn scalac_options(&self, params: &Value) -> Value {
        let items: Vec<Value> = requested_names(params)
            .iter()
            .map(|name| self.scalac_option_item(name))
            .collect();
        json!({ "items": items })
    }

    fn scalac_option_item(&self, name: &str) -> Value {
        // The three indexable targets point their SemanticDB targetroot at the
        // committed `out-<x>` dir (absolute, so it resolves regardless of the BSP
        // workspace root) and their sourceroot at the fixture `sources` dir.
        let sdb = |out: &str| {
            vec![
                "-Xsemanticdb".to_string(),
                format!(
                    "-semanticdb-target:{}",
                    fixtures_root().join(out).to_string_lossy()
                ),
                "-sourceroot".to_string(),
                self.sources_root.to_string_lossy().into_owned(),
            ]
        };
        let options = match name {
            "fixture-a" => sdb("out-a"),
            "fixture-b" => sdb("out-b"),
            "fixture-c" => sdb("out-c"),
            // No SemanticDB flag: the target is IndexUnavailable.
            _ => vec!["-deprecation".to_string()],
        };
        json!({
            "target": { "uri": target_id(name) },
            "options": options,
            "classpath": [],
            "classDirectory": path_to_uri(&fixtures_root().join("out").join(name)),
        })
    }
}

fn build_target(name: &str, deps: &[&str]) -> Value {
    json!({
        "id": { "uri": target_id(name) },
        "displayName": name,
        "tags": [],
        "languageIds": ["scala"],
        "dependencies": deps.iter().map(|d| json!({ "uri": target_id(d) })).collect::<Vec<_>>(),
        "capabilities": { "canCompile": true },
        "dataKind": "scala",
        "data": {
            "scalaOrganization": "org.scala-lang",
            "scalaVersion": "3.3.1",
            "scalaBinaryVersion": "3",
            "platform": 1,
            "jars": [],
        },
    })
}

fn default_targets() -> Vec<Value> {
    vec![
        build_target("fixture-a", &[]),
        build_target("fixture-b", &["fixture-a"]),
        build_target("fixture-c", &[]),
        build_target("fixture-nosdb", &[]),
    ]
}

fn requested_names(params: &Value) -> Vec<String> {
    params
        .get("targets")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("uri").and_then(Value::as_str))
                .map(|u| u.strip_prefix("bsp://workspace/").unwrap_or(u).to_string())
                .collect()
        })
        .unwrap_or_default()
}

fn reply(writer: &mut UnixStream, id: Option<Value>, result: Value) {
    if let Some(id) = id {
        let _ = write_message(
            writer,
            &json!({ "jsonrpc": "2.0", "id": id, "result": result }),
        );
    }
}

fn reply_error(writer: &mut UnixStream, id: Value, code: i64, message: &str) {
    let _ = write_message(
        writer,
        &json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } }),
    );
}

fn notify(writer: &mut UnixStream, method: &str, params: Value) {
    let _ = write_message(
        writer,
        &json!({ "jsonrpc": "2.0", "method": method, "params": params }),
    );
}

/// One `build/publishDiagnostics` params for a file+target: `reset` replaces that
/// target's list for the file, and an empty `diagnostics` under `reset` clears it.
fn bsp_diagnostics(file_uri: &str, target: &str, reset: bool, diagnostics: Value) -> Value {
    json!({
        "textDocument": { "uri": file_uri },
        "buildTarget": { "uri": target_id(target) },
        "diagnostics": diagnostics,
        "reset": reset,
    })
}

fn bsp_error(message: &str, code: &str) -> Value {
    json!({
        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 4 } },
        "severity": 1,
        "code": code,
        "source": "sc",
        "message": message,
    })
}

fn serve_fake(server: Arc<FakeBuildServer>, stream: UnixStream) -> JoinHandle<()> {
    thread::spawn(move || {
        let read_half = stream.try_clone().expect("clone server stream");
        let mut reader = BufReader::new(read_half);
        let mut writer = stream;
        while let Ok(Some(msg)) = read_message(&mut reader) {
            if !server.handle(&msg, &mut writer) {
                break;
            }
        }
    })
}

// --- the model source that connects the real session to the fake --------------

type Streams = (Box<dyn Read + Send>, Box<dyn Write + Send>);

/// A `ModelSource` that `connect`s a real `BspSession` to the in-process fake
/// over the client half of a `UnixStream` pair, then goes through the SAME
/// `ready_model_from_session` assembly the live source uses (initialize + load +
/// retain the session in the real compiler). Clone shares one connection; `load`
/// consumes the streams exactly once (bootstrap calls it once).
#[derive(Clone)]
struct FakeBspModelSource {
    workspace_root: PathBuf,
    streams: Arc<Mutex<Option<Streams>>>,
    reload_flag: Arc<AtomicBool>,
    /// The production diagnostics plumbing: the session's `on_diagnostics` routes
    /// each BSP publish through this router and writes the accepted LSP publish
    /// straight to the shared output sink (the same sink the loop writes responses
    /// to), exactly as production does.
    router: Arc<Mutex<DiagnosticRouter>>,
    sink: Arc<OutputSink<Vec<u8>>>,
}

impl ModelSource for FakeBspModelSource {
    fn load(&self, _workspace_root: &Path) -> Result<LoadOutcome, String> {
        let (input, output) = self
            .streams
            .lock()
            .unwrap()
            .take()
            .ok_or_else(|| "fake BSP streams already consumed".to_string())?;
        let reload = Arc::clone(&self.reload_flag);
        let router = Arc::clone(&self.router);
        let sink = Arc::clone(&self.sink);
        let handlers = BspClientHandlers::new()
            .on_did_change_build_target(move |_| reload.store(true, Ordering::SeqCst))
            .on_diagnostics(move |params: BspPublishDiagnosticsParams| {
                if let Some(publish) = router.lock().unwrap().accept(&params) {
                    let _ = sink.publish_diagnostics(&publish);
                }
            });
        let session = BspSession::connect(
            self.workspace_root.clone(),
            input,
            output,
            handlers,
            BspSessionConfig {
                request_timeout: Duration::from_secs(20),
                shutdown_timeout: Duration::from_secs(2),
                ..BspSessionConfig::default()
            },
        );
        ready_model_from_session(session).map(LoadOutcome::Model)
    }
}

// --- harness ------------------------------------------------------------------

struct E2e {
    server: Arc<FakeBuildServer>,
    workspace_root: PathBuf,
    source: FakeBspModelSource,
    _server_thread: JoinHandle<()>,
    _tempdir: tempfile::TempDir,
}

/// Stand up the fake server on one end of a socketpair and a `ModelSource` on the
/// other. `reload_flag` is the server core's flag (fired by `buildTarget/didChange`).
fn setup(reload_flag: Arc<AtomicBool>) -> E2e {
    let tempdir = tempfile::tempdir().unwrap();
    let workspace_root = tempdir.path().to_path_buf();
    // The no-SemanticDB target's single source, on disk under the workspace.
    let nosdb_source = workspace_root.join("nosdb").join("NoSdb.scala");
    std::fs::create_dir_all(nosdb_source.parent().unwrap()).unwrap();
    std::fs::write(&nosdb_source, "class NoSdb\n").unwrap();

    let server = Arc::new(FakeBuildServer::new(nosdb_source));
    let (client, server_stream) = UnixStream::pair().unwrap();
    let server_thread = serve_fake(Arc::clone(&server), server_stream);

    let input: Box<dyn Read + Send> = Box::new(client.try_clone().unwrap());
    let output: Box<dyn Write + Send> = Box::new(client);
    let source = FakeBspModelSource {
        workspace_root: workspace_root.clone(),
        streams: Arc::new(Mutex::new(Some((input, output)))),
        reload_flag,
        router: Arc::new(Mutex::new(DiagnosticRouter::new())),
        sink: Arc::new(OutputSink::new(Vec::new())),
    };
    E2e {
        server,
        workspace_root,
        source,
        _server_thread: server_thread,
        _tempdir: tempdir,
    }
}

fn frame(body: Value) -> Vec<u8> {
    let text = serde_json::to_string(&body).unwrap();
    format!("Content-Length: {}\r\n\r\n{}", text.len(), text).into_bytes()
}

fn request(id: i64, method: &str, params: Value) -> Vec<u8> {
    frame(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
}

fn notification(method: &str, params: Value) -> Vec<u8> {
    frame(json!({ "jsonrpc": "2.0", "method": method, "params": params }))
}

fn responses(bytes: Vec<u8>) -> Vec<Value> {
    let mut reader = Cursor::new(bytes);
    let mut out = Vec::new();
    while let Some(body) = read_frame(&mut reader).unwrap() {
        out.push(serde_json::from_slice(&body).unwrap());
    }
    out
}

fn split_after_initialized(bytes: &[u8]) -> usize {
    let mut reader = Cursor::new(bytes.to_vec());
    while let Ok(Some(body)) = read_frame(&mut reader) {
        let is_initialized = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v.get("method")?.as_str().map(str::to_string))
            .as_deref()
            == Some("initialized");
        if is_initialized {
            return reader.position() as usize;
        }
    }
    bytes.len()
}

/// Drives `serve` over the framed input, pumping the async bootstrap (which talks
/// the real fake BSP) to completion between the `initialized`-split halves.
fn serve_pumped(
    core: &mut ServerCore<ls_server::CoreServices>,
    source: &FakeBspModelSource,
    input: Vec<u8>,
) -> Vec<Value> {
    let split = split_after_initialized(&input);
    for chunk in [input[..split].to_vec(), input[split..].to_vec()] {
        if chunk.is_empty() {
            continue;
        }
        let mut reader = Cursor::new(chunk);
        let bootstrap = IndexBootstrap::new(source.clone());
        // Responses (loop) and diagnostics (BSP reader thread) share one sink, just
        // like production; all frames accumulate in it across both passes.
        serve(
            &mut reader,
            source.sink.as_ref(),
            core,
            &CoreHandlers,
            bootstrap,
        )
        .unwrap();
    }
    responses(source.sink.written())
}

fn by_id(out: &[Value], id: i64) -> &Value {
    out.iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no response for id {id} in {out:?}"))
}

fn core_uri() -> String {
    path_to_uri(&fixtures_root().join("sources/a/src/pkga/Core.scala"))
}

/// initialize (with the workspace root) + initialized + `body` requests + exit.
fn session_input(root: &Path, body: Vec<Vec<u8>>) -> Vec<u8> {
    let mut input = vec![
        request(1, "initialize", json!({ "rootUri": path_to_uri(root) })),
        notification("initialized", json!({})),
    ];
    input.extend(body);
    input.push(notification("exit", json!({})));
    input.concat()
}

fn eventually(clue: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_millis(3000);
    while !cond() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(cond(), "condition not reached within 3000ms: {clue}");
}

/// The `textDocument/publishDiagnostics` notifications the loop wrote.
fn publishes(out: &[Value]) -> Vec<&Value> {
    out.iter()
        .filter(|m| m["method"] == "textDocument/publishDiagnostics")
        .collect()
}

fn execute(id: i64, command: &str) -> Vec<u8> {
    request(
        id,
        "workspace/executeCommand",
        json!({ "command": command }),
    )
}

fn did_open(uri: &str, text: &str) -> Vec<u8> {
    notification(
        "textDocument/didOpen",
        json!({ "textDocument": { "uri": uri, "languageId": "scala", "version": 1, "text": text } }),
    )
}

// --- scenarios ----------------------------------------------------------------

// The synchronous initialize advertises the capabilities, and once ready the
// bootstrap has completed the real BSP initialize/initialized handshake with the
// fake server.
#[test]
fn capabilities_and_the_real_bsp_handshake() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let input = session_input(&e2e.workspace_root, vec![]);
    let out = serve_pumped(&mut core, &e2e.source, input);

    assert!(core.state.is_ready(), "workspace did not reach ready");
    let caps = &by_id(&out, 1)["result"]["capabilities"];
    assert_eq!(caps["referencesProvider"], true, "{caps}");
    assert_eq!(
        caps["executeCommandProvider"]["commands"],
        json!([
            "scala3SemanticLs.doctor",
            "scala3SemanticLs.reindex",
            "scala3SemanticLs.compile"
        ])
    );
    assert!(
        e2e.server.initialize_received.load(Ordering::SeqCst),
        "bootstrap did not reach the fake BSP initialize"
    );
    assert!(
        e2e.server.initialized_notified.load(Ordering::SeqCst),
        "bootstrap did not send build/initialized"
    );
}

// workspace/symbol resolves a real hit over the model loaded from the fake BSP.
#[test]
fn workspace_symbol_over_the_real_bsp_model() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let input = session_input(
        &e2e.workspace_root,
        vec![request(2, "workspace/symbol", json!({ "query": "Core" }))],
    );
    let out = serve_pumped(&mut core, &e2e.source, input);

    let symbols = by_id(&out, 2)["result"].as_array().expect("symbol array");
    let core = symbols
        .iter()
        .find(|s| s["name"] == "Core")
        .unwrap_or_else(|| panic!("no Core symbol in {symbols:?}"));
    assert!(core["location"]["uri"]
        .as_str()
        .unwrap()
        .ends_with("a/src/pkga/Core.scala"));
}

// references + documentHighlight + prepareRename resolve over the real BSP model.
#[test]
fn references_highlight_and_prepare_rename_over_the_real_bsp_model() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let uri = core_uri();
    let at = json!({ "textDocument": { "uri": uri }, "position": { "line": 2, "character": 6 } });
    let input = session_input(
        &e2e.workspace_root,
        vec![
            request(
                2,
                "textDocument/references",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": 2, "character": 6 },
                    "context": { "includeDeclaration": true }
                }),
            ),
            request(3, "textDocument/documentHighlight", at.clone()),
            request(4, "textDocument/prepareRename", at),
        ],
    );
    let out = serve_pumped(&mut core, &e2e.source, input);

    assert!(
        !by_id(&out, 2)["result"].as_array().unwrap().is_empty(),
        "expected references for Core"
    );
    assert!(
        !by_id(&out, 3)["result"].as_array().unwrap().is_empty(),
        "expected highlights for Core"
    );
    assert_eq!(
        by_id(&out, 4)["result"]["start"]["line"],
        2,
        "prepareRename span: {}",
        by_id(&out, 4)
    );
}

// A source in a target compiled WITHOUT SemanticDB stays a hard error for
// references AND documentHighlight — never an empty result.
#[test]
fn a_no_semanticdb_source_is_a_hard_error() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let uri = path_to_uri(&e2e.workspace_root.join("nosdb").join("NoSdb.scala"));
    let at = json!({ "textDocument": { "uri": uri }, "position": { "line": 0, "character": 6 } });
    let input = session_input(
        &e2e.workspace_root,
        vec![
            request(2, "textDocument/references", at.clone()),
            request(3, "textDocument/documentHighlight", at),
        ],
    );
    let out = serve_pumped(&mut core, &e2e.source, input);

    for id in [2, 3] {
        let r = by_id(&out, id);
        assert!(r.get("result").is_none(), "id {id} must be an error: {r}");
        assert!(
            r["error"]["message"]
                .as_str()
                .unwrap()
                .contains("has no SemanticDB output"),
            "id {id}: {r}"
        );
    }
}

// The doctor over the real BSP model flags the SemanticDB-less target in the BSP
// coverage error line (it is counted and named, not silently dropped).
#[test]
fn doctor_flags_the_semanticdb_less_target() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let input = session_input(
        &e2e.workspace_root,
        vec![request(
            2,
            "workspace/executeCommand",
            json!({ "command": "scala3SemanticLs.doctor" }),
        )],
    );
    let out = serve_pumped(&mut core, &e2e.source, input);

    let text = by_id(&out, 2)["result"].as_str().expect("doctor text");
    assert!(
        text.contains("fixture-nosdb"),
        "doctor must name the target: {text}"
    );
    assert!(
        text.contains("without SemanticDB"),
        "doctor must flag the coverage error: {text}"
    );
}

// Dropping the ready services (server teardown) shuts the retained BSP session
// down: the fake server sees build/shutdown.
#[test]
fn dropping_the_server_tears_down_the_bsp_session() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let input = session_input(&e2e.workspace_root, vec![]);
    let _ = serve_pumped(&mut core, &e2e.source, input);
    assert!(core.state.is_ready());
    assert!(
        !e2e.server.shutdown_requested.load(Ordering::SeqCst),
        "session shut down before teardown"
    );

    // Tear the ready bundle down; the compiler's Drop shuts the session down.
    drop(core);
    eventually("BSP session shut down on teardown", || {
        e2e.server.shutdown_requested.load(Ordering::SeqCst)
    });
}

// A build-server `build/publishDiagnostics` arriving during a compile is routed
// through the diagnostics router and forwarded to the client as an LSP
// `textDocument/publishDiagnostics`.
#[test]
fn compile_error_diagnostics_are_published_to_the_client() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let uri = core_uri();
    e2e.server.script_compile(CompileScript {
        status: 1,
        diagnostics: vec![bsp_diagnostics(
            &uri,
            "fixture-a",
            true,
            json!([bsp_error("value unused", "unused-value")]),
        )],
        reload_to: None,
    });
    let input = session_input(
        &e2e.workspace_root,
        vec![execute(2, "scala3SemanticLs.compile")],
    );
    let out = serve_pumped(&mut core, &e2e.source, input);

    let published = publishes(&out);
    let core_publish = published
        .iter()
        .find(|p| p["params"]["uri"] == uri)
        .unwrap_or_else(|| panic!("no publishDiagnostics for Core.scala in {out:?}"));
    let diagnostics = core_publish["params"]["diagnostics"].as_array().unwrap();
    assert_eq!(diagnostics.len(), 1, "{core_publish}");
    assert_eq!(diagnostics[0]["message"], "value unused");
    assert_eq!(diagnostics[0]["severity"], 1);
}

// After a non-empty publish, a clean reset (empty diagnostics for that target)
// forwards exactly one clear for the file.
#[test]
fn a_clean_reset_clears_the_previously_published_diagnostics() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let uri = core_uri();
    e2e.server.script_compile(CompileScript {
        status: 1,
        diagnostics: vec![bsp_diagnostics(
            &uri,
            "fixture-a",
            true,
            json!([bsp_error("boom", "unused-local")]),
        )],
        reload_to: None,
    });
    e2e.server.script_compile(CompileScript {
        status: 1,
        diagnostics: vec![bsp_diagnostics(&uri, "fixture-a", true, json!([]))],
        reload_to: None,
    });
    let input = session_input(
        &e2e.workspace_root,
        vec![
            execute(2, "scala3SemanticLs.compile"),
            execute(3, "scala3SemanticLs.compile"),
        ],
    );
    let out = serve_pumped(&mut core, &e2e.source, input);

    let core_publishes: Vec<_> = publishes(&out)
        .into_iter()
        .filter(|p| p["params"]["uri"] == uri)
        .collect();
    assert!(
        core_publishes.len() >= 2,
        "expected a publish then a clear: {out:?}"
    );
    assert!(
        core_publishes.last().unwrap()["params"]["diagnostics"]
            .as_array()
            .unwrap()
            .is_empty(),
        "the final publish must clear the file: {core_publishes:?}"
    );
}

// A clean reset for a file that was never published non-empty is suppressed: no
// LSP publish is emitted (clear-once semantics).
#[test]
fn a_never_published_clean_reset_is_suppressed() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let fresh = path_to_uri(&fixtures_root().join("sources/b/src/pkgb/UseB.scala"));
    e2e.server.script_compile(CompileScript {
        status: 1,
        diagnostics: vec![bsp_diagnostics(&fresh, "fixture-b", true, json!([]))],
        reload_to: None,
    });
    let input = session_input(
        &e2e.workspace_root,
        vec![execute(2, "scala3SemanticLs.compile")],
    );
    let out = serve_pumped(&mut core, &e2e.source, input);

    assert!(
        publishes(&out).iter().all(|p| p["params"]["uri"] != fresh),
        "an empty reset for a never-published file must not publish: {out:?}"
    );
}

// `textDocument/rename` runs the fresh-required ladder over the RETAINED BSP
// session: it compiles through the fake server and returns a cross-file
// `WorkspaceEdit`.
#[test]
fn rename_over_the_retained_bsp_compiler_edits_and_compiles() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let uri = path_to_uri(&fixtures_root().join("sources/a/src/pkga/Item.scala"));
    let input = session_input(
        &e2e.workspace_root,
        vec![request(
            2,
            "textDocument/rename",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": 2, "character": 11 },
                "newName": "Renamed",
            }),
        )],
    );
    let out = serve_pumped(&mut core, &e2e.source, input);

    let result = &by_id(&out, 2)["result"];
    let changes = result["changes"]
        .as_object()
        .unwrap_or_else(|| panic!("rename returned no WorkspaceEdit: {}", by_id(&out, 2)));
    assert!(!changes.is_empty(), "rename produced no edits: {result}");
    assert!(
        changes
            .values()
            .flat_map(|edits| edits.as_array().unwrap())
            .any(|edit| edit["newText"] == "Renamed"),
        "no edit renames to the new name: {result}"
    );
    assert!(
        !e2e.server.compiled_targets.lock().unwrap().is_empty(),
        "rename must compile over the retained BSP session"
    );
}

// A failing `buildTarget/compile` fails the rename with a typed `CompileFailed`
// error, over the retained session.
#[test]
fn rename_surfaces_a_bsp_compile_failure() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let uri = path_to_uri(&fixtures_root().join("sources/a/src/pkga/Item.scala"));
    // The next compile the rename issues fails (statusCode 2).
    e2e.server.script_compile(CompileScript {
        status: 2,
        diagnostics: Vec::new(),
        reload_to: None,
    });
    let input = session_input(
        &e2e.workspace_root,
        vec![request(
            2,
            "textDocument/rename",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": 2, "character": 11 },
                "newName": "Renamed",
            }),
        )],
    );
    let out = serve_pumped(&mut core, &e2e.source, input);

    let response = by_id(&out, 2);
    assert!(
        response.get("result").is_none(),
        "a failing compile must fail the rename: {response}"
    );
    assert!(
        response["error"]["message"]
            .as_str()
            .unwrap()
            .contains("buildTarget/compile failed"),
        "expected a compile-failure message: {response}"
    );
}

// A build server `buildTarget/didChange` reloads the model over the retained
// session: a symbol in a target absent from the initial model becomes searchable
// after the reload refetches the enlarged target set.
#[test]
fn a_build_target_did_change_reloads_the_model_over_the_session() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    // Start without fixture-c, so its `UseC` object is not yet indexed.
    e2e.server.set_targets(vec![
        build_target("fixture-a", &[]),
        build_target("fixture-b", &["fixture-a"]),
        build_target("fixture-nosdb", &[]),
    ]);
    // The compile fires a didChange and swaps in the full target set (adds
    // fixture-c) so the model refetch observes it.
    e2e.server.script_compile(CompileScript {
        status: 1,
        diagnostics: Vec::new(),
        reload_to: Some(default_targets()),
    });
    let input = session_input(
        &e2e.workspace_root,
        vec![
            request(2, "workspace/symbol", json!({ "query": "UseC" })),
            execute(3, "scala3SemanticLs.compile"),
            request(4, "workspace/symbol", json!({ "query": "UseC" })),
        ],
    );
    let out = serve_pumped(&mut core, &e2e.source, input);

    // Fuzzy search may surface near-name hits (e.g. `UseCounter`); assert on the
    // exact `UseC` symbol, which lives only in fixture-c.
    assert!(
        !by_id(&out, 2)["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "UseC"),
        "UseC must be absent before the reload: {}",
        by_id(&out, 2)
    );
    assert!(
        by_id(&out, 4)["result"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "UseC"),
        "UseC must be searchable after the reload refetches fixture-c: {}",
        by_id(&out, 4)
    );
}

// An unsaved top-level declaration the index has never seen is PC-only over the
// wire: `workspace/symbol` surfaces it with the unsaved-buffer container, and
// references/rename at it are the hard PC-only rejection (JVM-free — the overlay's
// dirty-buffer scan answers before any live PC query).
#[test]
fn an_unsaved_top_level_symbol_is_pc_only_over_the_wire() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let uri = core_uri();
    // A dirty buffer (differs from disk) declaring a new top-level object.
    let text = "package pkga\n\nobject GhostWidget:\n  def z = 1\n";
    let at = json!({ "textDocument": { "uri": uri }, "position": { "line": 2, "character": 10 } });
    let input = session_input(
        &e2e.workspace_root,
        vec![
            did_open(&uri, text),
            request(2, "workspace/symbol", json!({ "query": "GhostWidget" })),
            request(3, "textDocument/references", at.clone()),
            request(
                4,
                "textDocument/rename",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": 2, "character": 10 },
                    "newName": "Renamed",
                }),
            ),
        ],
    );
    let out = serve_pumped(&mut core, &e2e.source, input);

    let symbols = by_id(&out, 2)["result"].as_array().expect("symbol array");
    let ghost = symbols
        .iter()
        .find(|s| s["name"] == "GhostWidget")
        .unwrap_or_else(|| panic!("GhostWidget not surfaced by workspace/symbol: {symbols:?}"));
    assert_eq!(ghost["containerName"], "unsaved buffer (PC-only)");

    for id in [3, 4] {
        let response = by_id(&out, id);
        assert!(
            response.get("result").is_none(),
            "id {id} must be the hard PC-only rejection: {response}"
        );
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("PC-only plugin"),
            "id {id}: {response}"
        );
    }
}

// LSP `shutdown` then `exit` returns the shutdown response, ends the serve loop,
// and — as the ready services drop — tears the retained BSP session down
// (`build/shutdown` + `build/exit` reach the fake server).
#[test]
fn lsp_shutdown_then_exit_tears_down_the_bsp_session() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let input = session_input(&e2e.workspace_root, vec![request(2, "shutdown", json!({}))]);
    let out = serve_pumped(&mut core, &e2e.source, input);

    assert_eq!(
        by_id(&out, 2)["result"],
        Value::Null,
        "shutdown returns null"
    );
    assert!(core.shutting_down, "shutdown set the shutting-down state");

    drop(core);
    eventually("BSP session shut down + exited on teardown", || {
        e2e.server.shutdown_requested.load(Ordering::SeqCst)
            && e2e.server.exit_received.load(Ordering::SeqCst)
    });
}

// Env-gated (like `live_pc.rs`): with a real JVM + PC host jar + a target
// classpath, completion over an open buffer routes through the real island PC.
// Skips cleanly when the JVM environment is absent.
#[test]
fn pc_completion_over_the_fake_bsp_model() {
    if std::env::var_os("LS_LIBJVM").is_none()
        || std::env::var_os("PC_HOST_AGENT_JAR").is_none()
        || std::env::var_os("LS_PC_TARGET_CLASSPATH").is_none()
    {
        eprintln!(
            "fake_bsp_e2e: skipping PC completion — set LS_LIBJVM + PC_HOST_AGENT_JAR + \
             LS_PC_TARGET_CLASSPATH to run it"
        );
        return;
    }
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    // Complete a member of the fixture `Core` in an open buffer under fixture-a.
    let uri = core_uri();
    let text = "package pkga\n\nobject Probe:\n  val c = Core.make(\"x\").\n";
    let input = session_input(
        &e2e.workspace_root,
        vec![
            did_open(&uri, text),
            request(
                2,
                "textDocument/completion",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": 3, "character": 24 },
                }),
            ),
        ],
    );
    let out = serve_pumped(&mut core, &e2e.source, input);

    let completion = &by_id(&out, 2)["result"];
    let items = completion["items"]
        .as_array()
        .unwrap_or_else(|| panic!("completion returned no list: {completion}"));
    assert!(
        !items.is_empty(),
        "the real island PC returned no completions: {completion}"
    );
}
