//! Real-BSP end-to-end (index + BSP rows): the whole server driven over the
//! framed LSP wire against a REAL mill build server built from
//! `it/sample-workspace`, through PRODUCTION discovery + launch (a real
//! `.bsp/mill-bsp.json` from `mill mill.bsp.BSP/install`, model load over the
//! production `LiveBspModelSource`, the retained session-backed compiler). A port
//! of the index/BSP half of the Scala `RealBsp*` suites. The interactive harness
//! lives in [`real_bsp_common`].
//!
//! Gated on `LS_REAL_BSP_IT=1` (mill needs a JVM/toolchain the hermetic Nix check
//! forbids), so every scenario skips cleanly without it; the live path runs via
//! `scripts/it-real-bsp-rs.sh`.
//!
//! The presentation-compiler rows live in SEPARATE integration-test binaries
//! (`real_bsp_pc` for hover/signatureHelp/definition + dirty completion,
//! `real_bsp_pc_recovery` for the faulted dispatch-generation recovery) because
//! only one embedded JVM/island can boot per process — keeping this binary
//! JVM-free makes the cold-start zero-JVM assertion below unconditionally sound.
//!
//! Coverage here: doctor names mill-bsp + flags the no-SemanticDB module; the
//! compile fills the index; workspace/symbol; the cross-module reference set;
//! rename edits every compiled module; SemanticDB-mandatory hard errors; rename
//! rejection reasons; index documentHighlight; compile-error diagnostic
//! publish+clear; save-driven reingest; shared-source references + rename; the
//! repeated-save segment-hygiene invariant; and the cold-start zero-JVM property.
//!
//! The no-BSP warm-restart-from-recovery half of the Scala repeated-save scenario
//! is not ported: the no-BSP warm restart is the trimmed mode carried as a
//! recorded deferral, so there is no recovered-index restart path to exercise. The
//! ahead-of-time-trained boot scenario is out of scope for this rewrite.

mod real_bsp_common;

use std::thread;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use ls_bsp::uri::path_to_uri;
use real_bsp_common::*;

// --- scenarios: index + BSP (mill only) ---------------------------------------

#[test]
fn real_bsp_doctor_symbol_references_and_rename_over_live_mill() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let root_uri = path_to_uri(&ws);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();

    // The doctor names the real mill server, reports ready, sees the Scala 3
    // targets, and flags the no-SemanticDB module + mill's own build target as a
    // hard SemanticDB-coverage error — while the indexable module a stays clean.
    let report = server.ready();
    assert!(report.contains("server: mill-bsp"), "{report}");
    assert!(report.contains(&format!("{root_uri}/a")), "{report}");
    assert!(report.contains(&format!("{root_uri}/b")), "{report}");
    let coverage = report
        .lines()
        .find(|l| l.contains("SemanticDB coverage:"))
        .unwrap_or("");
    assert!(
        coverage.contains("ERROR"),
        "expected a SemanticDB error: {report}"
    );
    assert!(coverage.contains(&format!("{root_uri}/c")), "{report}");
    assert!(
        !coverage.contains(&format!("{root_uri}/a")),
        "module a must be indexable: {report}"
    );

    // workspace/symbol finds the class declared in module a.
    let symbols = server.result("workspace/symbol", json!({"query": "Greeting"}));
    let greeting_uri = server.file_uri(GREETING);
    let found = symbols.as_array().unwrap().iter().any(|s| {
        s.get("name").and_then(Value::as_str) == Some("Greeting")
            && s.pointer("/location/uri").and_then(Value::as_str) == Some(greeting_uri.as_str())
    });
    assert!(found, "Greeting not found in module a: {symbols}");

    // references on a usage in b returns the exact cross-module, cross-file set:
    // every `message` occurrence across the indexed sources.
    let consumer_text = source_text(&ws, CONSUMER);
    let (line, character) = position_of(&consumer_text, "message", 0);
    let locations = server.result(
        "textDocument/references",
        json!({
            "textDocument": server.text_doc(CONSUMER),
            "position": position_json(line, character),
            "context": {"includeDeclaration": true},
        }),
    );
    let locs = locations.as_array().unwrap();
    let expected = count_token(&ws, &INDEXED, "message");
    assert_eq!(
        locs.len(),
        expected,
        "cross-module message set: {locations}"
    );
    let mut expected_set: Vec<(String, Value)> = INDEXED
        .iter()
        .flat_map(|rel| {
            let text = source_text(&ws, rel);
            let uri = server.file_uri(rel);
            (0..text.matches("message").count())
                .map(move |n| (uri.clone(), span_of(&text, "message", n)))
        })
        .collect();
    for loc in locs {
        let uri = loc.get("uri").and_then(Value::as_str).unwrap().to_string();
        let range = loc.get("range").cloned().unwrap();
        let pos = expected_set
            .iter()
            .position(|(u, r)| *u == uri && *r == range)
            .unwrap_or_else(|| panic!("unexpected reference {loc}"));
        expected_set.remove(pos);
    }
    assert!(
        expected_set.is_empty(),
        "missing references: {expected_set:?}"
    );

    // rename compiles through the real BSP server and edits every indexed module.
    let edit = server.result(
        "textDocument/rename",
        json!({
            "textDocument": server.text_doc(CONSUMER),
            "position": position_json(line, character),
            "newName": "note",
        }),
    );
    let changes = edit.get("changes").and_then(Value::as_object).unwrap();
    for rel in INDEXED {
        let uri = server.file_uri(rel);
        let text = source_text(&ws, rel);
        let edits = changes
            .get(&uri)
            .and_then(Value::as_array)
            .unwrap_or_else(|| panic!("rename should edit {rel}: {edit}"));
        assert_eq!(
            edits.len(),
            text.matches("message").count(),
            "{rel}: {edits:?}"
        );
        for e in edits {
            assert_eq!(
                e.get("newText").and_then(Value::as_str),
                Some("note"),
                "{rel}"
            );
        }
    }

    server.shutdown();
}

