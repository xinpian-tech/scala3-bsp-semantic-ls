//! The PC-backed LSP wire surface, JVM-free: completion, `completionItem/resolve`,
//! hover, signature help, the definition family, and the payload-backed
//! inlayHint/selectionRange/foldingRange methods driven over the framed wire
//! through the REAL `serve` loop and the REAL `IndexBootstrap` — with the
//! embedded island replaced by the testkit's scriptable [`FakePcService`]
//! through the `IndexBootstrap::with_pc` seam. Until this suite, these methods
//! were wire-testable only against real mill + a real JVM (`real_bsp_pc.rs`);
//! here the routing, gating (`require_semanticdb` where it applies, the
//! `withPcBuffer` fallback, the resolve target gate, the selection/folding
//! no-SemanticDB-gate split), and response mapping are pinned hermetically
//! with insta snapshots.

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
        // The shared PC/BSP diagnostics merge layer, wired exactly like main:
        // BSP publishes route through it, and the bootstrap hands it to the
        // ready bundle so the live-typing pull publishes into the same stream.
        let pc_diagnostics = Arc::clone(&source.pc_diagnostics);
        let bootstrap = IndexBootstrap::with_pc(source, FakePcService::factory(pc_for_factory))
            .with_pc_diagnostics(pc_diagnostics);
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
    let _ = client.result("textDocument/hover", fence.clone());
    assert_eq!(
        pc.mirrored_text(&uri).as_deref(),
        Some(changed.as_str()),
        "didChange must replace the mirrored text"
    );

    // A RANGED didChange (incremental sync): the server folds the event into
    // its buffer and the PC seam still receives the FULL post-edit text.
    // Line 6 of `changed` is "class Extra"; replace its name span [6..11).
    let tweaked = changed.replace("class Extra", "class Tweaked");
    client.did_change_range_uri(&uri, 6, 6, 6, 11, "Tweaked", 3);
    let _ = client.result("textDocument/hover", fence);
    assert_eq!(
        pc.mirrored_text(&uri).as_deref(),
        Some(tweaked.as_str()),
        "a ranged didChange must mirror the full post-edit text into the PC"
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

// The payload-backed wire surface (inlayHint / selectionRange / foldingRange)
// over the same JVM-free session: each method routes to the injected PC
// service's payload op and maps the decoded carrier through the lsp-types
// bridge — the exact LSP shapes (label parts with location/tooltip, verbatim
// `data`, the linked selection parents, the folding kind strings) pinned by
// snapshot.
#[test]
fn the_payload_backed_wire_surface_is_served_jvm_free() {
    assert!(
        !ls_server::libjvm_mapped(),
        "the embedded JVM must be unmapped before the session"
    );
    let (mut client, pc) = boot();
    client.initialize();
    client.await_ready();

    let uri = core_uri();
    client.did_open_uri(&uri, DIRTY);

    let inlay = client.result(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 4, "character": 21 }
            }
        }),
    );
    insta::assert_json_snapshot!("inlay-hints", scrub(&inlay));

    let selection = client.result(
        "textDocument/selectionRange",
        json!({
            "textDocument": { "uri": uri },
            "positions": [
                { "line": 2, "character": 6 },
                { "line": 3, "character": 6 }
            ]
        }),
    );
    insta::assert_json_snapshot!("selection-ranges", scrub(&selection));

    let folding = client.result(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": uri } }),
    );
    insta::assert_json_snapshot!("folding-ranges", scrub(&folding));

    let calls = pc.calls();
    for expected in ["inlay_hints", "selection_range", "folding_range"] {
        assert!(
            calls.iter().any(|c| c.starts_with(expected)),
            "no {expected} call reached the PC service: {calls:?}"
        );
    }
    // The server's default hint-category bitset reached the seam verbatim
    // (inferredTypes + implicitParameters + byNameParameters +
    // implicitConversions + namedParameters = 0b111101 = 61).
    assert!(
        calls
            .iter()
            .any(|c| c.starts_with("inlay_hints") && c.ends_with("flags=61")),
        "the default inlay-hint flag set must reach the PC seam: {calls:?}"
    );
    assert!(
        !ls_server::libjvm_mapped(),
        "the payload-backed wire surface must stay JVM-free with the injected fake"
    );
    client.shutdown();
}

