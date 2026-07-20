//! The two INDEX-backed navigation methods over the framed wire, JVM-free:
//! `textDocument/documentSymbol` (the nested outline) and
//! `textDocument/implementation` (the method override-family query), driven
//! through the REAL `serve` loop + REAL `IndexBootstrap` over the committed
//! pinned-scalac fixture corpus (real dotty SemanticDB, real
//! `overridden_symbols` edges) with the island replaced by the testkit fake
//! PC. Neither method touches the PC: an open buffer is not required, a dirty
//! buffer still answers index truth, and the island stays cold throughout.

use std::sync::Arc;

use serde_json::{json, Value};

use ls_server::IndexBootstrap;
use ls_testkit::client::WireClient;
use ls_testkit::fake_bsp::FakeBsp;
use ls_testkit::fake_pc::FakePcService;
use ls_testkit::fixtures::{core_uri, source_uri, sources_root};
use ls_testkit::positions::position_of;

fn source_text(rel: &str) -> String {
    std::fs::read_to_string(sources_root().join(rel)).expect("fixture source")
}

/// Boot the production serve loop over the fake BSP corpus (the pc_wire boot).
fn boot() -> (WireClient, Arc<FakePcService>) {
    let pc = FakePcService::new();
    let pc_for_factory = Arc::clone(&pc);
    let client = WireClient::boot_in_process_with(move |parts| {
        let (fake, source) =
            FakeBsp::start(Arc::clone(&parts.reload_flag), Arc::clone(&parts.sink));
        let pc_diagnostics = Arc::clone(&source.pc_diagnostics);
        let bootstrap = IndexBootstrap::with_pc(source, FakePcService::factory(pc_for_factory))
            .with_pc_diagnostics(pc_diagnostics);
        (fake.workspace_root.clone(), fake, bootstrap)
    });
    (client, pc)
}

/// Replace the machine-dependent corpus URI prefix so snapshots are
/// host-independent (the pc_wire scrub).
fn scrub(value: &Value) -> Value {
    let sources = ls_testkit::fixtures::source_uri("");
    let prefix = sources.trim_end_matches('/');
    match value {
        Value::String(s) => Value::String(s.replace(prefix, "[SOURCES]")),
        Value::Array(items) => Value::Array(items.iter().map(scrub).collect()),
        Value::Object(map) => Value::Object(
            map.iter()
                .map(|(k, v)| (k.replace(prefix, "[SOURCES]"), scrub(v)))
                .collect(),
        ),
        other => other.clone(),
    }
}

// documentSymbol over NEVER-OPENED corpus files: the index knows closed files,
// so no didOpen precedes either request. The nested trees — class/companion/
// trait/enum-with-cases on Core.scala, the overload pair on Over.scala — are
// pinned by snapshot, and the island is never booted.
#[test]
fn document_symbol_outlines_closed_files_from_the_index() {
    assert!(
        !ls_server::libjvm_mapped(),
        "cold island before the session"
    );
    let (mut client, _pc) = boot();
    client.initialize();
    client.await_ready();

    let core = client.result(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": core_uri() } }),
    );
    insta::assert_json_snapshot!("document-symbol-core", scrub(&core));

    let over = client.result(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": source_uri("a/src/pkga/Over.scala") } }),
    );
    insta::assert_json_snapshot!("document-symbol-over", scrub(&over));

    assert!(
        !ls_server::libjvm_mapped(),
        "documentSymbol must not boot the island"
    );
    client.shutdown();
}

// A DIRTY buffer still answers index truth (the outline lags until save,
// never an error): open Core.scala, diverge it, and get the same outline as
// the on-disk index answer.
#[test]
fn a_dirty_buffer_still_answers_the_indexed_outline() {
    let (mut client, _pc) = boot();
    client.initialize();
    client.await_ready();

    let uri = core_uri();
    let at = json!({ "textDocument": { "uri": uri } });
    let clean = client.result("textDocument/documentSymbol", at.clone());

    let disk = source_text("a/src/pkga/Core.scala");
    client.did_open_uri(&uri, &disk);
    client.did_change_uri(&uri, &format!("{disk}\nclass Fresh\n"));
    let dirty = client.result("textDocument/documentSymbol", at);
    assert_eq!(dirty, clean, "index truth, unchanged by the dirty buffer");
    client.shutdown();
}

// implementation over the real corpus family: on the `Greeter#greet`
// declaration the response is the overrider's def site in Impl.scala — and on
// the overrider itself, the honest empty (nothing overrides the leaf).
#[test]
fn implementation_resolves_the_corpus_override_family() {
    let (mut client, _pc) = boot();
    client.initialize();
    client.await_ready();

    let core_text = source_text("a/src/pkga/Core.scala");
    let (line, character) = position_of(&core_text, "greet", 0);
    let result = client.result(
        "textDocument/implementation",
        json!({
            "textDocument": { "uri": core_uri() },
            "position": { "line": line, "character": character }
        }),
    );
    insta::assert_json_snapshot!("implementation-greet", scrub(&result));

    let impl_uri = source_uri("a/src/pkga/Impl.scala");
    let impl_text = source_text("a/src/pkga/Impl.scala");
    let (line, character) = position_of(&impl_text, "greet", 0);
    let leaf = client.result(
        "textDocument/implementation",
        json!({
            "textDocument": { "uri": impl_uri },
            "position": { "line": line, "character": character }
        }),
    );
    assert_eq!(leaf, json!([]), "a leaf override answers the honest empty");

    assert!(
        !ls_server::libjvm_mapped(),
        "implementation must not boot the island"
    );
    client.shutdown();
}