#[test]
fn real_bsp_semanticdb_is_mandatory_on_the_uncovered_module() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.ready();

    let widget_text = source_text(&ws, WIDGET);
    let (line, character) = position_of(&widget_text, "area", 0);
    let pos = position_json(line, character);

    // The no-SemanticDB module is a hard error on both the PC and index paths —
    // never a quiet fallback nor an empty result.
    let completion = server.error_message(
        "textDocument/completion",
        json!({"textDocument": server.text_doc(WIDGET), "position": pos}),
    );
    assert!(
        completion.contains("has no SemanticDB output"),
        "{completion}"
    );
    assert!(completion.contains("-Xsemanticdb"), "{completion}");

    let highlight = server.error_message(
        "textDocument/documentHighlight",
        json!({"textDocument": server.text_doc(WIDGET), "position": pos}),
    );
    assert!(
        highlight.contains("has no SemanticDB output"),
        "{highlight}"
    );

    let rename = server.error_message(
        "textDocument/rename",
        json!({"textDocument": server.text_doc(WIDGET), "position": pos, "newName": "surface"}),
    );
    assert!(rename.contains("has no SemanticDB output"), "{rename}");

    server.shutdown();
}

#[test]
fn real_bsp_rename_rejections_carry_the_typed_reason() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.ready();

    let greeting_text = source_text(&ws, GREETING);

    // An external/library symbol (`String` in the constructor) is outside the
    // workspace and is rejected after the fresh compile+ingest.
    let (sl, sc) = position_of(&greeting_text, "String", 0);
    let external = server.error_message(
        "textDocument/rename",
        json!({"textDocument": server.text_doc(GREETING), "position": position_json(sl, sc), "newName": "Str"}),
    );
    assert!(external.contains("rename rejected"), "{external}");
    assert!(external.contains("outside the workspace"), "{external}");

    // A cursor inside the string literal has no symbol occurrence.
    let (wl, wc) = position_of(&greeting_text, "\"world\"", 0);
    let no_symbol = server.error_message(
        "textDocument/rename",
        json!({"textDocument": server.text_doc(GREETING), "position": position_json(wl, wc + 3), "newName": "planet"}),
    );
    assert!(no_symbol.contains("no symbol occurrence"), "{no_symbol}");

    server.shutdown();
}

#[test]
fn real_bsp_documenthighlight_returns_in_file_occurrences() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.ready();

    // documentHighlight is served from the index — both `name` occurrences in
    // Greeting.scala, and nothing from other files.
    let greeting_text = source_text(&ws, GREETING);
    let (line, character) = position_of(&greeting_text, "name", 0);
    let highlights = server.result(
        "textDocument/documentHighlight",
        json!({"textDocument": server.text_doc(GREETING), "position": position_json(line, character)}),
    );
    let spans: Vec<Value> = highlights
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h.get("range").cloned().unwrap())
        .collect();
    let expected = greeting_text.matches("name").count();
    assert_eq!(
        spans.len(),
        expected,
        "in-file name occurrences: {highlights}"
    );

    server.shutdown();
}

