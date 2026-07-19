//! Behavior tests for the ingest pipeline, orchestrator, and engines over
//! fully controlled synthetic workspaces. (The exhaustive §18.1 correctness
//! matrix over pinned-scalac fixtures is a separate suite.)

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::*;
use ls_engine::{
    current_thread_label, identifiers, symbol_encoding, NoopOverlay, QueryOrchestrator,
    ReferencesEngine, RenameEngine, ResolutionSource, TargetSpec, WorkspaceTargets,
};
use ls_index_model::uri::path_to_uri;
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
    // The production wiring (`with_defaults`) heals the raw path inline; the rest
    // of this test proves that mode clears needs_reindex and heals the snapshot.
    assert!(
        orch.raw_path_writes_through(),
        "with_defaults must write through synchronously (write-through parity)"
    );
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

/// A three-target graph: `core` defines `pkg/S#`; `app` depends on `core` and
/// references it; `other` is disconnected but reuses the symbol string.
fn build_prune_workspace(dir: &TempDir) -> Arc<WorkspaceTargets> {
    let core_t = dir.sub("coretarget");
    let core_s = dir.sub("coresrc");
    let app_t = dir.sub("apptarget");
    let app_s = dir.sub("appsrc");
    let other_t = dir.sub("othertarget");
    let other_s = dir.sub("othersrc");
    write_doc(
        &core_t,
        &core_s,
        &DocFixture::new("c/S.scala", "class S\n")
            .symbol(sym("pkg/S#", KIND_CLASS, 0, "S"))
            .occurrence(occ(rng(0, 6, 0, 7), "pkg/S#", DEFINITION)),
    );
    write_doc(
        &app_t,
        &app_s,
        &DocFixture::new("a/A.scala", "val a = new S\n").occurrence(occ(
            rng(0, 12, 0, 13),
            "pkg/S#",
            REFERENCE,
        )),
    );
    write_doc(
        &other_t,
        &other_s,
        &DocFixture::new("o/O.scala", "val o = new S\n").occurrence(occ(
            rng(0, 12, 0, 13),
            "pkg/S#",
            REFERENCE,
        )),
    );
    Arc::new(WorkspaceTargets::new(vec![
        TargetSpec::new("core", core_t, core_s),
        TargetSpec::new("app", app_t, app_s).with_deps(vec!["core".to_string()]),
        TargetSpec::new("other", other_t, other_s),
    ]))
}

/// `core` defines `pkg/S#`; `app` depends on `core` and references it; `dup` is
/// disconnected and ALSO defines `pkg/S#` (the same symbol string, a different
/// module) — the go-to-definition forward-closure pruning fixture.
fn build_def_workspace(dir: &TempDir) -> (Arc<WorkspaceTargets>, PathBuf, PathBuf, PathBuf) {
    let core_t = dir.sub("coretarget");
    let core_s = dir.sub("coresrc");
    let app_t = dir.sub("apptarget");
    let app_s = dir.sub("appsrc");
    let dup_t = dir.sub("duptarget");
    let dup_s = dir.sub("dupsrc");
    write_doc(
        &core_t,
        &core_s,
        &DocFixture::new("c/S.scala", "class S\n")
            .symbol(sym("pkg/S#", KIND_CLASS, 0, "S"))
            .occurrence(occ(rng(0, 6, 0, 7), "pkg/S#", DEFINITION)),
    );
    write_doc(
        &app_t,
        &app_s,
        &DocFixture::new("a/A.scala", "val a = new S\n").occurrence(occ(
            rng(0, 12, 0, 13),
            "pkg/S#",
            REFERENCE,
        )),
    );
    write_doc(
        &dup_t,
        &dup_s,
        &DocFixture::new("d/S.scala", "class S\n")
            .symbol(sym("pkg/S#", KIND_CLASS, 0, "S"))
            .occurrence(occ(rng(0, 6, 0, 7), "pkg/S#", DEFINITION)),
    );
    let ws = WorkspaceTargets::new(vec![
        TargetSpec::new("core", core_t, core_s.clone()),
        TargetSpec::new("app", app_t, app_s.clone()).with_deps(vec!["core".to_string()]),
        TargetSpec::new("dup", dup_t, dup_s.clone()),
    ]);
    (Arc::new(ws), core_s, app_s, dup_s)
}

