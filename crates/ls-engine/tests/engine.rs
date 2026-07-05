//! Behavior tests for the ingest pipeline, orchestrator, and engines over
//! fully controlled synthetic workspaces. (The exhaustive §18.1 correctness
//! matrix over pinned-scalac fixtures is a separate suite.)

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::*;
use ls_engine::{
    current_thread_label, identifiers, symbol_encoding, QueryOrchestrator, ReferencesEngine,
    RenameEngine, ResolutionSource, TargetSpec, WorkspaceTargets,
};
use ls_index_model::{DocId, LsError, Role, Span, SymbolKey};
use ls_store::Store;

/// A two-file single-target workspace: `a/A.scala` defines `pkg/A#` +
/// `pkg/A#foo().` and references `scala/Int#`; `b/B.scala` references `pkg/A#`.
fn build_ab(dir: &TempDir) -> (Arc<WorkspaceTargets>, PathBuf, PathBuf) {
    let targetroot = dir.sub("target");
    let sourceroot = dir.sub("sources");
    let a = DocFixture::new("a/A.scala", "class A:\n  def foo: Int = 1\n")
        .symbol(sym("pkg/A#", KIND_CLASS, 0, "A"))
        .symbol(sym("pkg/A#foo().", KIND_METHOD, 0, "foo"))
        .occurrence(occ(rng(0, 6, 0, 7), "pkg/A#", DEFINITION))
        .occurrence(occ(rng(1, 6, 1, 9), "pkg/A#foo().", DEFINITION))
        .occurrence(occ(rng(1, 11, 1, 14), "scala/Int#", REFERENCE));
    let b = DocFixture::new("b/B.scala", "val a = new A\n").occurrence(occ(
        rng(0, 12, 0, 13),
        "pkg/A#",
        REFERENCE,
    ));
    write_doc(&targetroot, &sourceroot, &a);
    write_doc(&targetroot, &sourceroot, &b);
    let ws = WorkspaceTargets::new(vec![TargetSpec::new(
        "app",
        targetroot.clone(),
        sourceroot.clone(),
    )]);
    (Arc::new(ws), targetroot, sourceroot)
}

fn orchestrator(dir: &TempDir, ws: Arc<WorkspaceTargets>) -> QueryOrchestrator {
    let store = Store::open(&dir.sub("store")).unwrap();
    let orch = QueryOrchestrator::with_defaults(store);
    orch.ingest(ws).unwrap();
    orch
}

#[test]
fn ingest_report_counts() {
    let dir = TempDir::new("ingest");
    let (ws, _, _) = build_ab(&dir);
    let store = Store::open(&dir.sub("store")).unwrap();
    let orch = QueryOrchestrator::with_defaults(store);
    let report = orch.ingest(ws).unwrap();
    assert_eq!(report.docs_indexed, 2);
    assert_eq!(report.docs_shared, 0);
    assert_eq!(report.docs_stale, 0);
    assert_eq!(report.docs_skipped, 0);
    assert!(report.symbol_count >= 3, "A#, foo, Int at least");
}

#[test]
fn symbol_at_cursor_resolves_from_snapshot() {
    let dir = TempDir::new("cursor");
    let (ws, _, _) = build_ab(&dir);
    let orch = orchestrator(&dir, ws);
    let cur = orch.symbol_at_cursor("a/A.scala", 0, 6).unwrap();
    assert_eq!(cur.source, ResolutionSource::Snapshot);
    assert_eq!(cur.semantic_symbol, "pkg/A#");
    assert_eq!(cur.role, Role::Definition);
    assert!(!cur.needs_reindex);
}

#[test]
fn workspace_symbol_finds_class() {
    let dir = TempDir::new("wssym");
    let (ws, _, _) = build_ab(&dir);
    let orch = orchestrator(&dir, ws);
    let hits = orch.workspace_symbol("A", 10);
    assert!(hits.iter().any(|h| h.display == "A"));
    assert!(orch.workspace_symbol_name_exists("A"));
    assert!(!orch.workspace_symbol_name_exists("Nonexistent"));
}

#[test]
fn references_group_and_include_declaration() {
    let dir = TempDir::new("refs");
    let (ws, _, _) = build_ab(&dir);
    let orch = orchestrator(&dir, ws);
    let engine = ReferencesEngine::new(&orch);

    let refs = engine.references("b/B.scala", 0, 12, false).unwrap();
    assert_eq!(refs.hits.len(), 1);
    assert_eq!(refs.hits[0].loc.uri, "b/B.scala");
    assert_eq!(refs.hits[0].role, Role::Reference);
    assert!(!refs.needs_reindex);

    let refs2 = engine.references("b/B.scala", 0, 12, true).unwrap();
    assert_eq!(refs2.hits.len(), 2);
    assert_eq!(refs2.hits[0].loc.uri, "a/A.scala");
    assert_eq!(refs2.hits[0].role, Role::Definition);
    assert_eq!(refs2.hits[0].loc.span, Span::new(0, 6, 0, 7));
    assert_eq!(refs2.hits[1].loc.uri, "b/B.scala");
}