#[test]
fn real_bsp_compile_error_is_forwarded_then_cleared_by_the_fix() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.ready();

    let original = source_text(&ws, CONSUMER);
    // `message` is a String, so re-typing `text` as Int fails to compile.
    let broken = original.replace("val text: String =", "val text: Int =");
    assert_ne!(broken, original, "fixture text changed; update the edit");

    server.save(CONSUMER, &broken);
    let errors = server.await_publish(
        CONSUMER,
        |diags| {
            diags
                .iter()
                .any(|d| d.get("severity").and_then(Value::as_i64) == Some(1))
        },
        "an error diagnostic on the broken save",
    );
    assert!(
        errors
            .iter()
            .any(|d| d.get("severity").and_then(Value::as_i64) == Some(1)),
        "expected an error-severity diagnostic: {errors:?}"
    );

    // Fix and save — the file republishes an error-free diagnostic list.
    server.save(CONSUMER, &original);
    server.await_publish(
        CONSUMER,
        |diags| {
            diags
                .iter()
                .all(|d| d.get("severity").and_then(Value::as_i64) != Some(1))
        },
        "the cleared diagnostics after the fix",
    );

    server.shutdown();
}

#[test]
fn real_bsp_save_driven_reingest_reflects_new_token_positions() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.ready();

    let original = source_text(&ws, CONSUMER);
    // Insert a pad line ABOVE the usage, shifting `greeting.message` one line down.
    // The pad line must not contain `message`, or the span search would match it.
    let moved = original.replace(
        "  val text: String = greeting.message",
        "  // pad line to shift the usage down\n  val text: String = greeting.message",
    );
    assert_ne!(moved, original, "fixture text changed; update the edit");
    let old_span = span_of(&original, "message", 0);
    let new_span = span_of(&moved, "message", 0);
    assert_ne!(old_span, new_span, "the edit must shift the token");

    server.save(CONSUMER, &moved);
    let consumer_uri = server.file_uri(CONSUMER);
    let (line, character) = position_of(&moved, "message", 0);
    let query = json!({
        "textDocument": server.text_doc(CONSUMER),
        "position": position_json(line, character),
        "context": {"includeDeclaration": true},
    });

    // The debounced pipeline compiles then re-ingests with NO explicit reindex;
    // poll references (the wire-observable effect) until the moved span appears.
    // Until the background compile+reingest lands, references over the just-saved
    // buffer is transiently a hard `StaleIndex` error (its inline write-through
    // path cannot heal a moved token without a compile) — tolerate that and keep
    // polling; once the reingest commits, references answers with the moved span.
    let deadline = Instant::now() + Duration::from_secs(180);
    loop {
        let response = server.request("textDocument/references", query.clone());
        if let Some(locations) = response.get("result").and_then(Value::as_array) {
            let here: Vec<Value> = locations
                .iter()
                .filter(|l| l.get("uri").and_then(Value::as_str) == Some(consumer_uri.as_str()))
                .map(|l| l.get("range").cloned().unwrap())
                .collect();
            if here.contains(&new_span) {
                assert!(!here.contains(&old_span), "stale span survived: {here:?}");
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "reingest never reflected the moved span"
        );
        thread::sleep(Duration::from_millis(250));
    }

    server.shutdown();
}

#[test]
fn real_bsp_shared_source_unifies_references_and_passes_rename_consistency() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let shared_rel = "shared/src/pkgshared/Shared.scala";
    let shared_source =
        "package pkgshared\n\nobject Shared:\n  def marker: String = \"shared-marker\"\n";
    // A build where a and d BOTH compile shared/src, so Shared.scala is a source
    // shared across two targets and the index holds two documents for its uri.
    let shared_build = r#"//| mill-version: 1.1.2
//| mill-jvm-version: system
package build

import mill.*
import mill.scalalib.*

trait SampleModule extends ScalaModule {
  def scalaVersion = "3.8.4"
  def scalacOptions = Seq(
    "-Xsemanticdb",
    "-sourceroot",
    mill.api.BuildCtx.workspaceRoot.toString
  )
}

object a extends SampleModule {
  def sources = Task.Sources(
    mill.api.BuildCtx.workspaceRoot / "a" / "src",
    mill.api.BuildCtx.workspaceRoot / "shared" / "src"
  )
}

object d extends SampleModule {
  def sources = Task.Sources(mill.api.BuildCtx.workspaceRoot / "shared" / "src")
}

object b extends SampleModule {
  def moduleDeps = Seq(a)
}