/// The canonical (percent-encoded) `file://` uri of `rel` under `sourceroot`,
/// matching the resolver's absolutization exactly.
fn file_uri(sourceroot: &std::path::Path, rel: &str) -> String {
    path_to_uri(&sourceroot.join(rel))
}

#[test]
fn symbol_definition_reaches_visible_target_and_prunes_disconnected_duplicate() {
    let dir = TempDir::new("symdef");
    let (ws, core_s, app_s, _dup_s) = build_def_workspace(&dir);
    let orch = orchestrator(&dir, ws);
    // From `app` (which depends on `core`), go-to-definition of `pkg/S#`
    // resolves to core's definition only — never the disconnected `dup`
    // target's duplicate of the same symbol string.
    let locs = orch.symbol_definition("pkg/S#", &file_uri(&app_s, "a/A.scala"));
    assert_eq!(locs.len(), 1, "exactly the visible (core) definition");
    assert_eq!(locs[0].uri, file_uri(&core_s, "c/S.scala"));
    assert_eq!(locs[0].span, Span::new(0, 6, 0, 7));
}

#[test]
fn symbol_definition_disconnected_buffer_sees_only_its_own_definition() {
    let dir = TempDir::new("symdefdup");
    let (ws, _core_s, _app_s, dup_s) = build_def_workspace(&dir);
    let orch = orchestrator(&dir, ws);
    // `dup` depends on nothing, so its forward closure is just itself.
    let locs = orch.symbol_definition("pkg/S#", &file_uri(&dup_s, "d/S.scala"));
    assert_eq!(locs.len(), 1);
    assert_eq!(locs[0].uri, file_uri(&dup_s, "d/S.scala"));
}

#[test]
fn symbol_definition_unscoped_buffer_sees_all_definitions() {
    let dir = TempDir::new("symdefunscoped");
    let (ws, core_s, _app_s, dup_s) = build_def_workspace(&dir);
    let orch = orchestrator(&dir, ws);
    // A buffer outside every sourceroot is unscoped: the closure gate is what
    // prunes, so with no owning target BOTH duplicate definitions surface.
    let locs = orch.symbol_definition("pkg/S#", "file:///nowhere/X.scala");
    assert_eq!(locs.len(), 2, "unscoped sees both duplicate definitions");
    let uris: Vec<&str> = locs.iter().map(|l| l.uri.as_str()).collect();
    assert!(uris.contains(&file_uri(&core_s, "c/S.scala").as_str()));
    assert!(uris.contains(&file_uri(&dup_s, "d/S.scala").as_str()));
}

#[test]
fn symbol_definition_unknown_and_empty_symbols_answer_empty() {
    let dir = TempDir::new("symdefunknown");
    let (ws, _core_s, app_s, _dup_s) = build_def_workspace(&dir);
    let orch = orchestrator(&dir, ws);
    let from = file_uri(&app_s, "a/A.scala");
    assert!(orch.symbol_definition("pkg/Nope#", &from).is_empty());
    assert!(orch.symbol_definition("", &from).is_empty());
}

#[test]
fn symbol_definition_prunes_with_a_percent_encoded_sourceroot() {
    // The temp dir name carries spaces, so every sourceroot does too, and the PC
    // passes a percent-encoded `file://` uri. Before the uri was decoded, the
    // encoded `from_uri` failed the sourceroot match, the request went unscoped,
    // and the disconnected `dup` duplicate leaked.
    let dir = TempDir::new("sym def space");
    let (ws, core_s, app_s, _dup_s) = build_def_workspace(&dir);
    let orch = orchestrator(&dir, ws);
    let from = file_uri(&app_s, "a/A.scala");
    assert!(
        from.contains("%20"),
        "spaced sourceroot uri must be encoded: {from}"
    );
    let locs = orch.symbol_definition("pkg/S#", &from);
    assert_eq!(
        locs.len(),
        1,
        "the encoded from_uri must still prune to the visible (core) def"
    );
    assert_eq!(locs[0].uri, file_uri(&core_s, "c/S.scala"));
    assert!(
        locs[0].uri.contains("%20"),
        "the result uri must be percent-encoded: {}",
        locs[0].uri
    );
}

