//! The in-process fake BSP build server (the one copy of what
//! `fake_bsp_e2e.rs` used to host inline). A real `BspSession` speaks JSON-RPC
//! to it over a `UnixStream`, so BSP handshake, model load, compile, and
//! diagnostics forwarding all run through production client code. The fake
//! advertises the committed fixture corpus ([`crate::fixtures`]); its reaction
//! to `buildTarget/compile` (status, published diagnostics, a
//! `buildTarget/didChange` reload) is scriptable per test.

use std::collections::VecDeque;
use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{json, Value};

use ls_bsp::protocol::PublishDiagnosticsParams as BspPublishDiagnosticsParams;
use ls_bsp::uri::path_to_uri;
use ls_bsp::wire::{read_message, write_message};
use ls_bsp::{BspClientHandlers, BspSession, BspSessionConfig};
use ls_server::{ready_model_from_session, DiagnosticRouter, LoadOutcome, ModelSource, OutputSink};

use crate::fixtures::{default_targets, fixtures_root, sources_root, target_id};

/// How the fake reacts to the next `buildTarget/compile`: which status to
/// return, which `build/publishDiagnostics` notifications to emit first, and
/// whether to fire a `buildTarget/didChange` (swapping the advertised targets so
/// a subsequent model refetch sees the reloaded set).
#[derive(Clone)]
pub struct CompileScript {
    pub status: i64,
    pub diagnostics: Vec<Value>,
    pub reload_to: Option<Vec<Value>>,
}

impl CompileScript {
    pub fn ok() -> CompileScript {
        CompileScript {
            status: 1,
            diagnostics: Vec::new(),
            reload_to: None,
        }
    }
}

/// A fake BSP server over a `UnixStream`, describing the fixture corpus. The
/// three indexable targets carry `-Xsemanticdb` pointing at the committed
/// `out-*` targetroots; `fixture-nosdb` carries no SemanticDB flag.
pub struct FakeBuildServer {
    sources_root: PathBuf,
    nosdb_source: PathBuf,
    pub initialize_received: AtomicBool,
    pub initialized_notified: AtomicBool,
    pub shutdown_requested: AtomicBool,
    pub exit_received: AtomicBool,
    /// The build targets advertised for `workspace/buildTargets`; a `didChange`
    /// reload swaps this so a refetch observes the new set.
    current_targets: Mutex<Vec<Value>>,
    /// Scripts consumed one per `buildTarget/compile`; empties to a plain success.
    compile_scripts: Mutex<VecDeque<CompileScript>>,
    /// The target ids seen across all compile requests.
    compiled_targets: Mutex<Vec<String>>,
}

impl FakeBuildServer {
    pub fn new(nosdb_source: PathBuf) -> FakeBuildServer {
        FakeBuildServer {
            sources_root: sources_root(),
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

    /// Advertise a restricted initial target set (for reload scenarios).
    pub fn set_targets(&self, targets: Vec<Value>) {
        *self.current_targets.lock().unwrap() = targets;
    }

    /// Queue the fake's reaction to the next compile.
    pub fn script_compile(&self, script: CompileScript) {
        self.compile_scripts.lock().unwrap().push_back(script);
    }

    pub fn compiled_targets(&self) -> Vec<String> {
        self.compiled_targets.lock().unwrap().clone()
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

/// One `build/publishDiagnostics` params for a file+target: `reset` replaces
/// that target's list for the file; empty `diagnostics` under `reset` clears it.
pub fn bsp_diagnostics(file_uri: &str, target: &str, reset: bool, diagnostics: Value) -> Value {
    json!({
        "textDocument": { "uri": file_uri },
        "buildTarget": { "uri": target_id(target) },
        "diagnostics": diagnostics,
        "reset": reset,
    })
}

pub fn bsp_error(message: &str, code: &str) -> Value {
    json!({
        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 4 } },
        "severity": 1,
        "code": code,
        "source": "sc",
        "message": message,
    })
}

pub fn serve_fake(server: Arc<FakeBuildServer>, stream: UnixStream) -> JoinHandle<()> {
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

type Streams = (Box<dyn Read + Send>, Box<dyn Write + Send>);

/// A `ModelSource` that `connect`s a real `BspSession` to the in-process fake
/// over the client half of a `UnixStream` pair, then goes through the SAME
/// `ready_model_from_session` assembly the live source uses. Clone shares one
/// connection; `load` consumes the streams exactly once.
pub struct FakeBspModelSource<W: Write + Send + Sync + 'static> {
    workspace_root: PathBuf,
    streams: Arc<Mutex<Option<Streams>>>,
    reload_flag: Arc<AtomicBool>,
    /// Production diagnostics plumbing: the session's `on_diagnostics` routes
    /// each BSP publish through this router straight to the shared output sink.
    router: Arc<Mutex<DiagnosticRouter>>,
    pub sink: Arc<OutputSink<W>>,
}

impl<W: Write + Send + Sync + 'static> Clone for FakeBspModelSource<W> {
    fn clone(&self) -> Self {
        FakeBspModelSource {
            workspace_root: self.workspace_root.clone(),
            streams: Arc::clone(&self.streams),
            reload_flag: Arc::clone(&self.reload_flag),
            router: Arc::clone(&self.router),
            sink: Arc::clone(&self.sink),
        }
    }
}

impl<W: Write + Send + Sync + 'static> ModelSource for FakeBspModelSource<W> {
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

/// A running fake plus the model source wired to it, over a fresh temp
/// workspace (holding the on-disk `nosdb` source the corpus references).
pub struct FakeBsp {
    pub server: Arc<FakeBuildServer>,
    pub workspace_root: PathBuf,
    _server_thread: JoinHandle<()>,
    _tempdir: tempfile::TempDir,
}

impl FakeBsp {
    /// Stand the fake up on one end of a socketpair and hand back the
    /// `ModelSource` on the other. `reload_flag` is the server core's flag
    /// (fired by `buildTarget/didChange`); `sink` is the shared output sink the
    /// serve loop writes to (diagnostics reach it from the session reader
    /// thread, exactly as production does).
    pub fn start<W: Write + Send + Sync + 'static>(
        reload_flag: Arc<AtomicBool>,
        sink: Arc<OutputSink<W>>,
    ) -> (FakeBsp, FakeBspModelSource<W>) {
        let tempdir = tempfile::tempdir().unwrap();
        let workspace_root = tempdir.path().to_path_buf();
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
            sink,
        };
        (
            FakeBsp {
                server,
                workspace_root,
                _server_thread: server_thread,
                _tempdir: tempdir,
            },
            source,
        )
    }
}