object c extends ScalaModule {
  def scalaVersion = "3.8.4"
}
"#;
    let (tmp, ws) = prepare_workspace(&[(shared_rel, shared_source)], Some(shared_build));
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.ready();

    let shared_text = source_text(&ws, shared_rel);
    let shared_uri = server.file_uri(shared_rel);
    let (line, character) = position_of(&shared_text, "marker", 0);
    let marker_span = span_of(&shared_text, "marker", 0);

    // references on the shared symbol unify to ONE location for the shared uri,
    // not one per compiling target.
    let locations = server.result(
        "textDocument/references",
        json!({
            "textDocument": server.text_doc(shared_rel),
            "position": position_json(line, character),
            "context": {"includeDeclaration": true},
        }),
    );
    let here: Vec<Value> = locations
        .as_array()
        .unwrap()
        .iter()
        .filter(|l| {
            l.get("uri").and_then(Value::as_str) == Some(shared_uri.as_str())
                && l.get("range") == Some(&marker_span)
        })
        .cloned()
        .collect();
    assert_eq!(here.len(), 1, "shared occurrence must unify: {locations}");

    // rename runs the shared-source consistency check across both target views;
    // the views agree, so it succeeds and edits the shared file.
    let edit = server.result(
        "textDocument/rename",
        json!({
            "textDocument": server.text_doc(shared_rel),
            "position": position_json(line, character),
            "newName": "flag",
        }),
    );
    let edits = edit
        .pointer("/changes")
        .and_then(Value::as_object)
        .and_then(|c| c.get(&shared_uri))
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("rename should edit the shared source: {edit}"));
    assert!(
        edits.iter().any(|e| e.get("range") == Some(&marker_span)),
        "{edits:?}"
    );
    assert!(
        edits
            .iter()
            .all(|e| e.get("newText").and_then(Value::as_str) == Some("flag")),
        "{edits:?}"
    );

    server.shutdown();
}

/// The committed segment id the doctor's `Store` manifest line reports.
fn committed_segment(server: &mut RealServer) -> u64 {
    let report = server.execute_command(DOCTOR);
    report
        .lines()
        .find_map(|l| l.trim().strip_prefix("manifest: schema v1, segment "))
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("no manifest segment in doctor:\n{report}"))
}

#[test]
fn real_bsp_repeated_saves_keep_a_single_committed_segment_dir() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.ready();

    // >= 3 didSave -> compile -> reingest cycles (toggle a trailing comment),
    // waiting for each save-driven reingest to commit a fresh segment generation.
    let original = source_text(&ws, OTHER);
    for i in 1..=3 {
        let before = committed_segment(&mut server);
        let text = source_text(&ws, OTHER);
        let edited = if text.ends_with("// hygiene mark\n") {
            original.clone()
        } else {
            format!("{text}// hygiene mark\n")
        };
        server.save(OTHER, &edited);
        let deadline = Instant::now() + Duration::from_secs(180);
        loop {
            if committed_segment(&mut server) > before {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "cycle {i}: the save-driven reingest never committed a new segment"
            );
            thread::sleep(Duration::from_millis(250));
        }
    }
    server.save(OTHER, &original);

    // Segment hygiene: the superseded generations are janitor-deleted once their
    // snapshots drop, so exactly one committed `segment-*` dir remains — and the
    // manifest + workspace-state stay readable through the doctor Store section.
    let segments_dir = ws.join(".scala3-bsp-semantic-ls").join("segments");
    let segment_dirs: Vec<_> = std::fs::read_dir(&segments_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("segment-"))
        .map(|e| e.file_name())
        .collect();
    assert_eq!(
        segment_dirs.len(),
        1,
        "expected exactly one committed segment dir in {segments_dir:?}, got {segment_dirs:?}"
    );
    let report = server.execute_command(DOCTOR);
    assert!(
        report.contains("manifest: schema v1"),
        "manifest unreadable in doctor:\n{report}"
    );
    assert!(
        report.contains("workspace-state: generation"),
        "workspace-state unreadable in doctor:\n{report}"
    );

    server.shutdown();
}

// --- cold start: index-only queries leave the process JVM-free ----------------

#[test]
fn real_bsp_cold_start_serves_index_queries_with_no_jvm() {
    if !mill_enabled() {
        return skip("real_bsp: skipping — set LS_REAL_BSP_IT=1 to run the real-mill e2e");
    }
    // This binary has no presentation-compiler scenario (they live in
    // `real_bsp_pc`/`real_bsp_pc_recovery`), so nothing here boots the embedded
    // island — `libjvm_mapped()` (a process-global `/proc/self/maps` read) is an
    // unconditionally sound cold-start assertion over the live mill model.
    let (tmp, ws) = prepare_workspace(&[], None);
    let mut server = RealServer::boot(tmp, ws.clone());
    server.initialize();
    server.ready();

    // Index-only queries (symbol + references) over the live mill model, and NO
    // presentation-compiler request — the embedded island must stay unbooted.
    server.result("workspace/symbol", json!({"query": "Greeting"}));
    let consumer_text = source_text(&ws, CONSUMER);
    let (line, character) = position_of(&consumer_text, "message", 0);
    server.result(
        "textDocument/references",
        json!({
            "textDocument": server.text_doc(CONSUMER),
            "position": position_json(line, character),
            "context": {"includeDeclaration": true},
        }),
    );
    assert!(
        !ls_server::libjvm_mapped(),
        "an index-only session over live mill must not map libjvm"
    );

    server.shutdown();
    assert!(
        !ls_server::libjvm_mapped(),
        "no PC request ran, so the JVM must never have booted"
    );
}
