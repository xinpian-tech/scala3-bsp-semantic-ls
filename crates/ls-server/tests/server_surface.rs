//! The observable LSP server-surface verification suite: the whole server
//! surface driven as a BLACK BOX through the real `serve` message loop (framed
//! JSON-RPC in → framed responses out) over the production `IndexBootstrap` +
//! `CoreHandlers`, with a fixture SemanticDB model standing in for a live BSP
//! session. Nothing here calls an internal handler directly — every assertion is
//! on a response the loop actually wrote — so it verifies the wiring
//! `initialize` → capabilities → `initialized` → async bootstrap → ready
//! dispatch → `executeCommand`/doctor holds end to end.
//!
//! It pins: the exact advertised capability set (present + absent), the pre-ready
//! per-method fallbacks and their post-ready resolution, the `executeCommand`
//! doctor (text + JSON) / reindex / compile / unknown-command behavior, and the
//! hard "index-only session runs with zero JVM in the process" property —
//! asserted here because this test binary never issues a presentation-compiler
//! request, so the embedded island never boots and `libjvm_mapped()` reflects
//! only this suite's behavior.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use ls_bsp::model::{BspProjectModel, BspTarget};
use ls_engine::{CompileOutcome, CompileService};
use ls_index_model::uri::path_to_uri;
use ls_server::{
    libjvm_mapped, serve, Bootstrap, BuildCompiler, CoreHandlers, Handlers, IndexBootstrap,
    LoadOutcome, ModelSource, OutputSink, ReadyModel, ServerCore,
};
use ls_testkit::wire::{by_id, decode_frames, notification, request, split_after_initialized};

// --- fixture model + black-box harness ---------------------------------------

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ls-engine/tests/fixtures")
}

fn collect_scala(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            collect_scala(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("scala") {
            out.push(path);
        }
    }
}

/// The corpus's URI ownership by top-level directory (`a`/`b`/`c`; everything
/// else belongs to `fixture-a`), the same shape `ProjectModelLoader` builds.
fn uri_to_target_for(src: &Path) -> HashMap<String, String> {
    let mut files = Vec::new();
    collect_scala(src, &mut files);
    files
        .into_iter()
        .map(|file| {
            let top = file
                .strip_prefix(src)
                .unwrap()
                .components()
                .next()
                .and_then(|c| c.as_os_str().to_str())
                .unwrap_or_default();
            let target = match top {
                "b" => "fixture-b",
                "c" => "fixture-c",
                _ => "fixture-a",
            };
            (path_to_uri(&file), target.to_string())
        })
        .collect()
}

/// The three-target project model over the committed SemanticDB corpus
/// (`fixture-b` depends on `fixture-a`), all sharing one sourceroot.
fn fixture_model() -> BspProjectModel {
    let fx = fixtures_root();
    let src = fx.join("sources");
    let target = |id: &str, out: &str, deps: Vec<String>| BspTarget {
        bsp_id: id.to_string(),
        display_name: id.to_string(),
        scala_version: "3".to_string(),
        scalac_options: Vec::new(),
        class_directory: fx.join(out),
        classpath: Vec::new(),
        semanticdb_root: Some(fx.join(out)),
        sourceroot: Some(src.clone()),
        sources: Vec::new(),
        direct_deps: deps,
    };
    BspProjectModel::new(
        vec![
            target("fixture-a", "out-a", Vec::new()),
            target("fixture-b", "out-b", vec!["fixture-a".to_string()]),
            target("fixture-c", "out-c", Vec::new()),
        ],
        uri_to_target_for(&src),
    )
}

/// Drives `serve` over the framed `input`, pumping the async bootstrap to
/// completion between the `initialized`-split halves, and returns every framed
/// response the loop wrote.
fn serve_pumped<S, H, B>(
    core: &mut ServerCore<S>,
    handlers: &H,
    mut bootstrap: impl FnMut() -> B,
    input: Vec<u8>,
) -> Vec<Value>
where
    S: Send + 'static,
    H: Handlers<S>,
    B: Bootstrap<S> + Send + Sync + 'static,
{
    let split = split_after_initialized(&input);
    let mut out = Vec::new();
    for chunk in [input[..split].to_vec(), input[split..].to_vec()] {
        if chunk.is_empty() {
            continue;
        }
        let mut reader = Cursor::new(chunk);
        let sink = OutputSink::new(Vec::new());
        serve(&mut reader, &sink, core, handlers, bootstrap()).unwrap();
        out.extend(decode_frames(sink.written()));
    }
    out
}