// Call hierarchy over the framed wire, JVM-free: prepare -> incoming -> outgoing
// round-tripped through the item's `data` field over the real corpus. Pins the
// usage-hierarchy semantics end to end — incoming keeps the DISCONNECTED
// target-C caller (no closure pruning, the deliberate difference from
// references), outgoing resolves the body's target — and the island stays cold.
#[test]
fn call_hierarchy_prepares_then_round_trips_incoming_and_outgoing() {
    assert!(
        !ls_server::libjvm_mapped(),
        "cold island before the session"
    );
    let (mut client, _pc) = boot();
    client.initialize();
    client.await_ready();

    // prepare on the `make` definition in Core.scala.
    let core_text = source_text("a/src/pkga/Core.scala");
    let (line, character) = position_of(&core_text, "make", 0);
    let prepared = client.result(
        "textDocument/prepareCallHierarchy",
        json!({
            "textDocument": { "uri": core_uri() },
            "position": { "line": line, "character": character }
        }),
    );
    let items = prepared.as_array().expect("prepare returns an array");
    assert_eq!(items.len(), 1, "one definition-side item: {prepared}");
    let item = items[0].clone();
    assert_eq!(item["name"], "make", "{item}");
    // The raw SemanticDB symbol round-trips through `data`.
    assert_eq!(item["data"]["symbol"], "pkga/Core.make().", "{item}");
    // The index knows name spans only: range == selectionRange.
    assert_eq!(item["range"], item["selectionRange"], "{item}");

    // incomingCalls: the four callers, INCLUDING the disconnected target-C
    // caller in CopyCore.scala (references prunes it; call hierarchy does not).
    let incoming = client.result("callHierarchy/incomingCalls", json!({ "item": item }));
    let calls = incoming.as_array().expect("incoming returns an array");
    let names: std::collections::BTreeSet<&str> = calls
        .iter()
        .map(|c| c["from"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(
        names,
        ["core", "defaultCore"].into_iter().collect(),
        "caller names: {incoming}"
    );
    let uris: Vec<&str> = calls
        .iter()
        .map(|c| c["from"]["uri"].as_str().unwrap())
        .collect();
    assert!(
        uris.iter()
            .any(|u| u.ends_with("c/src/pkga/CopyCore.scala")),
        "the disconnected target-C caller must appear (no closure pruning): {uris:?}"
    );
    // Every incoming call carries at least one fromRange.
    for call in calls {
        assert!(
            !call["fromRanges"].as_array().unwrap().is_empty(),
            "a caller with no fromRanges: {call}"
        );
    }

    // outgoingCalls: `make`'s body constructs the Core class.
    let outgoing = client.result("callHierarchy/outgoingCalls", json!({ "item": item }));
    let callees = outgoing.as_array().expect("outgoing returns an array");
    let callee_names: Vec<&str> = callees
        .iter()
        .map(|c| c["to"]["name"].as_str().unwrap())
        .collect();
    assert_eq!(callee_names, vec!["Core"], "make calls Core: {outgoing}");

    assert!(
        !ls_server::libjvm_mapped(),
        "call hierarchy must not boot the island"
    );
    client.shutdown();
}

// The gate ladder of both methods: a no-SemanticDB source is the hard typed
// error (never an empty lie), and an implementation cursor that resolves no
// symbol is the typed references-style error.
#[test]
fn index_nav_methods_gate_on_semanticdb_and_type_their_errors() {
    let (mut client, _pc) = boot();
    client.initialize();
    client.await_ready();

    let nosdb = client.file_uri("nosdb/NoSdb.scala");
    let outline_error = client.error_message(
        "textDocument/documentSymbol",
        json!({ "textDocument": { "uri": nosdb } }),
    );
    assert!(
        outline_error.contains("has no SemanticDB output"),
        "documentSymbol: {outline_error}"
    );
    let impl_error = client.error_message(
        "textDocument/implementation",
        json!({ "textDocument": { "uri": nosdb }, "position": { "line": 0, "character": 6 } }),
    );
    assert!(
        impl_error.contains("has no SemanticDB output"),
        "implementation: {impl_error}"
    );

    // A symbol-free cursor on an indexed file: the typed no-symbol error.
    let cursor_error = client.error_message(
        "textDocument/implementation",
        json!({
            "textDocument": { "uri": core_uri() },
            "position": { "line": 1, "character": 0 }
        }),
    );
    assert!(
        cursor_error.contains("no symbol occurrence"),
        "expected the typed cursor error, got: {cursor_error}"
    );
    client.shutdown();
}