#[test]
fn references_reject_pc_only_symbol() {
    let dir = TempDir::new("pconly");
    let (ws, _, _) = build_ab(&dir);
    let store = Store::open(&dir.sub("store")).unwrap();
    let overlay = TestOverlay::dirty(
        "a/A.scala",
        Some(overlay_hit(
            "pkg/A#",
            Span::new(0, 6, 0, 7),
            Role::Definition,
            true,
        )),
    );
    let orch = QueryOrchestrator::new(store, Box::new(overlay), true);
    orch.ingest(ws).unwrap();
    let engine = ReferencesEngine::new(&orch);
    let err = engine.references("a/A.scala", 0, 6, false).unwrap_err();
    assert!(matches!(err, LsError::PcOnlySymbol));
}

#[test]
fn rename_safe_produces_edits() {
    let dir = TempDir::new("rename");
    let (ws, _, _) = build_ab(&dir);
    let orch = orchestrator(&dir, ws);
    let compiler = OkCompiler;
    let engine = RenameEngine::new(&orch, &compiler);
    let plan = engine.rename("a/A.scala", 0, 6, "Renamed").unwrap();
    assert_eq!(plan.occurrence_count, 2);
    let a_edits = plan.edits.get("a/A.scala").unwrap();
    assert_eq!(a_edits.len(), 1);
    assert_eq!(a_edits[0].span, Span::new(0, 6, 0, 7));
    assert_eq!(a_edits[0].new_text, "Renamed");
    let b_edits = plan.edits.get("b/B.scala").unwrap();
    assert_eq!(b_edits[0].span, Span::new(0, 12, 0, 13));
}

#[test]
fn rename_compile_failure_is_typed() {
    let dir = TempDir::new("compilefail");
    let (ws, _, _) = build_ab(&dir);
    let orch = orchestrator(&dir, ws);
    let compiler = FailCompiler;
    let engine = RenameEngine::new(&orch, &compiler);
    let err = engine.rename("a/A.scala", 0, 6, "Renamed").unwrap_err();
    assert!(matches!(err, LsError::CompileFailed { .. }));
}

#[test]
fn rename_external_symbol_rejected() {
    let dir = TempDir::new("external");
    let (ws, _, _) = build_ab(&dir);
    let orch = orchestrator(&dir, ws);
    let compiler = OkCompiler;
    let engine = RenameEngine::new(&orch, &compiler);
    // The `scala/Int#` reference at (1,11) has no workspace definition.
    let err = engine.rename("a/A.scala", 1, 11, "Renamed").unwrap_err();
    assert!(matches!(err, LsError::RenameRejected { .. }));
}

#[test]
fn rename_invalid_new_name_rejected() {
    let dir = TempDir::new("badname");
    let (ws, _, _) = build_ab(&dir);
    let orch = orchestrator(&dir, ws);
    let compiler = OkCompiler;
    let engine = RenameEngine::new(&orch, &compiler);
    let err = engine.rename("a/A.scala", 0, 6, "").unwrap_err();
    assert!(matches!(err, LsError::RenameRejected { .. }));
}

#[test]
fn raw_path_write_through_runs_inline_and_heals() {
    let dir = TempDir::new("writethrough");
    let (ws, targetroot, sourceroot) = build_ab(&dir);
    let orch = orchestrator(&dir, ws);
    // Add a doc not present in the published snapshot.
    let c = DocFixture::new("c/C.scala", "class C\n")
        .symbol(sym("pkg/C#", KIND_CLASS, 0, "C"))
        .occurrence(occ(rng(0, 6, 0, 7), "pkg/C#", DEFINITION));
    write_doc(&targetroot, &sourceroot, &c);

    let cur = orch.symbol_at_cursor("c/C.scala", 0, 6).unwrap();
    assert_eq!(cur.source, ResolutionSource::RawSemanticdb);
    assert!(!cur.needs_reindex, "write-through cleared needs_reindex");
    assert_eq!(
        orch.last_write_through_thread_name(),
        Some(current_thread_label()),
        "write-through ran inline on the calling thread"
    );

    // The healed snapshot now serves C from the index.
    let cur2 = orch.symbol_at_cursor("c/C.scala", 0, 6).unwrap();
    assert_eq!(cur2.source, ResolutionSource::Snapshot);
}

#[test]
fn dirty_cursor_without_semanticdb_match_degrades_to_stale() {
    let dir = TempDir::new("stale");
    let (ws, _targetroot, sourceroot) = build_ab(&dir);
    let orch = orchestrator(&dir, ws);
    // Edit the source so its md5 no longer matches the indexed SemanticDB.
    std::fs::write(
        sourceroot.join("a/A.scala"),
        "class Changed:\n  def foo: Int = 1\n",
    )
    .unwrap();
    let err = orch.symbol_at_cursor("a/A.scala", 0, 6).unwrap_err();
    assert!(matches!(err, LsError::StaleIndex { .. }));
}

