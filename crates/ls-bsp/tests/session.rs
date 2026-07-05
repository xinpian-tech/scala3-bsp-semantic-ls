//! End-to-end tests against an in-process fake BSP server wired over a
//! `UnixStream` socketpair with the real JSON-RPC framing on both ends — port
//! of the Scala `BspSessionTest`.

use std::collections::BTreeSet;
use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use ls_bsp::protocol::{
    DidChangeBuildTarget, LogMessageParams, PublishDiagnosticsParams, ShowMessageParams,
};
use ls_bsp::uri::{path_to_uri, uri_to_path};
use ls_bsp::wire::{read_message, write_message};
use ls_bsp::{
    BspClientHandlers, BspCompileOutcome, BspError, BspProjectModel, BspSession, BspSessionConfig,
    ProjectModelLoader,
};
use ls_index_model::LsError;

fn id_of(name: &str) -> String {
    format!("bsp://workspace/{name}")
}

fn broken_id() -> String {
    id_of("broken")
}

// --- fake in-process BSP server ---

struct FakeBuildServer {
    workspace_root: PathBuf,
    a_source_dir: PathBuf,
    b_source_file: PathBuf,
    c_source_file: PathBuf,
    semanticdb_override: PathBuf,
    advertise_inverse_sources: bool,
    advertise_dependency_sources: bool,
    advertise_output_paths: bool,
    initialize_received: AtomicBool,
    initialized_notified: AtomicBool,
    shutdown_requested: AtomicBool,
    exit_received: AtomicBool,
    inverse_sources_calls: AtomicUsize,
    workspace_build_targets_calls: AtomicUsize,
    dependency_sources_calls: AtomicUsize,
    output_paths_calls: AtomicUsize,
}

impl FakeBuildServer {
    fn class_directory_of(&self, name: &str) -> PathBuf {
        self.workspace_root.join("out").join(name).join("classes")
    }

