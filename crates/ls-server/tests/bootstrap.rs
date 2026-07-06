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
    serve, Bootstrap, BootstrapContext, CoreHandlers, CoreServices, DocumentStore, Handlers,
    IndexBootstrap, LoadOutcome, ModelSource, PcLocation, PcQueryService, PublishDiagnosticsParams,
    ReadyModel, Request, RequestContext, RequestId, ServerCore, ServerHooks, WorkspaceState,
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

    let mut reader = Cursor::new(input);
    let mut writer = Vec::new();
    let mut core = ServerCore::new();
    let bootstrap = IndexBootstrap::new(|_root: &Path| Ok(fixture_model()));
    let publish = |_p: PublishDiagnosticsParams| {};
    let on_changed = || {};
    let hooks = ServerHooks {
        publish_diagnostics: &publish,
        on_build_targets_changed: &on_changed,
    };
    serve(
        &mut reader,
        &mut writer,
        &mut core,
        &CoreHandlers,
        &bootstrap,
        &hooks,
    )
    .unwrap();

    assert!(core.state.is_ready(), "workspace did not reach ready");
    let out = responses(writer);
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

    let mut reader = Cursor::new(input);
    let mut writer = Vec::new();
    let mut core = ServerCore::new();
    let bootstrap = IndexBootstrap::new(|_root: &Path| Ok(model_with_non_indexable_owner().0));
    let publish = |_p: PublishDiagnosticsParams| {};
    let on_changed = || {};
    let hooks = ServerHooks {
        publish_diagnostics: &publish,
        on_build_targets_changed: &on_changed,
    };
    serve(
        &mut reader,
        &mut writer,
        &mut core,
        &CoreHandlers,
        &bootstrap,
        &hooks,
    )
    .unwrap();

    assert!(core.state.is_ready(), "workspace did not reach ready");
    let out = responses(writer);
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

/// Injects the real fixture model plus a recording compiler into the bootstrap.
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
struct NoBspSource;

impl ModelSource for NoBspSource {
    fn load(&self, _root: &Path) -> Result<LoadOutcome, String> {
        Ok(LoadOutcome::NoBsp)
    }
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

    let mut reader = Cursor::new(input);
    let mut writer = Vec::new();
    let mut core = ServerCore::new();
    let bootstrap = IndexBootstrap::new(source);
    let publish = |_p: PublishDiagnosticsParams| {};
    let on_changed = || {};
    let hooks = ServerHooks {
        publish_diagnostics: &publish,
        on_build_targets_changed: &on_changed,
    };
    serve(
        &mut reader,
        &mut writer,
        &mut core,
        &CoreHandlers,
        &bootstrap,
        &hooks,
    )
    .unwrap();
    assert!(core.state.is_ready(), "workspace did not reach ready");

    let out = responses(writer);
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

    let mut reader = Cursor::new(input);
    let mut writer = Vec::new();
    let mut core = ServerCore::new();
    let bootstrap = IndexBootstrap::new(source);
    let publish = |_p: PublishDiagnosticsParams| {};
    let on_changed = || {};
    let hooks = ServerHooks {
        publish_diagnostics: &publish,
        on_build_targets_changed: &on_changed,
    };
    serve(
        &mut reader,
        &mut writer,
        &mut core,
        &CoreHandlers,
        &bootstrap,
        &hooks,
    )
    .unwrap();
    assert!(core.state.is_ready(), "workspace did not reach ready");

    let out = responses(writer);
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
    let documents = DocumentStore::new();
    let publish = |_p: PublishDiagnosticsParams| {};
    let on_changed = || {};
    let cx = BootstrapContext {
        workspace_root: Some(store_root.path()),
        documents: &documents,
        publish_diagnostics: &publish,
        on_build_targets_changed: &on_changed,
    };
    match bootstrap.run(cx) {
        WorkspaceState::Ready(services) => (services, store_root),
        other => panic!("bootstrap not ready: {}", other.status_line()),
    }
}

/// A fake PC returning canned locations, so the ready definition/typeDefinition
/// handlers are driven over the real fixture-ingested services (the production
/// `IslandPcService` would need a live JVM to answer).
#[derive(Clone, Default)]
struct FakePc {
    definition: Vec<PcLocation>,
    type_definition: Vec<PcLocation>,
}