#[test]
fn symbol_definition_prunes_with_single_slash_file_uri() {
    let dir = TempDir::new("symdefslash");
    let (ws, core_s, app_s, _dup_s) = build_def_workspace(&dir);
    let orch = orchestrator(&dir, ws);
    // Some clients spell the buffer uri with a single slash (`file:/…`) instead
    // of `file:///…`; it must still resolve to the owning target and prune the
    // disconnected duplicate rather than falling through to an unscoped lookup.
    let single = file_uri(&app_s, "a/A.scala").replacen("file://", "file:", 1);
    assert!(
        single.starts_with("file:/") && !single.starts_with("file://"),
        "expected a single-slash file uri: {single}"
    );
    let locs = orch.symbol_definition("pkg/S#", &single);
    assert_eq!(
        locs.len(),
        1,
        "single-slash from_uri must still prune to the visible (core) def"
    );
    assert_eq!(locs[0].uri, file_uri(&core_s, "c/S.scala"));
}

#[test]
fn symbol_definition_normalizes_dotdot_spellings_in_from_uri() {
    let dir = TempDir::new("symdefdots");
    let (ws, core_s, app_s, _dup_s) = build_def_workspace(&dir);
    let orch = orchestrator(&dir, ws);
    // A `..` that steps into a sibling name and back must normalize to the app
    // target — otherwise the raw prefix check misses and the request goes
    // unscoped (leaking the disconnected duplicate).
    let parent = app_s
        .parent()
        .unwrap()
        .to_str()
        .unwrap()
        .trim_end_matches('/');
    let app_name = app_s.file_name().unwrap().to_str().unwrap();
    let weird = format!("file://{parent}/nope/../{app_name}/a/A.scala");
    let locs = orch.symbol_definition("pkg/S#", &weird);
    assert_eq!(
        locs.len(),
        1,
        "a dot-dot spelling must still resolve + prune"
    );
    assert_eq!(locs[0].uri, file_uri(&core_s, "c/S.scala"));
}

#[test]
fn no_semanticdb_source_returns_hard_error() {
    let dir = TempDir::new("nosdb");
    // One real target so the segment is non-empty.
    let lib_t = dir.sub("libtarget");
    let lib_s = dir.sub("libsrc");
    write_doc(
        &lib_t,
        &lib_s,
        &DocFixture::new("l/L.scala", "class L\n")
            .symbol(sym("pkg/L#", KIND_CLASS, 0, "L"))
            .occurrence(occ(rng(0, 6, 0, 7), "pkg/L#", DEFINITION)),
    );
    // A target whose source exists but produced no SemanticDB.
    let app_t = dir.sub("apptarget");
    let app_s = dir.sub("appsrc");
    write_source_only(&app_s, "n/N.scala", "class N\n");
    let ws = WorkspaceTargets::new(vec![
        TargetSpec::new("lib", lib_t, lib_s),
        TargetSpec::new("app", app_t, app_s),
    ]);
    let store = Store::open(&dir.sub("store")).unwrap();
    let orch = QueryOrchestrator::with_defaults(store);
    orch.ingest(Arc::new(ws)).unwrap();

    assert!(matches!(
        orch.symbol_at_cursor("n/N.scala", 0, 6),
        Err(LsError::NoSemanticdb { .. })
    ));
    let refs = ReferencesEngine::new(&orch);
    assert!(matches!(
        refs.references("n/N.scala", 0, 6, false),
        Err(LsError::NoSemanticdb { .. })
    ));
    let compiler = OkCompiler;
    let rename = RenameEngine::new(&orch, &compiler);
    assert!(matches!(
        rename.prepare_rename("n/N.scala", 0, 6),
        Err(LsError::NoSemanticdb { .. })
    ));
    assert!(matches!(
        rename.rename("n/N.scala", 0, 6, "Renamed"),
        Err(LsError::NoSemanticdb { .. })
    ));
    // A uri outside every target sourceroot stays NotIndexed.
    assert!(matches!(
        orch.symbol_at_cursor("outside/Z.scala", 0, 0),
        Err(LsError::NotIndexed { .. })
    ));
}

