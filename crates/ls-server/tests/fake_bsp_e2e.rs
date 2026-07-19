//! Fake-BSP end-to-end: the whole server driven over the framed LSP wire against
//! an in-process Rust-hosted fake BSP server. Unlike the fixture-model suites,
//! bootstrap here loads the project model over the REAL BSP client (a real
//! `BspSession` speaking JSON-RPC to the fake server over a `UnixStream`) and
//! retains it in the REAL session-backed compiler, so `initialize`/`initialized`
//! handshake, model load, compile, diagnostics forwarding, and session teardown
//! all run through production code. A port of the Scala `LsEndToEndTest`.
//!
//! The fake server, the fixture-corpus geometry, and the wire builders live in
//! the shared [`ls_testkit`] (`fake_bsp`, `fixtures`, `wire`). The fake describes
//! the committed `ls-engine` SemanticDB fixture corpus (targets
//! `fixture-a`/`fixture-b`/`fixture-c` with real `.semanticdb` under
//! `out-a`/`out-b`/`out-c`) plus one `fixture-nosdb` target compiled WITHOUT
//! `-Xsemanticdb`, so its source stays a hard `NoSemanticdb` error (SemanticDB
//! is mandatory — a source in such a target is a hard error, never empty). It
//! drives capabilities + the real BSP handshake, index queries, compile-diagnostic
//! forwarding (publish / clear / clear-once suppression), rename over the retained
//! compiler (success + compile failure), `buildTarget/didChange` reload, the
//! dirty-buffer PC-only surface (JVM-free), and LSP shutdown teardown.
//!
//! This binary boots no embedded JVM: the fake fixture corpus has no matching
//! runtime classpath, so live presentation-compiler completion is covered where
//! it is sound — over real mill in the `real_bsp_pc` binary (one JVM per process).
//! Keeping this binary island-free makes its cold-start `libjvm_mapped()` check
//! unconditionally valid.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use ls_bsp::uri::path_to_uri;
use ls_server::{serve, CoreHandlers, CoreServices, IndexBootstrap, OutputSink, ServerCore};
use ls_testkit::fake_bsp::{
    bsp_diagnostics, bsp_error, CompileScript, FakeBsp, FakeBspModelSource, FakeBuildServer,
};
use ls_testkit::fixtures::{build_target, core_uri, default_targets, source_uri};
use ls_testkit::wire::{
    by_id, decode_frames, notification, publishes, request, split_after_initialized,
};

// --- harness ------------------------------------------------------------------

struct E2e {
    server: Arc<FakeBuildServer>,
    workspace_root: PathBuf,
    source: FakeBspModelSource<Vec<u8>>,
    _fake: FakeBsp,
}

/// Stand up the fake server on one end of a socketpair and a `ModelSource` on the
/// other. `reload_flag` is the server core's flag (fired by `buildTarget/didChange`).
fn setup(reload_flag: Arc<AtomicBool>) -> E2e {
    let sink = Arc::new(OutputSink::new(Vec::new()));
    let (fake, source) = FakeBsp::start(reload_flag, sink);
    E2e {
        server: Arc::clone(&fake.server),
        workspace_root: fake.workspace_root.clone(),
        source,
        _fake: fake,
    }
}

/// Drives `serve` over the framed input, pumping the async bootstrap (which talks
/// the real fake BSP) to completion between the `initialized`-split halves.
fn serve_pumped(
    core: &mut ServerCore<CoreServices>,
    source: &FakeBspModelSource<Vec<u8>>,
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
    decode_frames(source.sink.written())
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
            "scala3SemanticLs.compile",
            "scala3SemanticLs.pcPluginStatus"
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

// A cold-start session that answers index-only queries over the REAL BSP client
// (a real `BspSession`, not the fixture model) must never boot the embedded JVM:
// no presentation-compiler request runs, so the island stays cold and libjvm is
// never mapped into the process. This binary boots no island — the live-PC
// completion lives in its own `real_bsp_pc` binary (one JVM per process) — so the
// process-global `libjvm_mapped()` check is unconditionally sound here and runs
// in every configuration.
#[test]
fn an_index_only_session_over_the_real_bsp_client_stays_jvm_free() {
    assert!(
        !ls_server::libjvm_mapped(),
        "the embedded JVM must be unmapped before any session runs"
    );
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let uri = core_uri();
    let input = session_input(
        &e2e.workspace_root,
        vec![
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
        ],
    );
    let out = serve_pumped(&mut core, &e2e.source, input);
    assert!(
        by_id(&out, 2)["result"].is_array(),
        "the index-only symbol query should answer over the real BSP model"
    );
    assert!(
        !ls_server::libjvm_mapped(),
        "an index-only session over the real BSP client must never boot the JVM"
    );
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
    let fresh = source_uri("b/src/pkgb/UseB.scala");
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
    let uri = source_uri("a/src/pkga/Item.scala");
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
        !e2e.server.compiled_targets().is_empty(),
        "rename must compile over the retained BSP session"
    );
}

// A failing `buildTarget/compile` fails the rename with a typed `CompileFailed`
// error, over the retained session.
#[test]
fn rename_surfaces_a_bsp_compile_failure() {
    let mut core = ServerCore::new();
    let e2e = setup(core.reload_flag());
    let uri = source_uri("a/src/pkga/Item.scala");
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