/// A build compiler that records its `compile` calls, for the `compile`
/// executeCommand assertion; it never refetches a model.
struct RecordingCompiler {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl CompileService for RecordingCompiler {
    fn compile(&self, targets: &[String]) -> CompileOutcome {
        self.calls.lock().unwrap().push(targets.to_vec());
        CompileOutcome::Ok
    }
}

impl BuildCompiler for RecordingCompiler {
    fn refetch_model(&self) -> Result<BspProjectModel, String> {
        Err("recording compiler does not refetch".to_string())
    }
}

/// The fixture model plus a recording compiler, for the `compile` command path.
#[derive(Clone)]
struct RecordingModelSource {
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl ModelSource for RecordingModelSource {
    fn load(&self, _root: &Path) -> Result<LoadOutcome, String> {
        Ok(LoadOutcome::Model(ReadyModel {
            model: fixture_model(),
            compiler: Arc::new(RecordingCompiler {
                calls: self.calls.clone(),
            }),
            server_name: None,
            server_version: None,
        }))
    }
}

fn init(store: &Path) -> Vec<u8> {
    request(1, "initialize", json!({ "rootUri": path_to_uri(store) }))
}

// --- capabilities -------------------------------------------------------------

// initialize advertises EXACTLY the implemented capability set — present and
// absent — over the real loop (the CapabilitiesSuite contract as a black box).
#[test]
fn initialize_advertises_exactly_the_implemented_capability_set() {
    let store = tempfile::tempdir().unwrap();
    let out = serve_pumped(
        &mut ServerCore::new(),
        &CoreHandlers,
        || IndexBootstrap::new(|_root: &Path| Ok(fixture_model())),
        [init(store.path()), notification("exit", json!({}))].concat(),
    );
    let caps = &by_id(&out, 1)["result"]["capabilities"];

    assert_eq!(caps["textDocumentSync"], 2);
    assert_eq!(caps["positionEncoding"], "utf-16");
    assert_eq!(caps["completionProvider"]["resolveProvider"], true);
    assert_eq!(
        caps["completionProvider"]["triggerCharacters"],
        json!(["."])
    );
    assert_eq!(caps["hoverProvider"], true);
    assert_eq!(
        caps["signatureHelpProvider"]["triggerCharacters"],
        json!(["(", ","])
    );
    assert_eq!(caps["definitionProvider"], true);
    assert_eq!(caps["typeDefinitionProvider"], true);
    assert_eq!(caps["referencesProvider"], true);
    assert_eq!(caps["renameProvider"]["prepareProvider"], true);
    assert_eq!(caps["documentHighlightProvider"], true);
    assert_eq!(caps["workspaceSymbolProvider"], true);
    // The payload-backed providers: inlay hints without resolve (every hint
    // ships complete), selection range and folding range as plain booleans.
    assert_eq!(
        caps["inlayHintProvider"],
        json!({ "resolveProvider": false })
    );
    assert_eq!(caps["selectionRangeProvider"], true);
    assert_eq!(caps["foldingRangeProvider"], true);
    // semanticTokens: full + range as plain booleans (no full.delta — no delta
    // handler exists), over the PC-vendored 23-type / 10-modifier legend with
    // the golden anchors at their pinned indices.
    let tokens = &caps["semanticTokensProvider"];
    assert_eq!(tokens["full"], true, "{tokens}");
    assert_eq!(tokens["range"], true, "{tokens}");
    assert_eq!(tokens["legend"]["tokenTypes"].as_array().unwrap().len(), 23);
    assert_eq!(
        tokens["legend"]["tokenModifiers"].as_array().unwrap().len(),
        10
    );
    assert_eq!(tokens["legend"]["tokenTypes"][13], "method");
    assert_eq!(tokens["legend"]["tokenModifiers"][0], "declaration");
    assert_eq!(
        caps["executeCommandProvider"]["commands"],
        json!([
            "scala3SemanticLs.doctor",
            "scala3SemanticLs.reindex",
            "scala3SemanticLs.compile",
            "scala3SemanticLs.pcPluginStatus"
        ])
    );
    let commands = caps["executeCommandProvider"]["commands"].to_string();
    assert!(commands.contains("pcPluginStatus"), "{commands}");

    let info = &by_id(&out, 1)["result"]["serverInfo"];
    assert_eq!(info["name"], "scala3-bsp-semantic-ls");
    assert_eq!(info["version"], "0.1.0");
}

// Every advertised executeCommand routes to a real action (not unknown-command)
// — advertised set == routed set. pcPluginStatus routes to the plugin-status
// arm: these sessions never issue a PC query, so the island is cold and the
// answer is the typed cold status (a success string, never unknown-command).
#[test]
fn advertised_execute_commands_are_exactly_the_routed_ones() {
    let store = tempfile::tempdir().unwrap();
    let mut input = vec![init(store.path()), notification("initialized", json!({}))];
    for (id, command) in [
        (2, "scala3SemanticLs.doctor"),
        (3, "scala3SemanticLs.reindex"),
        (4, "scala3SemanticLs.compile"),
        (5, "scala3SemanticLs.pcPluginStatus"),
    ] {
        input.push(request(
            id,
            "workspace/executeCommand",
            json!({ "command": command }),
        ));
    }
    input.push(notification("exit", json!({})));
    let out = serve_pumped(
        &mut ServerCore::new(),
        &CoreHandlers,
        || IndexBootstrap::new(|_root: &Path| Ok(fixture_model())),
        input.concat(),
    );

    // Every advertised command answers a result.
    for id in [2, 3, 4, 5] {
        assert!(
            by_id(&out, id).get("result").is_some(),
            "command id {id} should route: {:?}",
            by_id(&out, id)
        );
    }
    // The plugin-status answer over the never-booted island is the typed cold
    // status, not an error and not a boot.
    let plugin = by_id(&out, 5)["result"].as_str().unwrap().to_string();
    assert!(plugin.contains("PC island not booted (cold)"), "{plugin}");
}

// --- pre-ready fallbacks then ready resolution --------------------------------

// Before `initialized` the workspace stays NotReady and each method takes its
// per-method fallback: references → typed not-ready error, workspace/symbol →
// [], prepareRename → null.
#[test]
fn pre_ready_methods_take_their_per_method_fallbacks() {
    let store = tempfile::tempdir().unwrap();
    let uri = path_to_uri(&fixtures_root().join("sources/a/src/pkga/Core.scala"));
    // No `initialized` — the stream runs as a single pre-ready pass.
    let input = [
        init(store.path()),
        request(2, "workspace/symbol", json!({ "query": "Core" })),
        request(
            3,
            "textDocument/references",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 2, "character": 6 } }),
        ),
        request(
            4,
            "textDocument/prepareRename",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 2, "character": 6 } }),
        ),
        request(
            5,
            "textDocument/semanticTokens/full",
            json!({ "textDocument": { "uri": uri } }),
        ),
        notification("exit", json!({})),
    ]
    .concat();
    let out = serve_pumped(
        &mut ServerCore::new(),
        &CoreHandlers,
        || IndexBootstrap::new(|_root: &Path| Ok(fixture_model())),
        input,
    );

    assert_eq!(by_id(&out, 2)["result"], json!([]), "workspace/symbol → []");
    let refs = by_id(&out, 3);
    assert!(
        refs.get("result").is_none(),
        "references is an error: {refs}"
    );
    assert!(
        refs["error"]["message"]
            .as_str()
            .unwrap()
            .contains("workspace is not ready"),
        "{refs}"
    );
    assert_eq!(
        by_id(&out, 4)["result"],
        Value::Null,
        "prepareRename → null"
    );
    // The spec result is `SemanticTokens | null`: pre-ready answers null (a
    // client that auto-requests tokens on open must not see an error or an
    // empty stream that wipes its highlighting).
    assert_eq!(
        by_id(&out, 5)["result"],
        Value::Null,
        "semanticTokens/full → null"
    );
}