#[test]
fn malformed_semanticdb_is_counted_and_ingest_continues() {
    let dir = TempDir::new("malformed");
    let targetroot = dir.sub("target");
    let sourceroot = dir.sub("sources");
    write_doc(
        &targetroot,
        &sourceroot,
        &DocFixture::new("a/A.scala", "class A\n")
            .symbol(sym("pkg/A#", KIND_CLASS, 0, "A"))
            .occurrence(occ(rng(0, 6, 0, 7), "pkg/A#", DEFINITION)),
    );
    write_corrupt(&targetroot, "z/Z.scala");
    let ws = WorkspaceTargets::new(vec![TargetSpec::new("app", targetroot, sourceroot)]);
    let store = Store::open(&dir.sub("store")).unwrap();
    let orch = QueryOrchestrator::with_defaults(store);
    let report = orch.ingest(Arc::new(ws)).unwrap();
    assert_eq!(report.docs_indexed, 1, "valid doc still indexed");
    assert_eq!(report.parse_errors.len(), 1, "corrupt file counted");
    assert!(report.parse_errors[0].file.contains("Z.scala"));
    assert!(!report.parse_errors[0].error.is_empty());
}

#[test]
fn references_pruned_to_reverse_dependency_closure() {
    let dir = TempDir::new("prune");
    let ws = build_prune_workspace(&dir);
    let orch = orchestrator(&dir, ws);
    let engine = ReferencesEngine::new(&orch);
    let result = engine.references("c/S.scala", 0, 6, false).unwrap();
    let uris: Vec<&str> = result.hits.iter().map(|h| h.loc.uri.as_str()).collect();
    assert_eq!(result.hits.len(), 1, "only the closure reference: {uris:?}");
    assert!(uris.contains(&"a/A.scala"), "app (dep of core) included");
    assert!(!uris.contains(&"o/O.scala"), "disconnected other pruned");
}

#[test]
fn compile_domain_is_reverse_dependency_closure() {
    let dir = TempDir::new("compiledomain");
    let ws = build_prune_workspace(&dir);
    let orch = orchestrator(&dir, ws);
    let compiler = RecordingCompiler::default();
    let engine = RenameEngine::new(&orch, &compiler);
    let _ = engine.rename("c/S.scala", 0, 6, "Renamed");
    assert_eq!(
        compiler.recorded(),
        Some(vec!["app".to_string(), "core".to_string()]),
        "compile domain is the sorted reverse-dependency closure of the def target"
    );
}

#[test]
fn references_raw_fallback_serves_same_doc() {
    let dir = TempDir::new("rawfallback");
    let (ws, targetroot, sourceroot) = build_ab(&dir);
    let store = Store::open(&dir.sub("store")).unwrap();
    let orch = QueryOrchestrator::new(store, Box::new(NoopOverlay), false); // write-through OFF
    orch.ingest(ws).unwrap();
    let c = DocFixture::new("c/C.scala", "class C:\n  def me: C = this\n")
        .symbol(sym("pkg/C#", KIND_CLASS, 0, "C"))
        .occurrence(occ(rng(0, 6, 0, 7), "pkg/C#", DEFINITION))
        .occurrence(occ(rng(1, 10, 1, 11), "pkg/C#", REFERENCE));
    write_doc(&targetroot, &sourceroot, &c);

    let engine = ReferencesEngine::new(&orch);
    let result = engine.references("c/C.scala", 0, 6, false).unwrap();
    assert!(result.needs_reindex, "raw path leaves needs_reindex set");
    assert_eq!(result.hits.len(), 1);
    assert_eq!(result.hits[0].loc.uri, "c/C.scala");
    assert_eq!(result.hits[0].loc.span, Span::new(1, 10, 1, 11));
}

