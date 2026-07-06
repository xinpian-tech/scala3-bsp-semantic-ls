//! Live embedded-JVM go-to-definition at the `ls-server` layer: boots the
//! PRODUCTION PC island through the real `IndexBootstrap` -> `IslandPcService`
//! and drives `textDocument/definition` through the real `CoreHandlers` dispatch
//! over an open buffer, proving the `ls-server` -> PC-island seam end-to-end (not
//! a fake). The presentation compiler resolves an in-buffer symbol and returns
//! its declaration location across the FFM boundary.
//!
//! Env-gated exactly like the `ls-jvm` live checks (`LS_LIBJVM` +
//! `PC_HOST_AGENT_JAR` + `LS_PC_TARGET_CLASSPATH`); skips cleanly when unset. A
//! separate test binary because only one JVM boots per process.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use ls_bsp::model::{BspProjectModel, BspTarget};
use ls_index_model::uri::{normalize_uri, path_to_uri};
use ls_server::{
    Bootstrap, BootstrapContext, CoreHandlers, DocumentStore, Handlers, IndexBootstrap,
    PublishDiagnosticsParams, Request, RequestContext, RequestId, WorkspaceState,
};

// `foo` is defined and referenced in the same buffer, so the presentation
// compiler resolves the go-to in-buffer (line 1) without the cross-file index
// resolver — proving the seam with the smallest live fixture.
const SOURCE: &str = "object Defs:\n  def foo = 1\n  val x = foo\n";

struct Env {
    classpath: Vec<PathBuf>,
}

fn env() -> Option<Env> {
    // The island boot itself reads LS_LIBJVM / PC_HOST_AGENT_JAR from the process
    // environment; the classpath is the PC target's compile classpath.
    std::env::var_os("LS_LIBJVM")?;
    std::env::var_os("PC_HOST_AGENT_JAR")?;
    Some(Env {
        classpath: std::env::var("LS_PC_TARGET_CLASSPATH")
            .ok()?
            .split(':')
            .filter(|e| !e.is_empty())
            .map(PathBuf::from)
            .collect(),
    })
}

#[test]
fn live_definition_over_an_open_buffer_routes_through_the_pc_island() {
    let Some(env) = env() else {
        eprintln!(
            "live_pc: skipping — set LS_LIBJVM + PC_HOST_AGENT_JAR + \
             LS_PC_TARGET_CLASSPATH to run the live PC definition test"
        );
        return;
    };

    let workspace = std::env::temp_dir().join(format!("ls-server-live-pc-{}", std::process::id()));
    let src_root = workspace.join("src");
    let sdb_root = workspace.join("meta");
    fs::create_dir_all(&src_root).expect("create sourceroot");
    fs::create_dir_all(&sdb_root).expect("create semanticdb root");
    let src_file = src_root.join("Defs.scala");
    fs::write(&src_file, SOURCE).expect("write source");
    let file_uri = normalize_uri(&path_to_uri(&src_file));

    // The build model: one indexable target owning the source, with the Scala
    // library classpath so the presentation compiler can type-check the buffer.
    // No `.semanticdb` is emitted; `requireSemanticdb` passes because the target
    // is in the ingested workspace and owns the URI.
    let src_root_m = src_root.clone();
    let sdb_root_m = sdb_root.clone();
    let src_file_m = src_file.clone();
    let file_uri_m = file_uri.clone();
    let classpath = env.classpath.clone();
    let model_source = move |_root: &Path| -> Result<BspProjectModel, String> {
        let mut uri_to_target = HashMap::new();
        uri_to_target.insert(file_uri_m.clone(), "live-target".to_string());
        Ok(BspProjectModel::new(
            vec![BspTarget {
                bsp_id: "live-target".to_string(),
                display_name: "live-target".to_string(),
                scala_version: "3.8.4".to_string(),
                scalac_options: Vec::new(),
                class_directory: sdb_root_m.clone(),
                classpath: classpath.clone(),
                semanticdb_root: Some(sdb_root_m.clone()),
                sourceroot: Some(src_root_m.clone()),
                sources: vec![src_file_m.clone()],
                direct_deps: Vec::new(),
            }],
            uri_to_target,
        ))
    };

    let documents = DocumentStore::new();
    let publish = |_p: PublishDiagnosticsParams| {};
    let on_changed = || {};
    let services = match IndexBootstrap::new(model_source).run(BootstrapContext {
        workspace_root: Some(&workspace),
        documents: &documents,
        publish_diagnostics: &publish,
        on_build_targets_changed: &on_changed,
    }) {
        WorkspaceState::Ready(services) => services,
        other => panic!("bootstrap not ready: {}", other.status_line()),
    };

    // The buffer must be open for the presentation compiler to serve it: the
    // document-notification hook mirrors it into the PC (the query then boots the
    // island and replays the mirror), exactly as `didOpen` forwards it.
    documents.open(&file_uri, SOURCE);
    CoreHandlers.on_did_open(&services, &file_uri, SOURCE);
    // `  val x = foo` is line 2; `foo` starts at column 10.
    let request = Request {
        id: RequestId::Number(1),
        method: "textDocument/definition".to_string(),
        params: json!({
            "textDocument": { "uri": file_uri },
            "position": { "line": 2, "character": 10 }
        }),
    };
    let response = CoreHandlers.handle(RequestContext {
        request: &request,
        services: &services,
        workspace_root: Some(&workspace),
        documents: &documents,
        shutting_down: false,
    });
    let value = serde_json::to_value(&response).expect("serialize response");
    let locations = value["result"]
        .as_array()
        .unwrap_or_else(|| panic!("expected a definition location array, got {value:?}"));
    // The presentation compiler resolves the in-buffer `foo` to its declaration
    // on line 1 (`  def foo = 1`), returned across the boundary.
    assert!(
        locations
            .iter()
            .any(|loc| loc["range"]["start"]["line"] == 1),
        "definition must land on the in-buffer declaration line: {value:?}"
    );
}