// The payload methods' gates over the wire. Closed buffer (the `withPcBuffer`
// fallback): empty hints, null selection (never a position-count-mismatched
// array), empty folds. And the deliberate gate SPLIT on a no-SemanticDB
// source with an OPEN buffer: inlayHint keeps the hard `require_semanticdb`
// error while selectionRange/foldingRange — pure syntax — answer normally.
#[test]
fn payload_methods_gate_on_the_buffer_and_split_on_semanticdb() {
    let (mut client, _pc) = boot();
    client.initialize();
    client.await_ready();

    // Closed buffer: each method's graceful fallback, never an error.
    let uri = core_uri();
    let inlay = client.result(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 1, "character": 0 }
            }
        }),
    );
    assert_eq!(inlay, json!([]));
    let selection = client.result(
        "textDocument/selectionRange",
        json!({
            "textDocument": { "uri": uri },
            "positions": [{ "line": 0, "character": 0 }]
        }),
    );
    assert_eq!(selection, Value::Null);
    let folding = client.result(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert_eq!(folding, json!([]));

    // The no-SemanticDB source, buffer OPEN: the semantic method hard-errors,
    // the syntax methods answer from the (fake) island.
    let nosdb = client.file_uri("nosdb/NoSdb.scala");
    client.did_open_uri(&nosdb, "class NoSdb\n");
    let error = client.error_message(
        "textDocument/inlayHint",
        json!({
            "textDocument": { "uri": nosdb },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 1, "character": 0 }
            }
        }),
    );
    assert!(
        error.contains("has no SemanticDB output"),
        "inlayHint must keep the hard NoSemanticdb error, got: {error}"
    );
    let selection = client.result(
        "textDocument/selectionRange",
        json!({
            "textDocument": { "uri": nosdb },
            "positions": [{ "line": 0, "character": 6 }]
        }),
    );
    assert_eq!(
        selection[0]["range"],
        json!({
            "start": { "line": 0, "character": 6 },
            "end": { "line": 0, "character": 8 }
        }),
        "selectionRange must answer on a no-SemanticDB source: {selection}"
    );
    let folding = client.result(
        "textDocument/foldingRange",
        json!({ "textDocument": { "uri": nosdb } }),
    );
    assert_eq!(
        folding[0]["kind"],
        json!("imports"),
        "foldingRange must answer on a no-SemanticDB source: {folding}"
    );
    client.shutdown();
}

// The semantic-tokens wire surface over the same JVM-free session: `full`
// encodes the fake island's offset nodes against the OPEN BUFFER TEXT into the
// LSP delta stream (the flat five-words-per-token integer array — including a
// cross-line delta and the dropped `-1` unclassified node), `range` slices the
// same nodes server-side before encoding — both pinned by snapshot.
#[test]
fn the_semantic_tokens_wire_surface_is_served_jvm_free() {
    assert!(
        !ls_server::libjvm_mapped(),
        "the embedded JVM must be unmapped before the session"
    );
    let (mut client, pc) = boot();
    client.initialize();
    client.await_ready();

    let uri = core_uri();
    client.did_open_uri(&uri, DIRTY);

    let full = client.result(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    insta::assert_json_snapshot!("semantic-tokens-full", scrub(&full));
    // The stream is well-formed: five words per token, no resultId.
    let data = full["data"].as_array().expect("a data array");
    assert_eq!(data.len() % 5, 0, "{full}");
    assert!(full.get("resultId").is_none(), "{full}");

    // The range slice: only line 0 — the line-2 "Core" token drops, and the
    // kept tokens re-encode from the document origin.
    let range = client.result(
        "textDocument/semanticTokens/range",
        json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 1, "character": 0 }
            }
        }),
    );
    insta::assert_json_snapshot!("semantic-tokens-range", scrub(&range));

    let calls = pc.calls();
    assert!(
        calls
            .iter()
            .filter(|c| c.starts_with("semantic_tokens"))
            .count()
            >= 2,
        "both requests must reach the PC service: {calls:?}"
    );
    assert!(
        !ls_server::libjvm_mapped(),
        "the semantic-tokens wire surface must stay JVM-free with the injected fake"
    );
    client.shutdown();
}