// The background reindex scheduler heals via `reingest_current`, which reads the
// workspace INSIDE the ingest lock. Pin the two invariants that fix relies on: an
// unset workspace is a no-op (never commits an empty segment over a live index),
// and a set workspace re-ingests the CURRENT model (not a stale pre-captured one,
// so a concurrent reload is never reverted).
#[test]
fn reingest_current_heals_the_set_workspace_and_is_a_noop_when_unset() {
    let dir = TempDir::new("reingestcurrent");
    let (ws, _targetroot, _sourceroot) = build_ab(&dir);
    let store = Store::open(&dir.sub("store")).unwrap();
    let orch = QueryOrchestrator::new(store, Box::new(NoopOverlay), false);

    assert!(
        orch.reingest_current().is_none(),
        "reingest_current with no workspace set must be a no-op"
    );

    orch.ingest(ws).unwrap();
    assert!(
        orch.reingest_current()
            .expect("workspace set -> Some")
            .is_ok(),
        "reingest_current with a set workspace must re-ingest it"
    );
}

#[test]
fn rename_synthetic_only_symbol_rejected() {
    let dir = TempDir::new("synthetic");
    let targetroot = dir.sub("target");
    let sourceroot = dir.sub("sources");
    // `pkg/X#gen().` is referenced but never defined; its owner `pkg/X#` is.
    let x = DocFixture::new("x/X.scala", "class X:\n  def use = gen\n")
        .symbol(sym("pkg/X#", KIND_CLASS, 0, "X"))
        .symbol(sym("pkg/X#use().", KIND_METHOD, 0, "use"))
        .symbol(sym("pkg/X#gen().", KIND_METHOD, 0, "gen"))
        .occurrence(occ(rng(0, 6, 0, 7), "pkg/X#", DEFINITION))
        .occurrence(occ(rng(1, 6, 1, 9), "pkg/X#use().", DEFINITION))
        .occurrence(occ(rng(1, 12, 1, 15), "pkg/X#gen().", REFERENCE));
    write_doc(&targetroot, &sourceroot, &x);
    let ws = WorkspaceTargets::new(vec![TargetSpec::new("app", targetroot, sourceroot)]);
    let orch = orchestrator(&dir, Arc::new(ws));
    let compiler = OkCompiler;
    let engine = RenameEngine::new(&orch, &compiler);
    let err = engine.rename("x/X.scala", 1, 12, "Renamed").unwrap_err();
    assert!(matches!(err, LsError::RenameRejected { .. }));
}

#[test]
fn rename_shared_source_disagreement_rejected() {
    let dir = TempDir::new("disagree");
    let t0 = dir.sub("t0");
    let s0 = dir.sub("s0");
    let t1 = dir.sub("t1");
    let s1 = dir.sub("s1");
    // The same uri, same source, but different symbols at the edit span.
    write_doc(
        &t0,
        &s0,
        &DocFixture::new("s/S.scala", "class S\n")
            .symbol(sym("pkg/S#", KIND_CLASS, 0, "S"))
            .occurrence(occ(rng(0, 6, 0, 7), "pkg/S#", DEFINITION)),
    );
    write_doc(
        &t1,
        &s1,
        &DocFixture::new("s/S.scala", "class S\n")
            .symbol(sym("other/S#", KIND_CLASS, 0, "S"))
            .occurrence(occ(rng(0, 6, 0, 7), "other/S#", DEFINITION)),
    );
    let ws = WorkspaceTargets::new(vec![
        TargetSpec::new("t0", t0, s0),
        TargetSpec::new("t1", t1, s1),
    ]);
    let orch = orchestrator(&dir, Arc::new(ws));
    let compiler = OkCompiler;
    let engine = RenameEngine::new(&orch, &compiler);
    let err = engine.rename("s/S.scala", 0, 6, "Renamed").unwrap_err();
    assert!(matches!(err, LsError::RenameRejected { .. }));
}