    /// Returns false to stop serving (on build/exit).
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
            "workspace/buildTargets" => {
                self.workspace_build_targets_calls
                    .fetch_add(1, Ordering::SeqCst);
                reply(writer, id, self.workspace_build_targets());
            }
            "buildTarget/sources" => reply(writer, id, self.sources(&params)),
            "buildTarget/scalacOptions" => reply(writer, id, self.scalac_options(&params)),
            "buildTarget/compile" => self.compile(writer, id, &params),
            "buildTarget/inverseSources" => self.inverse_sources(writer, id, &params),
            "buildTarget/dependencySources" => self.dependency_sources(writer, id, &params),
            "buildTarget/outputPaths" => self.output_paths(writer, id, &params),
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
            "capabilities": {
                "compileProvider": {"languageIds": ["scala"]},
                "inverseSourcesProvider": self.advertise_inverse_sources,
                "dependencySourcesProvider": self.advertise_dependency_sources,
                "outputPathsProvider": self.advertise_output_paths,
            }
        })
    }

    fn workspace_build_targets(&self) -> Value {
        // a <- b <- c dependency chain, plus a Scala 2 and a Java-only target
        // that the loader must filter out.
        let targets = vec![
            build_target("a", &["scala"], &[], Some("3.8.4")),
            build_target("b", &["scala"], &["a"], Some("3.8.4")),
            build_target("c", &["scala"], &["b"], Some("3.8.4")),
            build_target("scala2", &["scala"], &[], Some("2.13.16")),
            build_target("java-only", &["java"], &[], None),
        ];
        json!({ "targets": targets })
    }

    fn sources(&self, params: &Value) -> Value {
        let items: Vec<Value> = requested_names(params)
            .iter()
            .map(|name| self.source_item(name))
            .collect();
        json!({ "items": items })
    }

    fn source_item(&self, name: &str) -> Value {
        match name {
            "a" => json!({
                "target": {"uri": id_of("a")},
                "sources": [{"uri": path_to_uri(&self.a_source_dir), "kind": 2, "generated": false}],
            }),
            "b" => json!({
                "target": {"uri": id_of("b")},
                "sources": [{"uri": path_to_uri(&self.b_source_file), "kind": 1, "generated": false}],
            }),
            "c" => json!({
                "target": {"uri": id_of("c")},
                "sources": [{"uri": path_to_uri(&self.c_source_file), "kind": 1, "generated": false}],
            }),
            other => json!({"target": {"uri": id_of(other)}, "sources": []}),
        }
    }

    fn scalac_options(&self, params: &Value) -> Value {
        let items: Vec<Value> = requested_names(params)
            .iter()
            .map(|name| self.scalac_option_item(name))
            .collect();
        json!({ "items": items })
    }

    fn scalac_option_item(&self, name: &str) -> Value {
        let options: Vec<String> = match name {
            // -Xsemanticdb + colon-form -semanticdb-target override + two-token
            // -sourceroot form.
            "a" => vec![
                "-deprecation".to_string(),
                "-Xsemanticdb".to_string(),
                format!(
                    "-semanticdb-target:{}",
                    self.semanticdb_override.to_string_lossy()
                ),
                "-sourceroot".to_string(),
                self.workspace_root.to_string_lossy().into_owned(),
            ],
            // plain -Ysemanticdb: targetroot = classDirectory.
            "b" => vec!["-Ysemanticdb".to_string()],
            // no SemanticDB flags at all.
            _ => vec!["-deprecation".to_string()],
        };
        json!({
            "target": {"uri": id_of(name)},
            "options": options,
            "classpath": [],
            "classDirectory": path_to_uri(&self.class_directory_of(name)),
        })
    }

    fn compile(&self, writer: &mut UnixStream, id: Option<Value>, params: &Value) {
        let origin = params
            .get("originId")
            .and_then(Value::as_str)
            .map(str::to_string);
        let requested = requested_uris(params);
        if requested.iter().any(|u| u == &broken_id()) {
            reply(writer, id, json!({"statusCode": 2, "originId": origin}));
            return;
        }
        let first_target = params
            .get("targets")
            .and_then(|t| t.get(0))
            .cloned()
            .unwrap_or(Value::Null);
        // Notifications flow to the client while the request is in flight.
        notify(
            writer,
            "build/publishDiagnostics",
            json!({
                "textDocument": {"uri": path_to_uri(&self.b_source_file)},
                "buildTarget": first_target.clone(),
                "diagnostics": [{
                    "range": {"start": {"line": 0, "character": 1}, "end": {"line": 0, "character": 5}},
                    "severity": 2,
                    "message": "value unused in fake target",
                }],
                "reset": true,
                "originId": origin,
            }),
        );
        notify(
            writer,
            "build/logMessage",
            json!({"type": 3, "message": "fake compile log"}),
        );
        notify(
            writer,
            "build/showMessage",
            json!({"type": 2, "message": "fake compile show"}),
        );
        notify(
            writer,
            "buildTarget/didChange",
            json!({"changes": [{"target": first_target}]}),
        );
        reply(writer, id, json!({"statusCode": 1, "originId": origin}));
    }

    fn inverse_sources(&self, writer: &mut UnixStream, id: Option<Value>, params: &Value) {
        self.inverse_sources_calls.fetch_add(1, Ordering::SeqCst);
        if !self.advertise_inverse_sources {
            if let Some(id) = id {
                reply_error(
                    writer,
                    id,
                    -32601,
                    "inverseSources capability not advertised",
                );
            }
            return;
        }
        let uri = params
            .get("textDocument")
            .and_then(|d| d.get("uri"))
            .and_then(Value::as_str)
            .unwrap_or("");
        let owner = match uri_to_path(uri).ok() {
            Some(p) if p.starts_with(&self.a_source_dir) => Some("a"),
            Some(p) if p == self.b_source_file => Some("b"),
            Some(p) if p == self.c_source_file => Some("c"),
            _ => None,
        };
        let targets: Vec<Value> = owner
            .into_iter()
            .map(|n| json!({"uri": id_of(n)}))
            .collect();
        reply(writer, id, json!({ "targets": targets }));
    }

    fn dependency_sources(&self, writer: &mut UnixStream, id: Option<Value>, params: &Value) {
        if !self.advertise_dependency_sources {
            if let Some(id) = id {
                reply_error(writer, id, -32601, "buildTarget/dependencySources");
            }
            return;
        }
        self.dependency_sources_calls.fetch_add(1, Ordering::SeqCst);
        let items: Vec<Value> = requested_names(params)
            .iter()
            .map(|n| json!({"target": {"uri": id_of(n)}, "sources": [format!("file:///dep/{n}-sources.jar")]}))
            .collect();
        reply(writer, id, json!({ "items": items }));
    }

    fn output_paths(&self, writer: &mut UnixStream, id: Option<Value>, params: &Value) {
        if !self.advertise_output_paths {
            if let Some(id) = id {
                reply_error(writer, id, -32601, "buildTarget/outputPaths");
            }
            return;
        }
        self.output_paths_calls.fetch_add(1, Ordering::SeqCst);
        let items: Vec<Value> = requested_names(params)
            .iter()
            .map(|n| json!({"target": {"uri": id_of(n)}, "outputPaths": [{"uri": format!("file:///out/{n}"), "kind": 2}]}))
            .collect();
        reply(writer, id, json!({ "items": items }));
    }
}

