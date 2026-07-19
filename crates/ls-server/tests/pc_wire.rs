//! The PC-backed LSP wire surface, JVM-free: completion, `completionItem/resolve`,
//! hover, signature help, and the definition family driven over the framed wire
//! through the REAL `serve` loop and the REAL `IndexBootstrap` — with the
//! embedded island replaced by the testkit's scriptable [`FakePcService`]
//! through the `IndexBootstrap::with_pc` seam. Until this suite, these methods
//! were wire-testable only against real mill + a real JVM (`real_bsp_pc.rs`);
//! here the routing, gating (`require_semanticdb`, the `withPcBuffer` fallback,
//! the resolve target gate), and response mapping are pinned hermetically with
//! insta snapshots.

use std::sync::Arc;

use serde_json::{json, Value};

use ls_server::{IndexBootstrap, PcQueryService};
use ls_testkit::client::WireClient;
use ls_testkit::fake_bsp::FakeBsp;
use ls_testkit::fake_pc::FakePcService;
use ls_testkit::fixtures::core_uri;
use ls_testkit::positions::position_of;

/// Boot the production serve loop over the fake BSP corpus with the fake PC
/// injected through the production bootstrap. Returns the interactive client
/// and the shared fake-PC handle for lifecycle assertions.
fn boot() -> (WireClient, Arc<FakePcService>) {
    let pc = FakePcService::new();
    let pc_for_factory = Arc::clone(&pc);
    let client = WireClient::boot_in_process_with(move |parts| {
        let (fake, source) =
            FakeBsp::start(Arc::clone(&parts.reload_flag), Arc::clone(&parts.sink));
        let bootstrap = IndexBootstrap::with_pc(source, FakePcService::factory(pc_for_factory));
        (fake.workspace_root.clone(), fake, bootstrap)
    });
    (client, pc)
}

const DIRTY: &str = "package pkga\n\nclass Core(val label: String):\n  def ping: String = \"core \" + label\n  def extra: Int = 41\n";

/// Replace every occurrence of the machine-dependent corpus URI prefix in
/// string values so snapshots are host-independent.
fn scrub(value: &Value) -> Value {
    let sources = ls_testkit::fixtures::source_uri("");
    let prefix = sources.trim_end_matches('/');
    match value {
        Value::String(s) => Value::String(s.replace(prefix, "[SOURCES]")),
        Value::Array(items) => Value::Array(items.iter().map(scrub).collect()),
        Value::Object(map) => {
            Value::Object(map.iter().map(|(k, v)| (k.clone(), scrub(v))).collect())
        }
        other => other.clone(),
    }
}

// The whole PC-backed wire surface over one session: every method routes to the
// injected PC service and maps its result to the LSP response — pinned by
// snapshot — while the process stays JVM-free.
#[test]
fn the_pc_backed_wire_surface_is_served_jvm_free() {
    assert!(
        !ls_server::libjvm_mapped(),
        "the embedded JVM must be unmapped before the session"
    );
    let (mut client, pc) = boot();
    client.initialize();
    client.await_ready();

    let uri = core_uri();
    client.did_open_uri(&uri, DIRTY);
    let (line, character) = position_of(DIRTY, "ping", 0);
    let at = json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } });

    let completion = client.result("textDocument/completion", at.clone());
    insta::assert_json_snapshot!("completion", scrub(&completion));

    // `completionItem/resolve` params are the item itself; the item's
    // `data.symbol` plus the recorded last-completion target pass the gates and
    // reach the PC service's resolve.
    let item = completion["items"][0].clone();
    let resolved = client.result("completionItem/resolve", item);
    insta::assert_json_snapshot!("resolved-item", scrub(&resolved));

    let hover = client.result("textDocument/hover", at.clone());
    insta::assert_json_snapshot!("hover", scrub(&hover));

    let help = client.result("textDocument/signatureHelp", at.clone());
    insta::assert_json_snapshot!("signature-help", scrub(&help));

    let definition = client.result("textDocument/definition", at.clone());
    insta::assert_json_snapshot!("definition", scrub(&definition));

    let type_definition = client.result("textDocument/typeDefinition", at.clone());
    insta::assert_json_snapshot!("type-definition", scrub(&type_definition));

    let calls = pc.calls();
    for expected in [
        "did_open",
        "completion",
        "resolve",
        "hover",
        "signature_help",
        "definition",
        "type_definition",
    ] {
        assert!(
            calls.iter().any(|c| c.starts_with(expected)),
            "no {expected} call reached the PC service: {calls:?}"
        );
    }
    assert!(
        !ls_server::libjvm_mapped(),
        "the PC-backed wire surface must stay JVM-free with the injected fake"
    );
    client.shutdown();
}

// The document lifecycle mirrors into the PC service: didChange replaces the
// mirrored text, didClose drops the buffer, and a PC query on the closed buffer
// takes the `withPcBuffer` empty fallback (never an error, never a stale answer).
#[test]
fn the_document_lifecycle_mirrors_into_the_pc_service() {
    let (mut client, pc) = boot();
    client.initialize();
    client.await_ready();

    let uri = core_uri();
    client.did_open_uri(&uri, DIRTY);
    let changed = format!("{DIRTY}\nclass Extra\n");
    client.did_change_uri(&uri, &changed);
    // Notifications carry no response; fence on a request so the loop (which
    // processes messages in order) has applied the lifecycle notifications.
    let fence =
        json!({ "textDocument": { "uri": uri }, "position": { "line": 0, "character": 0 } });
    let _ = client.result("textDocument/hover", fence);
    assert_eq!(
        pc.mirrored_text(&uri).as_deref(),
        Some(changed.as_str()),
        "didChange must replace the mirrored text"
    );

    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": uri } }),
    );
    // The close is a notification; synchronize on a following request.
    let at = json!({ "textDocument": { "uri": uri }, "position": { "line": 2, "character": 6 } });
    let definition = client.result("textDocument/definition", at);
    assert_eq!(
        definition,
        json!([]),
        "a PC query on a closed buffer takes the empty withPcBuffer fallback"
    );
    assert!(!pc.is_open(&uri), "didClose must drop the mirrored buffer");
    client.shutdown();
}

// `require_semanticdb` runs before the buffer gate: a source owned by a target
// compiled WITHOUT SemanticDB stays a hard error for the definition family even
// with an open dirty buffer.
#[test]
fn pc_queries_on_a_no_semanticdb_source_stay_hard_errors() {
    let (mut client, _pc) = boot();
    client.initialize();
    client.await_ready();

    let uri = client.file_uri("nosdb/NoSdb.scala");
    client.did_open_uri(&uri, "class NoSdb\n");
    let at = json!({ "textDocument": { "uri": uri }, "position": { "line": 0, "character": 6 } });
    let error = client.error_message("textDocument/definition", at);
    assert!(
        error.contains("has no SemanticDB output"),
        "expected the hard NoSemanticdb error, got: {error}"
    );
    client.shutdown();
}