// After `initialized` the same queries resolve over the freshly ingested index.
#[test]
fn ready_session_serves_real_index_queries() {
    let store = tempfile::tempdir().unwrap();
    let uri = path_to_uri(&fixtures_root().join("sources/a/src/pkga/Core.scala"));
    let input = [
        init(store.path()),
        notification("initialized", json!({})),
        request(2, "workspace/symbol", json!({ "query": "Core" })),
        request(
            3,
            "textDocument/references",
            json!({
                "textDocument": { "uri": uri },
                "position": { "line": 2, "character": 6 },
                "context": { "includeDeclaration": true }
            }),
        ),
        notification("exit", json!({})),
    ]
    .concat();
    let mut core = ServerCore::new();
    let out = serve_pumped(
        &mut core,
        &CoreHandlers,
        || IndexBootstrap::new(|_root: &Path| Ok(fixture_model())),
        input,
    );

    assert!(core.state.is_ready(), "workspace did not reach ready");
    let symbols = by_id(&out, 2)["result"].as_array().expect("symbol array");
    assert!(
        symbols.iter().any(|s| s["name"] == "Core"),
        "no Core symbol: {symbols:?}"
    );
    assert!(
        !by_id(&out, 3)["result"].as_array().unwrap().is_empty(),
        "expected references for Core"
    );
}