fn build_target(name: &str, langs: &[&str], deps: &[&str], scala_version: Option<&str>) -> Value {
    let mut target = json!({
        "id": {"uri": id_of(name)},
        "displayName": name,
        "tags": [],
        "languageIds": langs,
        "dependencies": deps.iter().map(|d| json!({"uri": id_of(d)})).collect::<Vec<_>>(),
        "capabilities": {"canCompile": true},
    });
    if let Some(version) = scala_version {
        let binary = if version.starts_with('3') {
            "3"
        } else {
            "2.13"
        };
        target["dataKind"] = json!("scala");
        target["data"] = json!({
            "scalaOrganization": "org.scala-lang",
            "scalaVersion": version,
            "scalaBinaryVersion": binary,
            "platform": 1,
            "jars": [],
        });
    }
    target
}

fn requested_uris(params: &Value) -> Vec<String> {
    params
        .get("targets")
        .and_then(Value::as_array)
        .map(|arr| {
            arr.iter()
                .filter_map(|t| t.get("uri").and_then(Value::as_str))
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn requested_names(params: &Value) -> Vec<String> {
    requested_uris(params)
        .iter()
        .map(|u| u.strip_prefix("bsp://workspace/").unwrap_or(u).to_string())
        .collect()
}

fn reply(writer: &mut UnixStream, id: Option<Value>, result: Value) {
    if let Some(id) = id {
        let _ = write_message(
            writer,
            &json!({"jsonrpc": "2.0", "id": id, "result": result}),
        );
    }
}

fn reply_error(writer: &mut UnixStream, id: Value, code: i64, message: &str) {
    let _ = write_message(
        writer,
        &json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}}),
    );
}

fn notify(writer: &mut UnixStream, method: &str, params: Value) {
    let _ = write_message(
        writer,
        &json!({"jsonrpc": "2.0", "method": method, "params": params}),
    );
}

fn serve(server: Arc<FakeBuildServer>, stream: UnixStream) -> JoinHandle<()> {
    thread::spawn(move || {
        let read_half = stream.try_clone().expect("clone server stream");
        let mut reader = BufReader::new(read_half);
        let mut writer = stream;
        // Stops on a clean EOF / read error, or when a handler returns false
        // (build/exit).
        while let Ok(Some(msg)) = read_message(&mut reader) {
            if !server.handle(&msg, &mut writer) {
                break;
            }
        }
    })
}

// --- fixture ---

