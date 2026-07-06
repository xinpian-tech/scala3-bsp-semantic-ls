//! Production bootstrap end-to-end: `IndexBootstrap` over a real SemanticDB
//! model, driven through the actual `serve` message loop and `CoreHandlers`
//! dispatch (not a fake seam), so the wiring `initialized` -> bootstrap ->
//! ingest -> ready query is exercised over a genuinely ingested index.
//!
//! The build model points at the committed pinned-scalac corpus that the engine
//! tests use (`ls-engine/tests/fixtures`: sourceroot `sources`, SemanticDB
//! targetroots `out-a`/`out-b`/`out-c`). The workspace root handed to
//! `initialize` is a throwaway temp dir, so the store lands there and the
//! read-only fixtures are never mutated.

use std::collections::HashMap;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use ls_bsp::model::{BspProjectModel, BspTarget};
use ls_engine::{CompileOutcome, CompileService};
use ls_index_model::uri::{normalize_uri, path_to_uri};
use ls_index_model::LsError;
use ls_server::{
    read_frame, reload_build_model, serve, Bootstrap, BuildCompiler, CoreHandlers, CoreServices,
    DocumentStore, Handlers, IndexBootstrap, LoadOutcome, ModelSource, PcLocation, PcQueryService,
    PublishDiagnosticsParams, ReadyModel, Request, RequestContext, RequestId, ServerCore,
    ServerHooks, WorkspaceState,
};

fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../ls-engine/tests/fixtures")
}

/// The three-target project model over the committed corpus (`fixture-b`
/// depends on `fixture-a`), all sharing the one `sources` sourceroot.
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

/// Recursively collect every `.scala` file under `dir`.
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

/// The corpus's URI ownership: each source mapped to its target by top-level
/// directory (`a`/`b`/`c`; `shared`/`dep` belong to `fixture-a`). Mirrors what
/// `ProjectModelLoader` builds from `buildTarget/sources`.
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

fn frame(body: Value) -> Vec<u8> {
    let text = serde_json::to_string(&body).unwrap();
    format!("Content-Length: {}\r\n\r\n{}", text.len(), text).into_bytes()
}