// --- doctor + executeCommand over the loop ------------------------------------

// executeCommand doctor renders the text report (state header + all seven
// sections in fixed order) and, with {"json":true}, the structured object (store
// key, no sqlite/postings, a state field).
#[test]
fn execute_command_doctor_renders_text_and_json() {
    let store = tempfile::tempdir().unwrap();
    let input = [
        init(store.path()),
        notification("initialized", json!({})),
        request(
            2,
            "workspace/executeCommand",
            json!({ "command": "scala3SemanticLs.doctor" }),
        ),
        request(
            3,
            "workspace/executeCommand",
            json!({ "command": "scala3SemanticLs.doctor", "arguments": [{ "json": true }] }),
        ),
        notification("exit", json!({})),
    ]
    .concat();
    let out = serve_pumped(
        &mut ServerCore::new(),
        &CoreHandlers,
        || IndexBootstrap::new(|_root: &Path| Ok(fixture_model())),
        input,
    );

    let text = by_id(&out, 2)["result"].as_str().expect("doctor text");
    assert!(text.starts_with("state: ready\n\n"), "{text}");
    for heading in [
        "Runtime:",
        "Nix:",
        "BSP:",
        "SemanticDB:",
        "Store:",
        "PC:",
        "PC Plugins:",
    ] {
        assert!(text.contains(heading), "missing {heading} in {text}");
    }

    let value = &by_id(&out, 3)["result"];
    assert!(value.is_object(), "doctor json is an object: {value}");
    assert!(value.get("store").is_some(), "store key: {value}");
    assert!(value.get("sqlite").is_none(), "no sqlite key: {value}");
    assert!(value.get("postings").is_none(), "no postings key: {value}");
    assert_eq!(value["state"], "ready");
}

// executeCommand reindex reingests, compile drives the build compiler, and an
// unknown command is an invalid-params error.
#[test]
fn execute_command_reindex_compile_and_unknown() {
    let store = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let source = RecordingModelSource {
        calls: Arc::clone(&calls),
    };
    let input = [
        init(store.path()),
        notification("initialized", json!({})),
        request(
            2,
            "workspace/executeCommand",
            json!({ "command": "scala3SemanticLs.reindex" }),
        ),
        request(
            3,
            "workspace/executeCommand",
            json!({ "command": "scala3SemanticLs.compile" }),
        ),
        request(
            4,
            "workspace/executeCommand",
            json!({ "command": "scala3SemanticLs.nonesuch" }),
        ),
        notification("exit", json!({})),
    ]
    .concat();
    let out = serve_pumped(
        &mut ServerCore::new(),
        &CoreHandlers,
        || IndexBootstrap::new(source.clone()),
        input,
    );

    assert!(
        by_id(&out, 2)["result"]
            .as_str()
            .unwrap()
            .starts_with("ingest: segment"),
        "reindex → ingest summary: {:?}",
        by_id(&out, 2)
    );
    assert_eq!(
        by_id(&out, 3)["result"],
        "compile ok (3 targets)",
        "compile → ok over the fixture targets"
    );
    // The compile command actually drove the build compiler.
    assert_eq!(
        calls.lock().unwrap().last().map(|c| c.len()),
        Some(3),
        "compile invoked the build compiler over 3 targets"
    );
    let unknown = by_id(&out, 4);
    assert!(unknown.get("result").is_none(), "{unknown}");
    assert!(
        unknown["error"]["message"]
            .as_str()
            .unwrap()
            .contains("unknown command 'scala3SemanticLs.nonesuch'"),
        "{unknown}"
    );
}