struct Fixture {
    workspace_root: PathBuf,
    server: Arc<FakeBuildServer>,
    session: BspSession,
    a_source_dir: PathBuf,
    b_source_file: PathBuf,
    c_source_file: PathBuf,
    diagnostics: Arc<Mutex<Vec<PublishDiagnosticsParams>>>,
    logs: Arc<Mutex<Vec<LogMessageParams>>>,
    shows: Arc<Mutex<Vec<ShowMessageParams>>>,
    did_changes: Arc<Mutex<Vec<DidChangeBuildTarget>>>,
    _tempdir: tempfile::TempDir,
    _server_thread: JoinHandle<()>,
}

impl Fixture {
    fn load_model(&self) -> BspProjectModel {
        ProjectModelLoader::load(&self.session).expect("load project model")
    }
}

fn eventually(clue: &str, cond: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_millis(3000);
    while !cond() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(10));
    }
    assert!(cond(), "condition not reached within 3000ms: {clue}");
}

fn with_fixture(
    advertise_inverse_sources: bool,
    advertise_dependency_sources: bool,
    advertise_output_paths: bool,
    body: impl FnOnce(&Fixture),
) {
    let tempdir = tempfile::tempdir().unwrap();
    let workspace_root = tempdir.path().to_path_buf();

    // Target a has a source *directory* (with a nested subdir and a java file
    // that must be ignored); b and c have single files.
    let a_source_dir = workspace_root.join("a").join("src");
    std::fs::create_dir_all(a_source_dir.join("nested")).unwrap();
    std::fs::write(a_source_dir.join("A1.scala"), "class A1\n").unwrap();
    std::fs::write(a_source_dir.join("A2.scala"), "class A2\n").unwrap();
    std::fs::write(a_source_dir.join("nested").join("A3.scala"), "class A3\n").unwrap();
    std::fs::write(a_source_dir.join("Ignored.java"), "class Ignored {}\n").unwrap();
    let b_source_file = workspace_root.join("b").join("src").join("B.scala");
    std::fs::create_dir_all(b_source_file.parent().unwrap()).unwrap();
    std::fs::write(&b_source_file, "class B\n").unwrap();
    let c_source_file = workspace_root.join("c").join("src").join("C.scala");
    std::fs::create_dir_all(c_source_file.parent().unwrap()).unwrap();
    std::fs::write(&c_source_file, "class C\n").unwrap();
    let semanticdb_override = workspace_root.join("out").join("a").join("semanticdb");

    let server = Arc::new(FakeBuildServer {
        workspace_root: workspace_root.clone(),
        a_source_dir: a_source_dir.clone(),
        b_source_file: b_source_file.clone(),
        c_source_file: c_source_file.clone(),
        semanticdb_override,
        advertise_inverse_sources,
        advertise_dependency_sources,
        advertise_output_paths,
        initialize_received: AtomicBool::new(false),
        initialized_notified: AtomicBool::new(false),
        shutdown_requested: AtomicBool::new(false),
        exit_received: AtomicBool::new(false),
        inverse_sources_calls: AtomicUsize::new(0),
        workspace_build_targets_calls: AtomicUsize::new(0),
        dependency_sources_calls: AtomicUsize::new(0),
        output_paths_calls: AtomicUsize::new(0),
    });

    let (client_stream, server_stream) = UnixStream::pair().unwrap();
    let server_thread = serve(Arc::clone(&server), server_stream);

    let diagnostics = Arc::new(Mutex::new(Vec::new()));
    let logs = Arc::new(Mutex::new(Vec::new()));
    let shows = Arc::new(Mutex::new(Vec::new()));
    let did_changes = Arc::new(Mutex::new(Vec::new()));
    let handlers = BspClientHandlers::new()
        .on_diagnostics({
            let q = Arc::clone(&diagnostics);
            move |d| q.lock().unwrap().push(d)
        })
        .on_log_message({
            let q = Arc::clone(&logs);
            move |m| q.lock().unwrap().push(m)
        })
        .on_show_message({
            let q = Arc::clone(&shows);
            move |m| q.lock().unwrap().push(m)
        })
        .on_did_change_build_target({
            let q = Arc::clone(&did_changes);
            move |c| q.lock().unwrap().push(c)
        });

    let input: Box<dyn Read + Send> = Box::new(client_stream.try_clone().unwrap());
    let output: Box<dyn Write + Send> = Box::new(client_stream);
    let session = BspSession::connect(
        workspace_root.clone(),
        input,
        output,
        handlers,
        BspSessionConfig {
            request_timeout: Duration::from_secs(10),
            shutdown_timeout: Duration::from_secs(2),
            ..BspSessionConfig::default()
        },
    );
    session.initialize().unwrap();

    let fixture = Fixture {
        workspace_root,
        server,
        session,
        a_source_dir,
        b_source_file,
        c_source_file,
        diagnostics,
        logs,
        shows,
        did_changes,
        _tempdir: tempdir,
        _server_thread: server_thread,
    };
    body(&fixture);
    fixture.session.shutdown();
}