#[test]
fn rename_md5_stale_before_emit_rejected() {
    let dir = TempDir::new("md5stale");
    let targetroot = dir.sub("target");
    let sourceroot = dir.sub("sources");
    write_doc(
        &targetroot,
        &sourceroot,
        &DocFixture::new("a/A.scala", "class S\n")
            .symbol(sym("pkg/S#", KIND_CLASS, 0, "S"))
            .occurrence(occ(rng(0, 6, 0, 7), "pkg/S#", DEFINITION)),
    );
    write_doc(
        &targetroot,
        &sourceroot,
        &DocFixture::new("b/B.scala", "val b = new S\n").occurrence(occ(
            rng(0, 12, 0, 13),
            "pkg/S#",
            REFERENCE,
        )),
    );
    let ws = WorkspaceTargets::new(vec![TargetSpec::new("app", targetroot, sourceroot.clone())]);
    let orch = orchestrator(&dir, Arc::new(ws));
    // Make B stale (append a comment; the ref token on line 0 is unchanged).
    std::fs::write(sourceroot.join("b/B.scala"), "val b = new S\n// changed\n").unwrap();
    let compiler = OkCompiler;
    let engine = RenameEngine::new(&orch, &compiler);
    let err = engine.rename("a/A.scala", 0, 6, "Renamed").unwrap_err();
    assert!(matches!(err, LsError::StaleIndex { uri } if uri == "b/B.scala"));
}

// --- object-symbol definition (the zaozi cross-file shape) ---------------------

const KIND_OBJECT: i32 = 5;
const KIND_TRAIT: i32 = 12;

/// Mirror of the real-project shape that surfaced empty definitions: a sealed
/// trait + companion object in one doc, the OBJECT referenced from a sibling
/// doc of the SAME target, in a multi-segment package.
#[test]
fn symbol_definition_resolves_a_companion_object_in_a_multi_segment_package() {
    let dir = TempDir::new("symdefobj");
    let t = dir.sub("target");
    let s = dir.sub("src");
    write_doc(
        &t,
        &s,
        &DocFixture::new(
            "decoder/BitSet.scala",
            "package me.jiuyang.decoder\ntrait BitSet\nobject BitSet\n",
        )
        .symbol(sym("me/jiuyang/decoder/BitSet#", KIND_TRAIT, 0, "BitSet"))
        .symbol(sym("me/jiuyang/decoder/BitSet.", KIND_OBJECT, 0, "BitSet"))
        .occurrence(occ(rng(1, 6, 1, 12), "me/jiuyang/decoder/BitSet#", DEFINITION))
        .occurrence(occ(rng(2, 7, 2, 13), "me/jiuyang/decoder/BitSet.", DEFINITION)),
    );
    write_doc(
        &t,
        &s,
        &DocFixture::new(
            "decoder/TruthTable.scala",
            "package me.jiuyang.decoder\nval x = BitSet\n",
        )
        .occurrence(occ(rng(1, 8, 1, 14), "me/jiuyang/decoder/BitSet.", REFERENCE)),
    );
    let ws = WorkspaceTargets::new(vec![TargetSpec::new("decoder", t, s.clone())]);
    let orch = orchestrator(&dir, Arc::new(ws));

    let locs = orch.symbol_definition(
        "me/jiuyang/decoder/BitSet.",
        &file_uri(&s, "decoder/TruthTable.scala"),
    );
    assert_eq!(locs.len(), 1, "the object definition must resolve: {locs:?}");
    assert_eq!(locs[0].uri, file_uri(&s, "decoder/BitSet.scala"));
    assert_eq!(locs[0].span, Span::new(2, 7, 2, 13));

    // The companion trait resolves independently to its own name span.
    let trait_locs = orch.symbol_definition(
        "me/jiuyang/decoder/BitSet#",
        &file_uri(&s, "decoder/TruthTable.scala"),
    );
    assert_eq!(trait_locs.len(), 1, "the trait definition must resolve: {trait_locs:?}");
    assert_eq!(trait_locs[0].span, Span::new(1, 6, 1, 12));
}

