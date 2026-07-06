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

use serde_json::{json, Value};

use ls_bsp::model::{BspProjectModel, BspTarget};
use ls_index_model::uri::path_to_uri;
use ls_server::{
    serve, CoreHandlers, IndexBootstrap, PublishDiagnosticsParams, ServerCore, ServerHooks,
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