fn request(id: i64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

fn responses(bytes: Vec<u8>) -> Vec<Value> {
    let mut reader = Cursor::new(bytes);
    let mut out = Vec::new();
    while let Some(body) = ls_server::read_frame(&mut reader).unwrap() {
        out.push(serde_json::from_slice(&body).unwrap());
    }
    out
}

#[test]
fn production_bootstrap_ingests_the_model_and_serves_real_queries() {
    let store_root = tempfile::tempdir().unwrap();
    let core_uri = path_to_uri(&fixtures_root().join("sources/a/src/pkga/Core.scala"));

    let input = [
        frame(request(
            1,
            "initialize",
            json!({ "rootUri": path_to_uri(store_root.path()) }),
        )),
        frame(notification("initialized", json!({}))),
        frame(request(2, "workspace/symbol", json!({ "query": "Core" }))),
        frame(request(
            3,
            "textDocument/references",
            json!({
                "textDocument": { "uri": core_uri },
                // `class Core` — zero-based line 2, the `C` of `Core` at column 6.
                "position": { "line": 2, "character": 6 },
                "context": { "includeDeclaration": true }
            }),
        )),
        frame(request(
            4,
            "textDocument/prepareRename",
            json!({
                "textDocument": { "uri": core_uri },
                "position": { "line": 2, "character": 6 }
            }),
        )),
        frame(request(
            5,
            "workspace/executeCommand",
            json!({ "command": "scala3SemanticLs.reindex" }),
        )),
        // A source the live model does not own -> gated out by requireSemanticdb.
        frame(request(
            6,
            "textDocument/references",
            json!({
                "textDocument": { "uri": "file:///elsewhere/Outside.scala" },
                "position": { "line": 0, "character": 0 }
            }),
        )),
        // An owned source passes the gate: documentHighlight answers real
        // occurrences over the index (not the gate's hard error).
        frame(request(
            7,
            "textDocument/documentHighlight",
            json!({
                "textDocument": { "uri": core_uri },
                "position": { "line": 2, "character": 6 }
            }),
        )),
        frame(notification("exit", json!({}))),
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
    let by_id = |id: i64| {
        out.iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no response for id {id} in {out:?}"))
    };

    // workspace/symbol resolves a real hit over the freshly ingested index, with
    // its defining location under the fixture sourceroot.
    let symbols = by_id(2)["result"].as_array().expect("symbol result array");
    let core_symbol = symbols
        .iter()
        .find(|s| s["name"] == "Core")
        .unwrap_or_else(|| panic!("no Core symbol in {symbols:?}"));
    assert!(core_symbol["location"]["uri"]
        .as_str()
        .unwrap()
        .ends_with("a/src/pkga/Core.scala"));

    // references answers real locations for the class under the cursor.
    let refs = by_id(3)["result"]
        .as_array()
        .expect("references result array");
    assert!(!refs.is_empty(), "expected references for Core, got none");

    // prepareRename returns the span of the `Core` occurrence under the cursor.
    let prepare = &by_id(4)["result"];
    assert_eq!(
        prepare["start"]["line"], 2,
        "prepareRename range: {prepare}"
    );
    assert_eq!(prepare["start"]["character"], 6);
    assert_eq!(prepare["end"]["character"], 10);

    // reindex re-ingests the retained workspace and returns the ingest summary.
    let summary = by_id(5)["result"].as_str().expect("reindex string result");
    assert!(
        summary.starts_with("ingest: segment ") && summary.contains(" docs "),
        "unexpected reindex summary: {summary}"
    );

    // references for a source the model does not own fails through
    // requireSemanticdb with NoSemanticdb, not a NotIndexed or empty result.
    let gated = &by_id(6)["error"];
    assert_eq!(
        gated["code"], -32803,
        "requireSemanticdb should RequestFailed"
    );
    assert!(
        gated["message"]
            .as_str()
            .unwrap()
            .contains("has no SemanticDB output"),
        "expected NoSemanticdb message, got {gated}"
    );

    // documentHighlight over the owned Core source passes the gate and returns
    // real same-document occurrences (the class declaration at minimum).
    let highlights = by_id(7)["result"]
        .as_array()
        .expect("documentHighlight result array");
    assert!(
        !highlights.is_empty(),
        "expected highlights for Core, got none"
    );
}

/// The indexable corpus plus one target that OWNS a source but produces no
/// SemanticDB (`semanticdb_root: None` → excluded from `indexable_targets()` and
/// so from the ingested workspace), whose source URI is still in `uri_to_target`.
/// Returns the model and the non-indexable-owned source URI.
fn model_with_non_indexable_owner() -> (BspProjectModel, String) {
    let fx = fixtures_root();
    let unindexed_uri = path_to_uri(&fx.join("noindex/pkgc/Widget.scala"));
    let mut model = fixture_model();
    model.targets.push(BspTarget {
        bsp_id: "fixture-c-noindex".to_string(),
        display_name: "fixture-c-noindex".to_string(),
        scala_version: "3".to_string(),
        scalac_options: Vec::new(),
        class_directory: fx.join("noindex-out"),
        classpath: Vec::new(),
        // No SemanticDB output: owns sources but is not indexable.
        semanticdb_root: None,
        sourceroot: Some(fx.join("noindex")),
        sources: Vec::new(),
        direct_deps: Vec::new(),
    });
    model
        .uri_to_target
        .insert(unindexed_uri.clone(), "fixture-c-noindex".to_string());
    (model, unindexed_uri)
}

/// A source the live model owns through a target that produces no SemanticDB is
/// a hard `NoSemanticdb`/`RequestFailed` error on every gated method — never
/// `NotIndexed`, an empty result, or `null` — proving the gate consults the live
/// model's ownership, not just index/sourceroot mappability. Rust equivalent of
/// the retained `LsEndToEndTest`/`RealBspCoreTest` no-SemanticDB module-`c`
/// cases. Exercises the `uri_to_target.get(uri) == Some(non-indexable)` branch,
/// distinct from the unowned-URI case (`get` → `None`).
#[test]
fn a_source_owned_by_a_non_indexable_target_is_no_semanticdb() {
    let store_root = tempfile::tempdir().unwrap();
    let (model, unindexed_uri) = model_with_non_indexable_owner();

    // Pin the scenario so a silent break of the ownership setup fails loudly
    // rather than passing via the unowned-URI branch: the source is owned in the
    // model by a target that produces no SemanticDB.
    let owner = model
        .uri_to_target
        .get(&unindexed_uri)
        .expect("the source is owned in the model");
    let owner_target = model
        .targets
        .iter()
        .find(|t| &t.bsp_id == owner)
        .expect("the owning target is in the model");
    assert!(
        !owner_target.indexable(),
        "the owning target must be non-indexable (no SemanticDB output)"
    );

    let input = [
        frame(request(
            1,
            "initialize",
            json!({ "rootUri": path_to_uri(store_root.path()) }),
        )),
        frame(notification("initialized", json!({}))),
        frame(request(
            2,
            "textDocument/references",
            json!({
                "textDocument": { "uri": unindexed_uri },
                "position": { "line": 0, "character": 0 },
                "context": { "includeDeclaration": true }
            }),
        )),
        frame(request(
            3,
            "textDocument/documentHighlight",
            json!({
                "textDocument": { "uri": unindexed_uri },
                "position": { "line": 0, "character": 0 }
            }),
        )),
        frame(request(
            4,
            "textDocument/prepareRename",
            json!({
                "textDocument": { "uri": unindexed_uri },
                "position": { "line": 0, "character": 0 }
            }),
        )),
        frame(request(
            5,
            "textDocument/rename",
            json!({
                "textDocument": { "uri": unindexed_uri },
                "position": { "line": 0, "character": 0 },
                "newName": "Renamed"
            }),
        )),
        frame(notification("exit", json!({}))),
    ]
    .concat();

    let mut core = ServerCore::new();
    let out = serve_pumped(
        &mut core,
        &CoreHandlers,
        || IndexBootstrap::new(|_root: &Path| Ok(model_with_non_indexable_owner().0)),
        input,
    );

    assert!(core.state.is_ready(), "workspace did not reach ready");
    let by_id = |id: i64| {
        out.iter()
            .find(|r| r["id"] == id)
            .unwrap_or_else(|| panic!("no response for id {id} in {out:?}"))
    };

    // references / documentHighlight / prepareRename / rename all hard-error with
    // the NoSemanticdb message; none falls through to NotIndexed, [], or null.
    for id in [2, 3, 4, 5] {
        let response = by_id(id);
        let error = &response["error"];
        assert_eq!(
            error["code"], -32803,
            "method id {id} should RequestFailed, got {response}"
        );
        let message = error["message"].as_str().unwrap_or_default();
        assert!(
            message.contains("has no SemanticDB output"),
            "id {id}: expected the NoSemanticdb message, got {response}"
        );
        assert!(
            !message.contains("not part of any indexed build target"),
            "id {id}: must be NoSemanticdb, not NotIndexed, got {response}"
        );
    }
}

/// A compile capability that records the target sets it is asked to compile and
/// returns a fixed outcome, so the `compile` executeCommand is exercised over
/// the real serve path without a live build server.
struct RecordingCompiler {
    succeed: bool,
    fail_reason: String,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl CompileService for RecordingCompiler {
    fn compile(&self, targets: &[String]) -> CompileOutcome {
        self.calls.lock().unwrap().push(targets.to_vec());
        if self.succeed {
            CompileOutcome::Ok
        } else {
            CompileOutcome::Failed {
                reason: self.fail_reason.clone(),
            }
        }
    }
}

impl BuildCompiler for RecordingCompiler {
    fn refetch_model(&self) -> Result<BspProjectModel, String> {
        Err("recording compiler does not refetch".to_string())
    }
}

/// Injects the real fixture model plus a recording compiler into the bootstrap.
#[derive(Clone)]
struct CompilingModelSource {
    succeed: bool,
    fail_reason: String,
    calls: Arc<Mutex<Vec<Vec<String>>>>,
}

impl ModelSource for CompilingModelSource {
    fn load(&self, _root: &Path) -> Result<LoadOutcome, String> {
        Ok(LoadOutcome::Model(ReadyModel {
            model: fixture_model(),
            compiler: Box::new(RecordingCompiler {
                succeed: self.succeed,
                fail_reason: self.fail_reason.clone(),
                calls: self.calls.clone(),
            }),
        }))
    }
}

/// A model source that selects the no-BSP recovered-index mode: no live model,
/// serve whatever the store recovered.
#[derive(Clone)]
struct NoBspSource;

impl ModelSource for NoBspSource {
    fn load(&self, _root: &Path) -> Result<LoadOutcome, String> {
        Ok(LoadOutcome::NoBsp)
    }
}

/// The fixture model with `fixture-c` removed (its target and its URI ownership),
/// so a reload back to the full model is observable: `fixture-c` becomes
/// registered, owns its sources, and its SemanticDB (the `CopyCore` `Core`) is
/// reingested.
fn fixture_model_ab_only() -> BspProjectModel {
    let full = fixture_model();
    let targets = full
        .targets
        .into_iter()
        .filter(|t| t.bsp_id != "fixture-c")
        .collect();
    let uri_to_target = full
        .uri_to_target
        .into_iter()
        .filter(|(_, t)| t != "fixture-c")
        .collect();
    BspProjectModel::new(targets, uri_to_target)
}

/// A compile capability whose model refetch returns a scripted model, so a
/// build-target-change reload runs over a retained session without a live server.
struct ReloadingCompiler {
    reload_model: BspProjectModel,
}

impl CompileService for ReloadingCompiler {
    fn compile(&self, _targets: &[String]) -> CompileOutcome {
        CompileOutcome::Ok
    }
}

impl BuildCompiler for ReloadingCompiler {
    fn refetch_model(&self) -> Result<BspProjectModel, String> {
        Ok(self.reload_model.clone())
    }
}

/// A model source that loads `initial` and whose retained compiler refetches
/// `reload` — the model the build server reports after a `buildTarget/didChange`.
#[derive(Clone)]
struct ReloadingModelSource {
    initial: BspProjectModel,
    reload: BspProjectModel,
}

impl ModelSource for ReloadingModelSource {
    fn load(&self, _root: &Path) -> Result<LoadOutcome, String> {
        Ok(LoadOutcome::Model(ReadyModel {
            model: self.initial.clone(),
            compiler: Box::new(ReloadingCompiler {
                reload_model: self.reload.clone(),
            }),
        }))
    }
}

/// The byte offset just past the `initialized` notification in a framed input
/// stream, or the full length if none is present.
fn split_after_initialized(bytes: &[u8]) -> usize {
    let mut reader = std::io::Cursor::new(bytes.to_vec());
    while let Ok(Some(body)) = read_frame(&mut reader) {
        let is_initialized = serde_json::from_slice::<serde_json::Value>(&body)
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

/// Drives `serve` over the scripted `input` in two passes split just after the
/// `initialized` notification, on one persistent `core`: the first pass reaches
/// Ready/Failed (the async bootstrap worker is drained at that pass's loop end),
/// so the ready-requiring requests in the second pass observe the settled state —
/// as a real client, which waits between `initialized` and its queries while the
/// bootstrap runs. `bootstrap` yields a fresh instance per pass (the second pass's
/// is unused for building, but `serve` consumes one by value). Returns the
/// responses from both passes, in order.
fn serve_pumped<S, H, B>(
    core: &mut ServerCore<S>,
    handlers: &H,
    mut bootstrap: impl FnMut() -> B,
    input: Vec<u8>,
) -> Vec<serde_json::Value>
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
        let mut reader = std::io::Cursor::new(chunk);
        let mut writer = Vec::new();
        let publish = |_p: PublishDiagnosticsParams| {};
        let hooks = ServerHooks {
            publish_diagnostics: &publish,
        };
        serve(
            &mut reader,
            &mut writer,
            core,
            handlers,
            bootstrap(),
            &hooks,
        )
        .unwrap();
        out.extend(responses(writer));
    }
    out
}

fn ready_over<M: ModelSource>(
    bootstrap: &IndexBootstrap<M>,
    root: &Path,
    documents: &DocumentStore,
) -> CoreServices {
    match bootstrap.build(Some(root.to_path_buf())) {
        WorkspaceState::Ready(services) => {
            bootstrap.replay(&services, documents);
            services
        }
        other => panic!("expected Ready, got {:?}", other.status_line()),
    }
}

// Ports ScalaLs.reloadBuildModel: a build-target change refetches the model over
// the retained session, reingests into the reused store, rebuilds the URI
// ownership, and re-registers the PC target set into the reused island.
#[test]
fn a_build_target_change_reload_refetches_reingests_and_reconfigures() {
    let dir = tempfile::tempdir().unwrap();
    let documents = DocumentStore::new();
    let bootstrap = IndexBootstrap::new(ReloadingModelSource {
        initial: fixture_model_ab_only(),
        reload: fixture_model(),
    });
    let before = ready_over(&bootstrap, dir.path(), &documents);
    // Before the reload the c target is unknown: unregistered, unowned, and its
    // `CopyCore` `Core` is not indexed.
    assert!(!before.pc.is_registered("fixture-c"));
    assert!(!before.uri_to_target.values().any(|t| t == "fixture-c"));
    let core_hits_before = before.orchestrator.workspace_symbols("Core", 100).len();

    let after = match reload_build_model(before, &documents) {
        WorkspaceState::Ready(services) => services,
        other => panic!("expected Ready after reload, got {:?}", other.status_line()),
    };
    // After the reload the refetched model's c target is registered, owns its
    // sources, and its symbols were reingested into the reused store.
    assert!(after.pc.is_registered("fixture-c"));
    assert!(after.uri_to_target.values().any(|t| t == "fixture-c"));
    assert!(
        after.orchestrator.workspace_symbols("Core", 100).len() > core_hits_before,
        "reingest did not add the c target's Core symbol"
    );
}

// A transient refetch failure keeps serving the previous ready snapshot rather
// than dropping the workspace to a failed state (the Scala `catch NonFatal`).
#[test]
fn a_build_target_change_reload_keeps_the_snapshot_on_a_refetch_failure() {
    let dir = tempfile::tempdir().unwrap();
    let documents = DocumentStore::new();
    // The closure blanket source gives an `UnavailableCompiler`, whose
    // `refetch_model` fails, so the reload must keep the loaded snapshot.
    let bootstrap = IndexBootstrap::new(|_root: &Path| Ok(fixture_model_ab_only()));
    let before = ready_over(&bootstrap, dir.path(), &documents);
    let owned_before = before.uri_to_target.len();

    let after = match reload_build_model(before, &documents) {
        WorkspaceState::Ready(services) => services,
        other => panic!(
            "a failed refetch must keep the ready snapshot, got {:?}",
            other.status_line()
        ),
    };
    assert_eq!(after.uri_to_target.len(), owned_before);
    assert!(!after.pc.is_registered("fixture-c"));
}

// A build-target change to a model with NO indexable targets must NOT wipe the
// index: Scala gates its reingest on `workspaceTargets.targets.nonEmpty`, so the
// prior segment is kept and the un-gated workspace/symbol still answers the old
// symbols even though the new (empty) model owns no URIs.
#[test]
fn a_build_target_change_reload_to_an_empty_model_keeps_the_prior_index() {
    let dir = tempfile::tempdir().unwrap();
    let documents = DocumentStore::new();
    let bootstrap = IndexBootstrap::new(ReloadingModelSource {
        initial: fixture_model(),
        reload: BspProjectModel::new(Vec::new(), HashMap::new()),
    });
    let before = ready_over(&bootstrap, dir.path(), &documents);
    let hits_before = before.orchestrator.workspace_symbols("Core", 100).len();
    assert!(
        hits_before > 0,
        "the fixture should index some Core symbols"
    );

    let after = match reload_build_model(before, &documents) {
        WorkspaceState::Ready(services) => services,
        other => panic!("expected Ready, got {:?}", other.status_line()),
    };
    // The empty model owns no URIs, but the prior segment is retained, so the
    // un-gated workspace/symbol still answers the old symbols (not an empty index).
    assert!(after.uri_to_target.is_empty());
    assert_eq!(
        after.orchestrator.workspace_symbols("Core", 100).len(),
        hits_before,
        "an empty-model reload must keep the prior index, not wipe it"
    );
}

// End-to-end over the serve loop: a build-target change flagged during startup is
// drained on the first ready turn, so the following workspace/symbol reflects the
// reloaded model (the c target's CopyCore.scala hit appears).
#[test]
fn a_build_target_change_reloads_the_model_over_the_serve_loop() {
    let dir = tempfile::tempdir().unwrap();
    let source = ReloadingModelSource {
        initial: fixture_model_ab_only(),
        reload: fixture_model(),
    };
    let input = [
        frame(request(
            1,
            "initialize",
            json!({ "rootUri": path_to_uri(dir.path()) }),
        )),
        frame(notification("initialized", json!({}))),
        frame(request(2, "workspace/symbol", json!({ "query": "Core" }))),
        frame(notification("exit", json!({}))),
    ]
    .concat();
    let mut core = ServerCore::new();
    // The build server reports a target change during startup: the flag is set
    // while the async bootstrap runs and is drained on the first ready turn (the
    // workspace/symbol query), so the query observes the reloaded model.
    core.reload_flag()
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let out = serve_pumped(
        &mut core,
        &CoreHandlers,
        || IndexBootstrap::new(source.clone()),
        input,
    );
    let symbols = out[1]["result"]
        .as_array()
        .expect("workspace/symbol returns an array");
    assert!(
        symbols.iter().any(|s| s["location"]["uri"]
            .as_str()
            .unwrap_or_default()
            .contains("CopyCore.scala")),
        "reload did not reingest the c target: {:?}",
        out[1]["result"]
    );
}

/// Bootstrap over the fixture model with a recording compiler, run the `compile`
/// executeCommand, and return its result and the recorded compile domains.
fn run_compile(succeed: bool, fail_reason: &str) -> (Value, Vec<Vec<String>>) {
    let store_root = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let source = CompilingModelSource {
        succeed,
        fail_reason: fail_reason.to_string(),
        calls: calls.clone(),
    };

    let input = [
        frame(request(
            1,
            "initialize",
            json!({ "rootUri": path_to_uri(store_root.path()) }),
        )),
        frame(notification("initialized", json!({}))),
        frame(request(
            2,
            "workspace/executeCommand",
            json!({ "command": "scala3SemanticLs.compile" }),
        )),
        frame(notification("exit", json!({}))),
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
    let result = out
        .iter()
        .find(|r| r["id"] == 2)
        .expect("compile response")
        .clone();
    let recorded = calls.lock().unwrap().clone();
    (result, recorded)
}

/// `compile` over the ingested indexable targets reports the ok summary and
/// compiles exactly the indexable bsp ids. Ports the `Compile` executeCommand
/// branch (`s.compiler.compile(s.indexableBspIds)` -> "compile ok (N targets)").
#[test]
fn compile_reports_ok_over_the_indexable_targets() {
    let (result, calls) = run_compile(true, "");
    assert_eq!(result["result"], "compile ok (3 targets)");
    assert_eq!(calls.len(), 1, "compile invoked once");
    let mut domain = calls[0].clone();
    domain.sort();
    assert_eq!(domain, vec!["fixture-a", "fixture-b", "fixture-c"]);
}

/// A failed compile surfaces the status in the summary
/// (`BspCompileOutcome.Failed(code, _)` -> "compile failed: $code").
#[test]
fn compile_reports_the_failure_status() {
    let (result, _calls) = run_compile(false, "ERROR");
    assert_eq!(result["result"], "compile failed: ERROR");
}

/// Bootstrap over the fixture model with a recording compiler, run `rename` for
/// `Core` at its declaration, and return the response and the recorded compile
/// domains (the FreshRequired ladder compiles before re-resolving).
fn run_rename(succeed: bool, new_name: &str) -> (Value, Vec<Vec<String>>) {
    let store_root = tempfile::tempdir().unwrap();
    let calls = Arc::new(Mutex::new(Vec::new()));
    let source = CompilingModelSource {
        succeed,
        fail_reason: "ERROR".to_string(),
        calls: calls.clone(),
    };
    let core_uri = path_to_uri(&fixtures_root().join("sources/a/src/pkga/Core.scala"));

    let input = [
        frame(request(
            1,
            "initialize",
            json!({ "rootUri": path_to_uri(store_root.path()) }),
        )),
        frame(notification("initialized", json!({}))),
        frame(request(
            2,
            "textDocument/rename",
            json!({
                "textDocument": { "uri": core_uri },
                "position": { "line": 2, "character": 6 },
                "newName": new_name
            }),
        )),
        frame(notification("exit", json!({}))),
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
    let result = out
        .iter()
        .find(|r| r["id"] == 2)
        .expect("rename response")
        .clone();
    let recorded = calls.lock().unwrap().clone();
    (result, recorded)
}

/// `rename` renames the class under the cursor across the workspace: the result
/// is an LSP WorkspaceEdit whose changes include the declaring source with a
/// TextEdit carrying the new name at the occurrence span, and the FreshRequired
/// ladder compiled the reverse-dependency closure. Ports `ScalaLs.rename`.
#[test]
fn rename_produces_a_workspace_edit_over_the_declaration() {
    let (result, calls) = run_rename(true, "Renamed");
    let changes = result["result"]["changes"]
        .as_object()
        .unwrap_or_else(|| panic!("expected a WorkspaceEdit, got {result}"));
    let (_, core_edits) = changes
        .iter()
        .find(|(uri, _)| uri.ends_with("a/src/pkga/Core.scala"))
        .unwrap_or_else(|| panic!("Core.scala not in the edit: {result}"));
    let edits = core_edits.as_array().expect("edit list");
    assert!(
        edits.iter().any(|e| {
            e["newText"] == "Renamed"
                && e["range"]["start"]["line"] == 2
                && e["range"]["start"]["character"] == 6
                && e["range"]["end"]["character"] == 10
        }),
        "no Core declaration edit in {edits:?}"
    );
    // The FreshRequired ladder compiled the reverse-dependency closure.
    assert!(!calls.is_empty(), "rename did not compile the domain");
}

/// A failed compile in the rename ladder is a hard error
/// (`LsError::CompileFailed` -> `RequestFailed`), never a partial or empty edit.
#[test]
fn rename_with_a_failing_compile_is_a_request_failed_error() {
    let (result, _calls) = run_rename(false, "Renamed");
    assert_eq!(
        result["error"]["code"], -32803,
        "expected RequestFailed, got {result}"
    );
}

/// Bootstraps the fixture model and returns the Ready `CoreServices` directly
/// (not through `serve`), so a test can drive `require_semanticdb` over the
/// ingested index while flipping the public `bsp_connected` / `uri_to_target`
/// fields to model the no-BSP recovered-index warm-restart mode.
fn ready_fixture_services() -> (CoreServices, tempfile::TempDir) {
    let store_root = tempfile::tempdir().unwrap();
    let bootstrap = IndexBootstrap::new(|_root: &Path| Ok(fixture_model()));
    match bootstrap.build(Some(store_root.path().to_path_buf())) {
        WorkspaceState::Ready(services) => (services, store_root),
        other => panic!("bootstrap not ready: {}", other.status_line()),
    }
}

/// A fake PC returning canned locations and recording its document-lifecycle
/// calls, so the ready definition/typeDefinition handlers and the notification
/// forwarding are driven over the real fixture-ingested services (the production
/// `IslandPcService` would need a live JVM to answer). The recorded `events` and
/// the `open` mirror are behind `Arc<Mutex<_>>` so a test can read them after the
/// fake is boxed into the services.
#[derive(Clone, Default)]
struct FakePc {
    definition: Vec<PcLocation>,
    type_definition: Vec<PcLocation>,
    completion: Value,
    hover: Value,
    signature_help: Value,
    registered: bool,
    resolved: Option<Value>,
    events: Arc<Mutex<Vec<String>>>,
    open: Arc<Mutex<Vec<String>>>,
}

impl PcQueryService for FakePc {
    fn did_open(&self, target_id: &str, uri: &str, text: &str) {
        self.events
            .lock()
            .unwrap()
            .push(format!("open {target_id} {uri} {text}"));
        self.open.lock().unwrap().push(uri.to_string());
    }
    fn did_change(&self, uri: &str, text: &str) {
        self.events
            .lock()
            .unwrap()
            .push(format!("change {uri} {text}"));
    }
    fn did_close(&self, uri: &str) {
        self.events.lock().unwrap().push(format!("close {uri}"));
        self.open.lock().unwrap().retain(|u| u != uri);
    }
    fn is_open(&self, uri: &str) -> bool {
        self.open.lock().unwrap().iter().any(|u| u == uri)
    }
    fn definition(&self, _u: &str, _l: u32, _c: u32) -> Vec<PcLocation> {
        self.definition.clone()
    }
    fn type_definition(&self, _u: &str, _l: u32, _c: u32) -> Vec<PcLocation> {
        self.type_definition.clone()
    }
    fn completion(&self, _u: &str, _l: u32, _c: u32) -> Value {
        self.completion.clone()
    }
    fn hover(&self, _u: &str, _l: u32, _c: u32) -> Value {
        self.hover.clone()
    }
    fn signature_help(&self, _u: &str, _l: u32, _c: u32) -> Value {
        self.signature_help.clone()
    }
    fn is_registered(&self, _t: &str) -> bool {
        self.registered
    }
    fn resolve_completion_item(&self, _t: &str, _s: &str, item: &Value) -> Value {
        self.resolved.clone().unwrap_or_else(|| item.clone())
    }
}

fn drive(services: &CoreServices, method: &str, params: Value) -> Value {
    let documents = DocumentStore::new();
    let request = Request {
        id: RequestId::Number(1),
        method: method.to_string(),
        params,
    };
    let response = CoreHandlers.handle(RequestContext {
        request: &request,
        services,
        workspace_root: None,
        documents: &documents,
        shutting_down: false,
    });
    serde_json::to_value(&response).unwrap()
}

/// The normalized `file://` URI of the fixture's `Core.scala` (owned by
/// `fixture-a`), the buffer the PC-routing tests drive.
fn core_uri() -> String {
    normalize_uri(&path_to_uri(
        &fixtures_root().join("sources/a/src/pkga/Core.scala"),
    ))
}

/// definition and typeDefinition each route to their own PC op (proven by
/// distinct canned locations) over an open, owned buffer that passes
/// `requireSemanticdb`, and the PC `file://` locations convert to LSP locations.
/// The buffer reaches the PC mirror through the document-notification hook (the
/// `withPcBuffer` precondition), exactly as `didOpen` forwards it.
#[test]
fn definition_and_type_definition_route_to_the_pc_over_an_open_owned_buffer() {
    let (mut services, _root) = ready_fixture_services();
    let core_uri = core_uri();
    services.pc = Box::new(FakePc {
        definition: vec![PcLocation {
            uri: core_uri.clone(),
            start_line: 2,
            start_character: 6,
            end_line: 2,
            end_character: 10,
        }],
        type_definition: vec![PcLocation {
            uri: core_uri.clone(),
            start_line: 5,
            start_character: 0,
            end_line: 5,
            end_character: 4,
        }],
        ..Default::default()
    });
    // Open the buffer through the lifecycle hook so the PC mirror holds it.
    CoreHandlers.on_did_open(&services, &core_uri, "class Core\n");
    let pos = json!({
        "textDocument": { "uri": core_uri.clone() },
        "position": { "line": 2, "character": 6 }
    });

    let def = drive(&services, "textDocument/definition", pos.clone());
    assert_eq!(
        def["result"],
        json!([{
            "uri": core_uri,
            "range": { "start": { "line": 2, "character": 6 }, "end": { "line": 2, "character": 10 } }
        }])
    );

    let type_def = drive(&services, "textDocument/typeDefinition", pos);
    assert_eq!(
        type_def["result"],
        json!([{
            "uri": core_uri,
            "range": { "start": { "line": 5, "character": 0 }, "end": { "line": 5, "character": 4 } }
        }])
    );
}

/// `withPcBuffer`: an owned URI that passes `requireSemanticdb` but is NOT an
/// open buffer (the PC mirror does not hold it) answers the empty list — the PC
/// is never consulted (the fake would return a location, yet the result is
/// empty).
#[test]
fn definition_over_an_owned_but_unopened_buffer_is_an_empty_list() {
    let (mut services, _root) = ready_fixture_services();
    let core_uri = core_uri();
    services.pc = Box::new(FakePc {
        definition: vec![PcLocation {
            uri: core_uri.clone(),
            start_line: 0,
            start_character: 0,
            end_line: 0,
            end_character: 1,
        }],
        ..Default::default()
    });
    // The buffer is never opened, so the PC mirror does not hold it.
    let params = json!({
        "textDocument": { "uri": core_uri },
        "position": { "line": 2, "character": 6 }
    });
    let def = drive(&services, "textDocument/definition", params);
    assert_eq!(def["result"], json!([]));
}

/// completion/hover/signatureHelp each route to their own PC op over an open,
/// owned buffer that passes `requireSemanticdb`, and the PC's JSON result reaches
/// the client verbatim. The buffer reaches the PC mirror through the
/// document-notification hook (the `withPcBuffer` precondition).
#[test]
fn completion_hover_and_signature_help_route_to_the_pc_over_an_open_owned_buffer() {
    let (mut services, _root) = ready_fixture_services();
    let core_uri = core_uri();
    let completion = json!({ "isIncomplete": false, "items": [ { "label": "foo", "kind": 2 } ] });
    let hover = json!({ "contents": { "kind": "markdown", "value": "T" } });
    let signature = json!({ "signatures": [ { "label": "f(a: Int)" } ], "activeSignature": 0 });
    services.pc = Box::new(FakePc {
        completion: completion.clone(),
        hover: hover.clone(),
        signature_help: signature.clone(),
        ..Default::default()
    });
    CoreHandlers.on_did_open(&services, &core_uri, "class Core\n");
    let pos = json!({
        "textDocument": { "uri": core_uri },
        "position": { "line": 2, "character": 6 }
    });

    assert_eq!(
        drive(&services, "textDocument/completion", pos.clone())["result"],
        completion
    );
    assert_eq!(
        drive(&services, "textDocument/hover", pos.clone())["result"],
        hover
    );
    assert_eq!(
        drive(&services, "textDocument/signatureHelp", pos)["result"],
        signature
    );
}

/// `withPcBuffer`: an owned URI that passes `requireSemanticdb` but is NOT an open
/// buffer answers each method's fallback — an empty completion list for
/// completion, `null` for hover/signatureHelp — and the PC is never consulted
/// (the fake would return a populated result, yet the fallback is served).
#[test]
fn pc_query_methods_over_an_owned_but_unopened_buffer_answer_the_fallback() {
    let (mut services, _root) = ready_fixture_services();
    let core_uri = core_uri();
    services.pc = Box::new(FakePc {
        completion: json!({ "isIncomplete": true, "items": [ { "label": "x" } ] }),
        hover: json!({ "contents": "present" }),
        signature_help: json!({ "signatures": [ { "label": "g()" } ] }),
        ..Default::default()
    });
    // The buffer is never opened, so the PC mirror does not hold it.
    let pos = json!({
        "textDocument": { "uri": core_uri },
        "position": { "line": 2, "character": 6 }
    });
    assert_eq!(
        drive(&services, "textDocument/completion", pos.clone())["result"],
        json!({ "isIncomplete": false, "items": [] })
    );
    assert_eq!(
        drive(&services, "textDocument/hover", pos.clone())["result"],
        Value::Null
    );
    assert_eq!(
        drive(&services, "textDocument/signatureHelp", pos)["result"],
        Value::Null
    );
}

/// `lastCompletionTarget` handshake: `completion` records the owning target, so a
/// following `completionItem/resolve` for an item carrying `data.symbol` (with the
/// target still a registered PC config) enriches through the PC. Drives both over
/// the real `CoreHandlers` dispatch, proving the completion write feeds resolve.
#[test]
fn completion_records_the_target_so_a_following_resolve_enriches() {
    let (mut services, _root) = ready_fixture_services();
    let core_uri = core_uri();
    let enriched = json!({ "label": "foo", "detail": "def foo: Int" });
    services.pc = Box::new(FakePc {
        completion: json!({ "isIncomplete": false, "items": [ { "label": "foo" } ] }),
        registered: true,
        resolved: Some(enriched.clone()),
        ..Default::default()
    });
    CoreHandlers.on_did_open(&services, &core_uri, "class Core\n");
    // Completion records lastCompletionTarget = the buffer's owning target.
    drive(
        &services,
        "textDocument/completion",
        json!({
            "textDocument": { "uri": core_uri },
            "position": { "line": 2, "character": 6 }
        }),
    );
    // Resolve now enriches via the PC, keyed by that recorded target.
    let item = json!({ "label": "foo", "data": { "symbol": "pkga/Core#foo()." } });
    assert_eq!(
        drive(&services, "completionItem/resolve", item)["result"],
        enriched
    );
}

/// `TextDocs.didChange` parity: a change for a buffer the PC does not yet hold
/// OPENS it (resolving the owning target), and a subsequent change UPDATES the
/// now-mirrored buffer — never a second open. A close then drops it.
#[test]
fn on_did_change_opens_an_unmirrored_buffer_then_updates_it() {
    let (mut services, _root) = ready_fixture_services();
    let core_uri = core_uri();
    let fake = FakePc::default();
    let events = fake.events.clone();
    services.pc = Box::new(fake);

    // First change: the PC has no buffer, so it opens (owner resolved to fixture-a).
    CoreHandlers.on_did_change(&services, &core_uri, "v1");
    // Second change: the PC now holds it, so it updates.
    CoreHandlers.on_did_change(&services, &core_uri, "v2");
    // Close: dropped from the mirror.
    CoreHandlers.on_did_close(&services, &core_uri);

    assert_eq!(
        *events.lock().unwrap(),
        vec![
            format!("open fixture-a {core_uri} v1"),
            format!("change {core_uri} v2"),
            format!("close {core_uri}"),
        ]
    );
    assert!(!services.pc.is_open(&core_uri));
}

/// `ScalaLs.replayOpenBuffers`: a buffer already open when the workspace reaches
/// ready is replayed into the PC mirror (the production `IslandPcService`), so it
/// is visible to a later PC query — and the replay does NOT boot the JVM (proven
/// by the real service being usable with no `LS_LIBJVM` in the environment).
#[test]
fn ready_replays_pre_opened_buffers_into_the_pc_mirror() {
    let store_root = tempfile::tempdir().unwrap();
    let documents = DocumentStore::new();
    let core_uri = core_uri();
    // Opened during the pre-ready window.
    documents.open(&core_uri, "class Core\n");
    let bootstrap = IndexBootstrap::new(|_root: &Path| Ok(fixture_model()));
    let services = match bootstrap.build(Some(store_root.path().to_path_buf())) {
        WorkspaceState::Ready(services) => {
            // Replay runs on the loop after Ready; drive it directly here.
            bootstrap.replay(&services, &documents);
            services
        }
        other => panic!("bootstrap not ready: {}", other.status_line()),
    };

    // The pre-opened buffer was replayed into the real PC mirror at ready.
    assert!(
        services.pc.is_open(&core_uri),
        "a pre-opened buffer must be replayed into the PC mirror at ready"
    );
    // A buffer that was never open is not mirrored.
    let unopened = normalize_uri(&path_to_uri(
        &fixtures_root().join("sources/b/src/pkgb/Widget.scala"),
    ));
    assert!(!services.pc.is_open(&unopened));
}

/// The no-BSP recovered-index fallback (`ScalaLs.requireSemanticdb`'s
/// `indexedOnDisk` branch): with no live BSP session, a source the recovered
/// index still holds an active document for is serviceable even though no live
/// model owns it.
#[test]
fn no_bsp_recovered_index_serves_a_persisted_source() {
    let (mut services, _root) = ready_fixture_services();
    services.bsp_connected = false;
    services.uri_to_target = HashMap::new();
    let core = path_to_uri(&fixtures_root().join("sources/a/src/pkga/Core.scala"));
    assert!(
        services.require_semanticdb(&core).is_ok(),
        "a persisted active source must be serviceable in no-BSP mode"
    );
}

/// In no-BSP mode a uri with no active persisted document is still a hard
/// `NoSemanticdb` — the fallback does not invent coverage for an unindexed file.
#[test]
fn no_bsp_recovered_index_rejects_an_unindexed_uri() {
    let (mut services, _root) = ready_fixture_services();
    services.bsp_connected = false;
    services.uri_to_target = HashMap::new();
    let absent = path_to_uri(&fixtures_root().join("sources/a/src/pkga/Absent.scala"));
    let err = services
        .require_semanticdb(&absent)
        .expect_err("an unindexed uri must be rejected even in no-BSP mode");
    assert!(matches!(err, LsError::NoSemanticdb { .. }), "got {err:?}");
}

/// A LIVE BSP session suppresses the persisted-index fallback (the fallback is
/// restricted to BSP-less mode): a uri the live model no longer owns is a hard
/// `NoSemanticdb`, never answered from a stale-but-active persisted row.
#[test]
fn a_live_session_suppresses_the_persisted_index_fallback() {
    let (mut services, _root) = ready_fixture_services();
    services.bsp_connected = true;
    services.uri_to_target = HashMap::new();
    let core = path_to_uri(&fixtures_root().join("sources/a/src/pkga/Core.scala"));
    let err = services
        .require_semanticdb(&core)
        .expect_err("a live session must not serve a uri it no longer owns");
    assert!(matches!(err, LsError::NoSemanticdb { .. }), "got {err:?}");
}

/// Serving over the recovered index with no build connection is deferred: with
/// no connection the workspace does not reach Ready, and source-scoped queries
/// answer the not-ready contract rather than serving a divergent recovered
/// index. This keeps the deferred mode absent from the served surface (never
/// advertised-and-broken). `LiveBspModelSource` still detects the no-connection
/// case (`LoadOutcome::NoBsp`); the bootstrap declines to serve it.
#[test]
fn no_bsp_connection_is_a_deferred_failed_bootstrap() {
    let store_root = tempfile::tempdir().unwrap();
    let core_uri = path_to_uri(&fixtures_root().join("sources/a/src/pkga/Core.scala"));

    let input = [
        frame(request(
            1,
            "initialize",
            json!({ "rootUri": path_to_uri(store_root.path()) }),
        )),
        frame(notification("initialized", json!({}))),
        frame(request(
            2,
            "textDocument/references",
            json!({
                "textDocument": { "uri": core_uri },
                "position": { "line": 2, "character": 6 },
                "context": { "includeDeclaration": true }
            }),
        )),
        frame(notification("exit", json!({}))),
    ]
    .concat();

    let mut core = ServerCore::new();
    let out = serve_pumped(
        &mut core,
        &CoreHandlers,
        || IndexBootstrap::new(NoBspSource),
        input,
    );

    // The no-BSP bootstrap does not reach Ready; it is a clean Failed state.
    assert!(
        !core.state.is_ready(),
        "no-BSP bootstrap must not reach Ready"
    );

    // references does not serve a recovered index; it answers the not-ready path.
    let refs = out
        .iter()
        .find(|r| r["id"] == 2)
        .expect("references response");
    assert!(
        refs.get("error").is_some(),
        "references must answer the not-ready contract, not a served result: {refs}"
    );
}