/// A lone object (no companion) in a single-segment package — the minimal
/// object-symbol shape.
#[test]
fn symbol_definition_resolves_a_plain_object_symbol() {
    let dir = TempDir::new("symdefobjplain");
    let t = dir.sub("target");
    let s = dir.sub("src");
    write_doc(
        &t,
        &s,
        &DocFixture::new("o/O.scala", "package pkg\nobject O\n")
            .symbol(sym("pkg/O.", KIND_OBJECT, 0, "O"))
            .occurrence(occ(rng(1, 7, 1, 8), "pkg/O.", DEFINITION)),
    );
    write_doc(
        &t,
        &s,
        &DocFixture::new("u/U.scala", "package pkg\nval y = O\n")
            .occurrence(occ(rng(1, 8, 1, 9), "pkg/O.", REFERENCE)),
    );
    let ws = WorkspaceTargets::new(vec![TargetSpec::new("app", t, s.clone())]);
    let orch = orchestrator(&dir, Arc::new(ws));
    let locs = orch.symbol_definition("pkg/O.", &file_uri(&s, "u/U.scala"));
    assert_eq!(locs.len(), 1, "a plain object definition must resolve: {locs:?}");
    assert_eq!(locs[0].uri, file_uri(&s, "o/O.scala"));
}

/// The mill layout: EVERY target's `-sourceroot` is the workspace root, so
/// sourceroot-prefix attribution ties across all targets. The requesting
/// buffer's true owner is the target whose ingested doc row carries it —
/// never an arbitrary tied pick (which prunes valid definitions through a
/// disconnected target's closure).
#[test]
fn symbol_definition_attributes_the_buffer_by_doc_row_under_a_shared_sourceroot() {
    let dir = TempDir::new("symdefshared");
    let s = dir.sub("ws"); // ONE sourceroot for every target, like mill.
    let lib_t = dir.sub("lib-target");
    let tests_t = dir.sub("tests-target");
    let other_t = dir.sub("other-target");
    let other2_t = dir.sub("other2-target");
    write_doc(
        &lib_t,
        &s,
        &DocFixture::new("decoder/src/BitSet.scala", "package pkg\nobject B\n")
            .symbol(sym("pkg/B.", 5, 0, "B"))
            .occurrence(occ(rng(1, 7, 1, 8), "pkg/B.", DEFINITION)),
    );
    write_doc(
        &tests_t,
        &s,
        &DocFixture::new("decoder/tests/src/BSpec.scala", "package pkg\nval u = B\n")
            .occurrence(occ(rng(1, 8, 1, 9), "pkg/B.", REFERENCE)),
    );
    write_doc(
        &other_t,
        &s,
        &DocFixture::new("rv/src/R.scala", "package rv\nclass R\n")
            .symbol(sym("rv/R#", KIND_CLASS, 0, "R"))
            .occurrence(occ(rng(1, 6, 1, 7), "rv/R#", DEFINITION)),
    );
    write_doc(
        &other2_t,
        &s,
        &DocFixture::new("rv/tests/src/RSpec.scala", "package rv\nval r = new R\n")
            .occurrence(occ(rng(1, 12, 1, 13), "rv/R#", REFERENCE)),
    );
    // Workspace order mirrors the observed real model: the requesting target
    // first, disconnected targets LAST (the arbitrary tie-pick victims).
    let ws = WorkspaceTargets::new(vec![
        TargetSpec::new("lib", lib_t, s.clone()),
        TargetSpec::new("tests", tests_t, s.clone()).with_deps(vec!["lib".to_string()]),
        TargetSpec::new("other", other_t, s.clone()),
        TargetSpec::new("other2", other2_t, s.clone()).with_deps(vec!["other".to_string()]),
    ]);
    let orch = orchestrator(&dir, Arc::new(ws));

    // From the tests buffer (owned by `tests`, which depends on `lib`), the
    // object defined in `lib` must resolve.
    let locs = orch.symbol_definition("pkg/B.", &file_uri(&s, "decoder/tests/src/BSpec.scala"));
    assert_eq!(
        locs.len(),
        1,
        "shared-sourceroot attribution must use the doc's own target: {locs:?}"
    );
    assert_eq!(locs[0].uri, file_uri(&s, "decoder/src/BitSet.scala"));

    // And from the defining buffer itself (same-target cross-file).
    let same = orch.symbol_definition("pkg/B.", &file_uri(&s, "decoder/src/BitSet.scala"));
    assert_eq!(same.len(), 1, "same-target lookup must survive the tie: {same:?}");
}