// --- tests ---

#[test]
fn initialize_handshake_exposes_capabilities_and_notifies_the_server() {
    with_fixture(true, false, false, |fx| {
        assert!(fx.server.initialize_received.load(Ordering::SeqCst));
        eventually("build/initialized received", || {
            fx.server.initialized_notified.load(Ordering::SeqCst)
        });
        let caps = fx.session.server_capabilities().expect("capabilities");
        assert_eq!(caps.inverse_sources_provider, Some(true));
        assert_eq!(
            caps.compile_provider.map(|c| c.language_ids),
            Some(vec!["scala".to_string()])
        );
    });
}

#[test]
fn project_model_keeps_only_scala3_targets_sorted() {
    with_fixture(true, false, false, |fx| {
        let model = fx.load_model();
        assert_eq!(
            model
                .targets
                .iter()
                .map(|t| t.bsp_id.clone())
                .collect::<Vec<_>>(),
            vec![id_of("a"), id_of("b"), id_of("c")]
        );
        let a = model.target_for(&id_of("a")).unwrap();
        assert_eq!(a.display_name, "a");
        assert_eq!(a.scala_version, "3.8.4");
        assert_eq!(
            a.class_directory,
            fx.workspace_root.join("out").join("a").join("classes")
        );
        assert_eq!(a.direct_deps, Vec::<String>::new());
        assert_eq!(
            model.target_for(&id_of("b")).unwrap().direct_deps,
            vec![id_of("a")]
        );
        assert_eq!(
            model.target_for(&id_of("c")).unwrap().direct_deps,
            vec![id_of("b")]
        );
    });
}

#[test]
fn semanticdb_config_override_class_directory_default_disabled() {
    with_fixture(true, false, false, |fx| {
        let model = fx.load_model();
        let a = model.target_for(&id_of("a")).unwrap();
        assert_eq!(
            a.semanticdb_root,
            Some(fx.workspace_root.join("out").join("a").join("semanticdb"))
        );
        assert_eq!(a.sourceroot, Some(fx.workspace_root.clone()));
        let b = model.target_for(&id_of("b")).unwrap();
        assert_eq!(
            b.semanticdb_root,
            Some(fx.workspace_root.join("out").join("b").join("classes"))
        );
        assert_eq!(b.sourceroot, Some(fx.workspace_root.clone()));
        assert_eq!(model.target_for(&id_of("c")).unwrap().semanticdb_root, None);
    });
}