// The semantic-tokens gates: a buffer the PC mirror does not hold answers null
// (`SemanticTokens | null` — never an empty stream that would wipe client
// highlighting), and a no-SemanticDB source keeps the hard `require_semanticdb`
// error even with an open buffer (the hover discipline).
#[test]
fn semantic_tokens_gate_on_the_buffer_and_semanticdb() {
    let (mut client, _pc) = boot();
    client.initialize();
    client.await_ready();

    let uri = core_uri();
    let full = client.result(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert_eq!(full, Value::Null);
    let range = client.result(
        "textDocument/semanticTokens/range",
        json!({
            "textDocument": { "uri": uri },
            "range": {
                "start": { "line": 0, "character": 0 },
                "end": { "line": 1, "character": 0 }
            }
        }),
    );
    assert_eq!(range, Value::Null);

    let nosdb = client.file_uri("nosdb/NoSdb.scala");
    client.did_open_uri(&nosdb, "class NoSdb\n");
    let error = client.error_message(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": nosdb } }),
    );
    assert!(
        error.contains("has no SemanticDB output"),
        "semanticTokens/full must keep the hard NoSemanticdb error, got: {error}"
    );
    client.shutdown();
}

// The live-typing diagnostics flow over the wire: a didChange on an open dirty
// buffer arms the debounced pull; the fake PC answers its canned diagnostic;
// the merged publish reaches the client tagged "scala3-pc (typing)". A
// didClose then clears the overlay with an empty publish (BSP truth alone).
#[test]
fn a_did_change_publishes_pc_tagged_diagnostics_and_did_close_clears_them() {
    let (mut client, pc) = boot();
    client.initialize();
    client.await_ready();

    let uri = core_uri();
    client.did_open_uri(&uri, DIRTY);
    // The pull is didChange-driven: opening alone publishes nothing.
    client.did_change_uri(&uri, &format!("{DIRTY}\nclass Typo\n"));

    let diags = client.await_publish_uri(
        &uri,
        |diags| {
            diags
                .iter()
                .any(|d| d["source"] == json!("scala3-pc (typing)"))
        },
        "the PC-tagged typing diagnostics publish",
    );
    assert_eq!(diags.len(), 1, "{diags:?}");
    assert_eq!(diags[0]["severity"], json!(2));
    assert_eq!(diags[0]["code"], json!("FAKE1"));
    assert!(
        diags[0]["message"]
            .as_str()
            .unwrap()
            .starts_with("fake diagnostic for"),
        "{diags:?}"
    );
    assert!(
        pc.calls().iter().any(|c| c.starts_with("pc_diagnostics")),
        "the debounced pull must reach the PC service: {:?}",
        pc.calls()
    );

    client.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": uri } }),
    );
    let cleared = client.await_publish_uri(
        &uri,
        |diags| diags.is_empty(),
        "the clearing publish after didClose",
    );
    assert!(cleared.is_empty());
    assert!(
        !ls_server::libjvm_mapped(),
        "the typing-diagnostics flow must stay JVM-free with the injected fake"
    );
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

// `$/cancelRequest` for a QUEUED request over the wire: completion 1 is held in
// flight at the fake PC while completion 2 (and a didChange behind it) queue on
// the serve loop; the reader thread intercepts the cancel for 2 even though
// dispatch is busy. Releasing 1 answers 1 normally and 2 with −32800 WITHOUT
// the PC ever seeing 2, and the notification queued after the cancelled request
// is still processed in order.
#[test]
fn a_cancelled_queued_completion_answers_request_cancelled_over_the_wire() {
    use std::sync::Barrier;
    use std::time::{Duration, Instant};

    let (mut client, pc) = boot();
    client.initialize();
    client.await_ready();

    let uri = core_uri();
    client.did_open_uri(&uri, DIRTY);
    let (line, character) = position_of(DIRTY, "ping", 0);
    let at = json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } });

    // Hold the next completion in flight at the fake PC (a one-shot gate).
    let gate = Arc::new(Barrier::new(2));
    pc.gate_method("completion", Arc::clone(&gate));
    let first = client.send_request_no_wait("textDocument/completion", at.clone());
    let deadline = Instant::now() + Duration::from_secs(60);
    while !pc.calls().iter().any(|c| c.starts_with("completion")) {
        assert!(
            Instant::now() < deadline,
            "completion 1 never reached the PC"
        );
        std::thread::sleep(Duration::from_millis(2));
    }

    // Queue completion 2 behind the blocked dispatch, a didChange behind it,
    // then cancel 2 and wait for the reader thread to intercept the cancel
    // before releasing 1 (the deterministic fence).
    let second = client.send_request_no_wait("textDocument/completion", at.clone());
    let retagged = DIRTY.replace("class Core", "class Retagged");
    client.did_change_uri(&uri, &retagged);
    client.cancel(second);
    client.await_cancel_registered(second);
    gate.wait();

    let first_response = client.await_response(first);
    assert!(
        first_response.get("error").is_none(),
        "completion 1 answers normally: {first_response}"
    );
    assert_eq!(
        first_response["result"]["items"][0]["label"],
        format!("fakeItem@{line}:{character}")
    );
    let second_response = client.await_response(second);
    assert_eq!(
        second_response["error"]["code"],
        ls_server::jsonrpc::error_codes::REQUEST_CANCELLED,
        "{second_response}"
    );
    assert_eq!(second_response["error"]["message"], "request cancelled");

    // The PC saw exactly one completion — the cancelled request was answered
    // without dispatching — and the didChange queued AFTER it still mirrored in
    // order (a hover fences the loop past the notification).
    let _ = client.result("textDocument/hover", at);
    assert_eq!(
        pc.calls()
            .iter()
            .filter(|c| c.starts_with("completion"))
            .count(),
        1,
        "the cancelled completion must never reach the PC: {:?}",
        pc.calls()
    );
    assert_eq!(
        pc.mirrored_text(&uri).as_deref(),
        Some(retagged.as_str()),
        "the notification behind the cancelled request still applied in order"
    );
    client.shutdown();
}