// The doctor renders BEFORE ready too: a NotReady session still returns the full
// report with the pre-ready state header and the live-only sections unavailable.
#[test]
fn doctor_renders_before_ready() {
    let store = tempfile::tempdir().unwrap();
    let input = [
        init(store.path()),
        request(
            2,
            "workspace/executeCommand",
            json!({ "command": "scala3SemanticLs.doctor" }),
        ),
        notification("exit", json!({})),
    ]
    .concat();
    let out = serve_pumped(
        &mut ServerCore::new(),
        &CoreHandlers,
        || IndexBootstrap::new(|_root: &Path| Ok(fixture_model())),
        input,
    );

    let text = by_id(&out, 2)["result"].as_str().expect("doctor text");
    assert!(
        text.starts_with("state: not ready: waiting for the initialized notification\n\n"),
        "{text}"
    );
    assert!(text.contains("Runtime:"), "{text}");
    assert!(text.contains("BSP:\n  unavailable:"), "{text}");
    assert!(text.contains("Store:"), "{text}");
}

// --- cold no-JVM: an index-only session leaves the process JVM-free -----------

// A full index-only session — initialize, index queries, doctor, reindex,
// compile — leaves the embedded presentation-compiler island unbooted: no
// libjvm mapping in the process. The hard "index-only runs with zero JVM"
// assertion, sound here because this whole test binary never issues a PC request.
#[test]
fn an_index_only_session_never_boots_the_embedded_jvm() {
    assert!(
        !libjvm_mapped(),
        "the JVM must not be mapped before an index-only session"
    );

    let store = tempfile::tempdir().unwrap();
    let uri = path_to_uri(&fixtures_root().join("sources/a/src/pkga/Core.scala"));
    let calls = Arc::new(Mutex::new(Vec::new()));
    let source = RecordingModelSource {
        calls: Arc::clone(&calls),
    };
    let input = [
        init(store.path()),
        notification("initialized", json!({})),
        request(2, "workspace/symbol", json!({ "query": "Core" })),
        request(
            3,
            "textDocument/references",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 2, "character": 6 } }),
        ),
        request(
            4,
            "textDocument/documentHighlight",
            json!({ "textDocument": { "uri": uri }, "position": { "line": 2, "character": 6 } }),
        ),
        request(
            5,
            "workspace/executeCommand",
            json!({ "command": "scala3SemanticLs.reindex" }),
        ),
        request(
            6,
            "workspace/executeCommand",
            json!({ "command": "scala3SemanticLs.compile" }),
        ),
        request(
            7,
            "workspace/executeCommand",
            json!({ "command": "scala3SemanticLs.doctor" }),
        ),
        notification("exit", json!({})),
    ]
    .concat();
    let mut core = ServerCore::new();
    let out = serve_pumped(
        &mut core,
        &CoreHandlers,
        || IndexBootstrap::new(source.clone()),
        input,
    );

    assert!(core.state.is_ready(), "workspace did not reach ready");
    // The session actually did index work (not a vacuous pass).
    assert!(
        !by_id(&out, 2)["result"].as_array().unwrap().is_empty(),
        "workspace/symbol resolved a hit"
    );
    // The doctor reports the island cold, non-invasively.
    let doctor = by_id(&out, 7)["result"].as_str().unwrap();
    assert!(doctor.contains("worker status: not booted"), "{doctor}");

    // The hard assertion: after a full index-only session the island never booted.
    assert!(
        !libjvm_mapped(),
        "an index-only session must leave the process JVM-free"
    );
}