#[test]
fn unavailable_target_detection_produces_index_unavailable_errors() {
    with_fixture(true, false, false, |fx| {
        let model = fx.load_model();
        assert_eq!(
            model
                .indexable_targets()
                .iter()
                .map(|t| t.bsp_id.clone())
                .collect::<Vec<_>>(),
            vec![id_of("a"), id_of("b")]
        );
        assert_eq!(
            model
                .unavailable_targets()
                .iter()
                .map(|t| t.bsp_id.clone())
                .collect::<Vec<_>>(),
            vec![id_of("c")]
        );
        let errors = model.unavailable_errors();
        assert_eq!(errors.len(), 1);
        match &errors[0] {
            LsError::IndexUnavailable { target } => {
                assert_eq!(target, &id_of("c"));
                assert!(errors[0].message().contains(&id_of("c")));
            }
            other => panic!("expected one IndexUnavailable error, got {other:?}"),
        }
    });
}

#[test]
fn sources_directories_expand_and_uri_to_target_maps_them() {
    with_fixture(true, false, false, |fx| {
        let model = fx.load_model();
        let a = model.target_for(&id_of("a")).unwrap();
        let mut names: Vec<String> = a
            .sources
            .iter()
            .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["A1.scala", "A2.scala", "A3.scala"]);
        assert!(a.sources.iter().all(|p| p.is_file()));
        let b = model.target_for(&id_of("b")).unwrap();
        assert_eq!(b.sources, vec![fx.b_source_file.clone()]);

        let a1_uri = path_to_uri(&fx.a_source_dir.join("A1.scala"));
        let a3_uri = path_to_uri(&fx.a_source_dir.join("nested").join("A3.scala"));
        assert_eq!(model.uri_to_target.get(&a1_uri), Some(&id_of("a")));
        assert_eq!(model.uri_to_target.get(&a3_uri), Some(&id_of("a")));
        assert_eq!(
            model.uri_to_target.get(&path_to_uri(&fx.b_source_file)),
            Some(&id_of("b"))
        );
        assert_eq!(
            model.uri_to_target.get(&path_to_uri(&fx.c_source_file)),
            Some(&id_of("c"))
        );
        assert_eq!(
            model.target_of_uri(&a1_uri).map(|t| t.bsp_id.clone()),
            Some(id_of("a"))
        );
        // the ignored java file never enters the map
        let java_uri = path_to_uri(&fx.a_source_dir.join("Ignored.java"));
        assert_eq!(model.uri_to_target.get(&java_uri), None);
    });
}

#[test]
fn graph_ops_dependencies_dependents_reverse_closure() {
    with_fixture(true, false, false, |fx| {
        let model = fx.load_model();
        let (a, b, c) = (id_of("a"), id_of("b"), id_of("c"));
        assert_eq!(model.dependencies_of(&c), vec![b.clone()]);
        assert_eq!(model.dependencies_of(&a), Vec::<String>::new());
        assert_eq!(model.dependents_of(&a), vec![b.clone()]);
        assert_eq!(model.dependents_of(&c), Vec::<String>::new());
        assert_eq!(
            model.reverse_dependency_closure(&a),
            BTreeSet::from([a.clone(), b.clone(), c.clone()])
        );
        assert_eq!(
            model.reverse_dependency_closure(&b),
            BTreeSet::from([b.clone(), c.clone()])
        );
        assert_eq!(
            model.reverse_dependency_closure(&c),
            BTreeSet::from([c.clone()])
        );
    });
}

#[test]
fn compile_round_trip_ok_diagnostics_and_messages_forwarded() {
    with_fixture(true, false, false, |fx| {
        let outcome = fx
            .session
            .compile(&[id_of("a")], Some("origin-42".to_string()))
            .unwrap();
        assert_eq!(
            outcome,
            BspCompileOutcome::Ok {
                origin_id: Some("origin-42".to_string())
            }
        );
        assert!(outcome.is_ok());
        // The fake emits notifications before answering the request, and the
        // client processes the stream in order, so they are already here.
        let diag = fx
            .diagnostics
            .lock()
            .unwrap()
            .first()
            .cloned()
            .expect("a publishDiagnostics");
        assert_eq!(diag.text_document.uri, path_to_uri(&fx.b_source_file));
        assert_eq!(diag.origin_id, Some("origin-42".to_string()));
        assert_eq!(
            diag.diagnostics.first().map(|d| d.message.clone()),
            Some("value unused in fake target".to_string())
        );
        assert!(fx
            .logs
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.message == "fake compile log"));
        assert!(fx
            .shows
            .lock()
            .unwrap()
            .iter()
            .any(|m| m.message == "fake compile show"));
        assert!(!fx.did_changes.lock().unwrap().is_empty());
    });
}