// The `scala3SemanticLs.pcPluginStatus` executeCommand round-trips through the
// REAL serve loop to the injected PC service's plugin report: the text summary
// by default, the structured `{compilerPlugins, servicePlugins, disabled}`
// object with the doctor's `{"json": true}` argument convention — JVM-free.
#[test]
fn the_pc_plugin_status_command_round_trips_over_the_wire() {
    let (mut client, pc) = boot();
    client.initialize();
    client.await_ready();

    let text = client.result(
        "workspace/executeCommand",
        json!({ "command": "scala3SemanticLs.pcPluginStatus" }),
    );
    let text = text.as_str().expect("a text summary");
    assert!(text.contains("compiler plugins: 1"), "{text}");
    assert!(
        text.contains("  /plugins/fake-plugin.jar: loaded"),
        "{text}"
    );
    assert!(
        text.contains("  fake.nav (builtin): enabled, self-test ok"),
        "{text}"
    );
    assert!(text.contains("disabled plugins: 1"), "{text}");
    assert!(
        text.contains("  fake.disabled: disabled by config"),
        "{text}"
    );

    let report = client.result(
        "workspace/executeCommand",
        json!({
            "command": "scala3SemanticLs.pcPluginStatus",
            "arguments": [{ "json": true }]
        }),
    );
    assert_eq!(
        report["compilerPlugins"][0]["jars"][0],
        "/plugins/fake-plugin.jar"
    );
    assert_eq!(report["compilerPlugins"][0]["loaded"], true);
    assert_eq!(report["servicePlugins"][0]["id"], "fake.nav");
    assert_eq!(report["servicePlugins"][0]["selfTestOk"], true);
    assert_eq!(report["disabled"][0]["reason"], "disabled by config");

    assert!(
        pc.calls().iter().any(|c| c == "plugin_status"),
        "the command must reach the PC service's plugin_status: {:?}",
        pc.calls()
    );
    assert!(
        !ls_server::libjvm_mapped(),
        "the pcPluginStatus round-trip must stay JVM-free with the injected fake"
    );
    client.shutdown();
}