impl PcQueryService for FakePc {
    fn definition(&self, _t: &str, _u: &str, _txt: &str, _l: u32, _c: u32) -> Vec<PcLocation> {
        self.definition.clone()
    }
    fn type_definition(&self, _t: &str, _u: &str, _txt: &str, _l: u32, _c: u32) -> Vec<PcLocation> {
        self.type_definition.clone()
    }
}

fn drive(services: &CoreServices, documents: &DocumentStore, method: &str, params: Value) -> Value {
    let request = Request {
        id: RequestId::Number(1),
        method: method.to_string(),
        params,
    };
    let response = CoreHandlers.handle(RequestContext {
        request: &request,
        services,
        workspace_root: None,
        documents,
        shutting_down: false,
    });
    serde_json::to_value(&response).unwrap()
}

/// definition and typeDefinition each route to their own PC op (proven by
/// distinct canned locations) over an open, owned buffer that passes
/// `requireSemanticdb`, and the PC `file://` locations convert to LSP locations.
#[test]
fn definition_and_type_definition_route_to_the_pc_over_an_open_owned_buffer() {
    let (mut services, _root) = ready_fixture_services();
    // The document store keys by normalized URI (as `did_open` does), and the
    // handler looks the open buffer up by the normalized request URI; the fixture
    // root carries a `..`, so normalize to match.
    let core_uri = normalize_uri(&path_to_uri(
        &fixtures_root().join("sources/a/src/pkga/Core.scala"),
    ));
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
    });
    let documents = DocumentStore::new();
    documents.open(&core_uri, "class Core\n");
    let pos = json!({
        "textDocument": { "uri": core_uri.clone() },
        "position": { "line": 2, "character": 6 }
    });

    let def = drive(
        &services,
        &documents,
        "textDocument/definition",
        pos.clone(),
    );
    assert_eq!(
        def["result"],
        json!([{
            "uri": core_uri,
            "range": { "start": { "line": 2, "character": 6 }, "end": { "line": 2, "character": 10 } }
        }])
    );

    let type_def = drive(&services, &documents, "textDocument/typeDefinition", pos);
    assert_eq!(
        type_def["result"],
        json!([{
            "uri": core_uri,
            "range": { "start": { "line": 5, "character": 0 }, "end": { "line": 5, "character": 4 } }
        }])
    );
}

/// `withPcBuffer`: an owned URI that passes `requireSemanticdb` but is NOT an
/// open buffer answers the empty list — the PC is never consulted (the fake
/// would return a location, yet the result is empty).
#[test]
fn definition_over_an_owned_but_unopened_buffer_is_an_empty_list() {
    let (mut services, _root) = ready_fixture_services();
    let core_uri = normalize_uri(&path_to_uri(
        &fixtures_root().join("sources/a/src/pkga/Core.scala"),
    ));
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
    // The buffer is not opened, so the PC serves nothing for it.
    let documents = DocumentStore::new();
    let params = json!({
        "textDocument": { "uri": core_uri },
        "position": { "line": 2, "character": 6 }
    });
    let def = drive(&services, &documents, "textDocument/definition", params);
    assert_eq!(def["result"], json!([]));
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

    let mut reader = Cursor::new(input);
    let mut writer = Vec::new();
    let mut core = ServerCore::new();
    let bootstrap = IndexBootstrap::new(NoBspSource);
    let publish = |_p: PublishDiagnosticsParams| {};
    let on_changed = || {};
    let hooks = ServerHooks {
        publish_diagnostics: &publish,
        on_build_targets_changed: &on_changed,
    };
    serve(
        &mut reader,
        &mut writer,
        &mut core,
        &CoreHandlers,
        &bootstrap,
        &hooks,
    )
    .unwrap();

    // The no-BSP bootstrap does not reach Ready; it is a clean Failed state.
    assert!(
        !core.state.is_ready(),
        "no-BSP bootstrap must not reach Ready"
    );

    // references does not serve a recovered index; it answers the not-ready path.
    let out = responses(writer);
    let refs = out
        .iter()
        .find(|r| r["id"] == 2)
        .expect("references response");
    assert!(
        refs.get("error").is_some(),
        "references must answer the not-ready contract, not a served result: {refs}"
    );
}