#[test]
fn compile_failure_surfaces_the_status_code() {
    with_fixture(true, false, false, |fx| {
        let outcome = fx
            .session
            .compile(&[broken_id()], Some("origin-err".to_string()))
            .unwrap();
        assert_eq!(
            outcome,
            BspCompileOutcome::Failed {
                status_code: 2,
                origin_id: Some("origin-err".to_string())
            }
        );
        assert!(!outcome.is_ok());
    });
}

#[test]
fn inverse_sources_uses_the_server_when_advertised() {
    with_fixture(true, false, false, |fx| {
        let model = fx.load_model();
        let uri = path_to_uri(&fx.b_source_file);
        assert_eq!(
            fx.session.inverse_sources(&uri, &model).unwrap(),
            vec![id_of("b")]
        );
        assert_eq!(fx.server.inverse_sources_calls.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn inverse_sources_falls_back_to_uri_to_target_without_the_capability() {
    with_fixture(false, false, false, |fx| {
        let model = fx.load_model();
        let uri = path_to_uri(&fx.c_source_file);
        assert_eq!(
            fx.session.inverse_sources(&uri, &model).unwrap(),
            vec![id_of("c")]
        );
        assert_eq!(
            fx.session
                .inverse_sources("file:///nowhere/X.scala", &model)
                .unwrap(),
            Vec::<String>::new()
        );
        assert_eq!(fx.server.inverse_sources_calls.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn dependency_sources_and_output_paths_attempted_when_advertised() {
    with_fixture(true, true, true, |fx| {
        let ids = vec![id_of("a"), id_of("b")];
        let deps = fx.session.dependency_sources(&ids);
        assert!(
            deps.is_some(),
            "dependencySources should be attempted when advertised"
        );
        assert_eq!(deps.unwrap().len(), 2);
        assert_eq!(fx.server.dependency_sources_calls.load(Ordering::SeqCst), 1);
        let outputs = fx.session.output_paths(&ids);
        assert!(
            outputs.is_some(),
            "outputPaths should be attempted when advertised"
        );
        assert_eq!(outputs.unwrap().len(), 2);
        assert_eq!(fx.server.output_paths_calls.load(Ordering::SeqCst), 1);
    });
}

#[test]
fn dependency_sources_and_output_paths_none_when_not_advertised() {
    with_fixture(true, false, false, |fx| {
        let ids = vec![id_of("a")];
        assert!(fx.session.dependency_sources(&ids).is_none());
        assert!(fx.session.output_paths(&ids).is_none());
        assert_eq!(fx.server.dependency_sources_calls.load(Ordering::SeqCst), 0);
        assert_eq!(fx.server.output_paths_calls.load(Ordering::SeqCst), 0);
        // Empty id sets are also None.
        assert!(fx.session.dependency_sources(&[]).is_none());
    });
}

#[test]
fn shutdown_is_graceful_and_requests_after_close_raise_typed_errors() {
    with_fixture(true, false, false, |fx| {
        fx.session.shutdown();
        assert!(fx.session.is_closed());
        assert!(fx.server.shutdown_requested.load(Ordering::SeqCst));
        eventually("build/exit received", || {
            fx.server.exit_received.load(Ordering::SeqCst)
        });
        match fx.session.workspace_build_targets() {
            Err(BspError::SessionClosed { method }) => assert_eq!(method, "workspace/buildTargets"),
            other => panic!("expected SessionClosed, got {other:?}"),
        }
    });
}