#[test]
fn shared_source_is_indexed_once() {
    let dir = TempDir::new("shared");
    let t0 = dir.sub("t0");
    let t1 = dir.sub("t1");
    let src = dir.sub("src");
    let s = DocFixture::new("s/S.scala", "class S\n")
        .symbol(sym("pkg/S#", KIND_CLASS, 0, "S"))
        .occurrence(occ(rng(0, 6, 0, 7), "pkg/S#", DEFINITION));
    write_doc(&t0, &src, &s);
    write_doc(&t1, &src, &s);
    let ws = WorkspaceTargets::new(vec![
        TargetSpec::new("t0", t0, src.clone()),
        TargetSpec::new("t1", t1, src),
    ]);
    let store = Store::open(&dir.sub("store")).unwrap();
    let orch = QueryOrchestrator::with_defaults(store);
    let report = orch.ingest(Arc::new(ws)).unwrap();
    assert_eq!(report.docs_indexed, 1);
    assert_eq!(report.docs_shared, 1);
}

#[test]
fn epoch_bumps_when_md5_changes() {
    let dir = TempDir::new("epoch");
    let (ws, targetroot, sourceroot) = build_ab(&dir);
    let orch = orchestrator(&dir, ws);

    let snap1 = orch.current_snapshot().unwrap();
    let a1 = doc_ord_of(&snap1, "a/A.scala");
    assert_eq!(snap1.segment().epoch_of(a1), 1);

    // Recompile A with different content (new md5).
    let a2 = DocFixture::new("a/A.scala", "class A:\n  def foo: Int = 2\n")
        .symbol(sym("pkg/A#", KIND_CLASS, 0, "A"))
        .symbol(sym("pkg/A#foo().", KIND_METHOD, 0, "foo"))
        .occurrence(occ(rng(0, 6, 0, 7), "pkg/A#", DEFINITION))
        .occurrence(occ(rng(1, 6, 1, 9), "pkg/A#foo().", DEFINITION))
        .occurrence(occ(rng(1, 11, 1, 14), "scala/Int#", REFERENCE));
    write_doc(&targetroot, &sourceroot, &a2);
    orch.ingest(orch.workspace().unwrap()).unwrap();

    let snap2 = orch.current_snapshot().unwrap();
    let a2ord = doc_ord_of(&snap2, "a/A.scala");
    assert_eq!(snap2.segment().epoch_of(a2ord), 2);
}

#[test]
fn overlay_dirty_resolves_from_overlay() {
    let dir = TempDir::new("overlay");
    let (ws, _, _) = build_ab(&dir);
    let store = Store::open(&dir.sub("store")).unwrap();
    let overlay = TestOverlay::dirty(
        "a/A.scala",
        Some(overlay_hit(
            "pkg/A#",
            Span::new(0, 6, 0, 7),
            Role::Definition,
            false,
        )),
    );
    let orch = QueryOrchestrator::new(store, Box::new(overlay), true);
    orch.ingest(ws).unwrap();
    let cur = orch.symbol_at_cursor("a/A.scala", 0, 6).unwrap();
    assert_eq!(cur.source, ResolutionSource::Overlay);
    assert_eq!(cur.semantic_symbol, "pkg/A#");
}

#[test]
fn overlay_dirty_without_hit_degrades() {
    let dir = TempDir::new("overlaynohit");
    let (ws, _, _) = build_ab(&dir);
    let store = Store::open(&dir.sub("store")).unwrap();
    let overlay = TestOverlay::dirty("a/A.scala", None);
    let orch = QueryOrchestrator::new(store, Box::new(overlay), true);
    orch.ingest(ws).unwrap();
    let err = orch.symbol_at_cursor("a/A.scala", 0, 6).unwrap_err();
    assert!(matches!(err, LsError::StaleIndex { .. }));
}

#[test]
fn identifier_encoding_matches_scala_rules() {
    assert_eq!(identifiers::encode("plain"), Ok("plain".to_string()));
    assert_eq!(identifiers::encode("type"), Ok("`type`".to_string()));
    assert_eq!(
        identifiers::encode("has space"),
        Ok("`has space`".to_string())
    );
    assert!(identifiers::encode("").is_err());
    assert!(identifiers::encode("has`backtick").is_err());
    assert_eq!(identifiers::encode("+"), Ok("+".to_string()));
}

#[test]
fn symbol_encoding_round_trips_locals() {
    let key = SymbolKey::local("local3", DocId::new(42));
    let encoded = symbol_encoding::encode_key(&key);
    assert_eq!(encoded, "local3@42");
    assert_eq!(
        symbol_encoding::decode(&encoded),
        ("local3".to_string(), Some(42))
    );
    assert_eq!(symbol_encoding::to_key(&encoded), key);

    let global = SymbolKey::global("pkg/A#");
    assert_eq!(symbol_encoding::encode_key(&global), "pkg/A#");
    assert_eq!(
        symbol_encoding::decode("pkg/A#"),
        ("pkg/A#".to_string(), None)
    );
}

fn doc_ord_of(snap: &ls_store::Snapshot, uri: &str) -> u32 {
    (0..snap.segment().doc_count())
        .find(|&d| snap.segment().uri_of(d) == uri)
        .unwrap()
}
