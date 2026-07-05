//! §18.1 references / workspace-symbol / document-highlight correctness matrix
//! over the pinned-scalac `FixtureWorkspace` corpus (three targets, B -> A edge,
//! C disconnected). Port of the Scala `ReferencesAndQuerySuite`.

mod fixture;

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use fixture::*;
use ls_engine::{
    DirtyBufferOverlay, DocumentHighlightService, HighlightKind, OverlayHit, QueryOrchestrator,
    ReferencesEngine, ReferencesResult, ResolutionSource,
};
use ls_index_model::{Loc, LsError, Role, Span};
use ls_store::Store;

type SpanKey = (u32, u32, u32, u32);

fn key(s: Span) -> SpanKey {
    (s.start_line, s.start_char, s.end_line, s.end_char)
}

fn refs(
    stack: &Stack,
    uri: &str,
    token: &str,
    nth: usize,
    include_declaration: bool,
) -> ReferencesResult {
    let (line, ch) = cursor(uri, token, nth);
    ReferencesEngine::new(&stack.orch)
        .references(uri, line, ch, include_declaration)
        .unwrap()
}

fn locs_in(result: &ReferencesResult, uri: &str) -> Vec<Loc> {
    result
        .locations()
        .into_iter()
        .filter(|l| l.uri == uri)
        .collect()
}

fn spans_in(result: &ReferencesResult, uri: &str) -> BTreeSet<SpanKey> {
    locs_in(result, uri)
        .into_iter()
        .map(|l| key(l.span))
        .collect()
}

fn token_set(uri: &str, token: &str) -> BTreeSet<SpanKey> {
    token_spans(uri, token).into_iter().map(key).collect()
}

fn contains_token(result: &ReferencesResult, uri: &str, token: &str, nth: usize) -> bool {
    let span = token_span(uri, token, nth);
    result
        .locations()
        .contains(&Loc::new(uri.to_string(), span))
}

fn uris_of(result: &ReferencesResult) -> BTreeSet<String> {
    result.locations().into_iter().map(|l| l.uri).collect()
}

fn uri_set(uris: &[&str]) -> BTreeSet<String> {
    uris.iter().map(|s| s.to_string()).collect()
}

// ------------------------------------------------------------ ingest

#[test]
fn ingest_report_all_docs_indexed_shared_counted_none_stale() {
    let stack = new_stack();
    assert_eq!(
        stack.report.docs_indexed, SOURCE_COUNT,
        "{:?}",
        stack.report
    );
    assert_eq!(
        stack.report.docs_shared, 1,
        "shared/src/shared/Shared.scala compiled by A and B"
    );
    assert_eq!(stack.report.docs_stale, 0);
    assert_eq!(stack.report.docs_skipped, 0);
    assert!(stack.report.parse_errors.is_empty());
    assert!(stack.report.symbol_count > 0);
    assert!(stack.report.ref_group_count > 0);
    assert_eq!(
        stack.report.rename_group_count,
        stack.report.ref_group_count
    );
}

// ------------------------------------------------------------ class/companion/constructor

#[test]
fn class_references_unify_companion_and_constructor_across_files_and_targets() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Core.scala", "Core", 0, false);
    assert!(
        contains_token(&r, "a/src/pkga/Core.scala", "Core", 2),
        "{r:?}"
    );
    assert!(!locs_in(&r, "a/src/pkga/Impl.scala").is_empty(), "{r:?}");
    assert!(!locs_in(&r, "b/src/pkgb/UseB.scala").is_empty(), "{r:?}");
}

#[test]
fn target_pruning_disconnected_c_excluded() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Core.scala", "Core", 0, true);
    assert!(
        locs_in(&r, "c/src/pkga/CopyCore.scala").is_empty(),
        "fixture-c has no dependency edge to fixture-a and must be pruned: {r:?}"
    );
    assert!(!token_spans("c/src/pkga/CopyCore.scala", "Core").is_empty());
}

#[test]
fn apply_sugar_unification_case_class() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Item.scala", "Item", 0, false);
    assert!(
        contains_token(&r, "a/src/pkga/Item.scala", "Item", 1),
        "{r:?}"
    );
    assert!(
        contains_token(&r, "a/src/pkga/Item.scala", "Item", 2),
        "{r:?}"
    );
    assert!(
        contains_token(&r, "a/src/pkga/Item.scala", "Item", 3),
        "{r:?}"
    );
    assert!(!locs_in(&r, "b/src/pkgb/UseB.scala").is_empty(), "{r:?}");
}

// ------------------------------------------------------------ trait/object/enum

#[test]
fn trait_references_reach_extends_and_cross_target() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Core.scala", "Greeter", 0, false);
    assert!(
        contains_token(&r, "a/src/pkga/Impl.scala", "Greeter", 0),
        "{r:?}"
    );
    assert!(
        contains_token(&r, "b/src/pkgb/UseB.scala", "Greeter", 0),
        "{r:?}"
    );
}

#[test]
fn object_references_from_shared_source() {
    let stack = new_stack();
    let r = refs(
        &stack,
        "shared/src/shared/Shared.scala",
        "SharedThing",
        0,
        false,
    );
    assert!(
        contains_token(&r, "b/src/pkgb/UseB.scala", "SharedThing", 0),
        "{r:?}"
    );
}

#[test]
fn enum_references_include_type_and_case_use() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Core.scala", "Color", 0, false);
    assert!(
        contains_token(&r, "b/src/pkgb/UseB.scala", "Color", 0),
        "{r:?}"
    );
    assert!(
        contains_token(&r, "b/src/pkgb/UseB.scala", "Color", 1),
        "{r:?}"
    );
}

// ------------------------------------------------------------ methods

#[test]
fn method_overloads_stay_separate() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Over.scala", "fmt", 0, false); // def fmt(i: Int)
    let fmt = token_spans("a/src/pkga/Over.scala", "fmt");
    let locs = spans_in(&r, "a/src/pkga/Over.scala");
    assert!(
        locs.contains(&key(fmt[2])),
        "fmt(1) call must be present: {r:?}"
    );
    assert!(
        !locs.contains(&key(fmt[1])),
        "other overload def must be absent"
    );
    assert!(
        !locs.contains(&key(fmt[3])),
        "fmt(\"x\") call must be absent"
    );
}

#[test]
fn export_forwarder_exact_set() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Exported.scala", "exported", 0, true);
    assert_eq!(uris_of(&r), uri_set(&["a/src/pkga/Exported.scala"]));
    let expected: BTreeSet<SpanKey> = [
        key(token_span("a/src/pkga/Exported.scala", "exported", 0)), // definition
        key(token_span("a/src/pkga/Exported.scala", "exported", 2)), // forwarder call
    ]
    .into_iter()
    .collect();
    assert_eq!(spans_in(&r, "a/src/pkga/Exported.scala"), expected, "{r:?}");
}

#[test]
fn case_class_copy_call_site_only() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Copyable.scala", "copy", 0, true);
    assert_eq!(uris_of(&r), uri_set(&["a/src/pkga/Copyable.scala"]));
    assert_eq!(
        spans_in(&r, "a/src/pkga/Copyable.scala"),
        token_set("a/src/pkga/Copyable.scala", "copy"),
        "{r:?}"
    );
}

#[test]
fn var_getter_setter_definition_exact() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Vars.scala", "value", 0, true);
    assert_eq!(uris_of(&r), uri_set(&["a/src/pkga/Vars.scala"]));
    assert_eq!(
        spans_in(&r, "a/src/pkga/Vars.scala"),
        token_set("a/src/pkga/Vars.scala", "value"),
        "{r:?}"
    );
}

#[test]
fn local_val_stays_in_document() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Vars.scala", "tmp", 0, true);
    assert_eq!(uris_of(&r), uri_set(&["a/src/pkga/Vars.scala"]));
    assert_eq!(
        spans_in(&r, "a/src/pkga/Vars.scala"),
        token_set("a/src/pkga/Vars.scala", "tmp"),
        "{r:?}"
    );
}

#[test]
fn extension_method_exact_set() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Core.scala", "shout", 0, true);
    for uri in [
        "a/src/pkga/Core.scala",
        "a/src/pkga/Impl.scala",
        "b/src/pkgb/UseB.scala",
    ] {
        assert_eq!(spans_in(&r, uri), token_set(uri, "shout"), "{uri}: {r:?}");
    }
    assert_eq!(
        uris_of(&r),
        uri_set(&[
            "a/src/pkga/Core.scala",
            "a/src/pkga/Impl.scala",
            "b/src/pkgb/UseB.scala",
        ])
    );
}

#[test]
fn given_references_by_name() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Core.scala", "defaultCore", 0, false);
    assert!(
        contains_token(&r, "a/src/pkga/Impl.scala", "defaultCore", 0),
        "{r:?}"
    );
    assert!(
        contains_token(&r, "b/src/pkgb/UseB.scala", "defaultCore", 0),
        "{r:?}"
    );
}

#[test]
fn given_references_exact_by_name_uses() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Core.scala", "defaultCore", 0, false);
    let use_files = [
        "a/src/pkga/Impl.scala",
        "b/src/pkgb/UseB.scala",
        "a/src/pkga/Using.scala",
    ];
    for uri in use_files {
        assert_eq!(
            spans_in(&r, uri),
            token_set(uri, "defaultCore"),
            "{uri}: {r:?}"
        );
    }
    assert_eq!(uris_of(&r), uri_set(&use_files));
}

#[test]
fn inline_def_exact_set() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Inline.scala", "twice", 0, true);
    assert_eq!(
        spans_in(&r, "a/src/pkga/Inline.scala"),
        token_set("a/src/pkga/Inline.scala", "twice"),
        "{r:?}"
    );
    assert_eq!(
        spans_in(&r, "b/src/pkgb/UseB.scala"),
        token_set("b/src/pkgb/UseB.scala", "twice")
    );
    assert_eq!(
        uris_of(&r),
        uri_set(&["a/src/pkga/Inline.scala", "b/src/pkgb/UseB.scala"])
    );
}

#[test]
fn top_level_def_and_val_exact() {
    let stack = new_stack();
    for token in ["topHelper", "topConst"] {
        let r = refs(&stack, "a/src/pkga/TopLevel.scala", token, 0, true);
        assert_eq!(
            spans_in(&r, "a/src/pkga/TopLevel.scala"),
            token_set("a/src/pkga/TopLevel.scala", token),
            "{token}: {r:?}"
        );
        assert_eq!(
            spans_in(&r, "b/src/pkgb/UseB.scala"),
            token_set("b/src/pkgb/UseB.scala", token),
            "{token} cross-file"
        );
        assert_eq!(
            uris_of(&r),
            uri_set(&["a/src/pkga/TopLevel.scala", "b/src/pkgb/UseB.scala"]),
            "{token}"
        );
    }
}

#[test]
fn cross_file_val_member_exact() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Named.scala", "title", 0, true);
    assert_eq!(
        spans_in(&r, "a/src/pkga/Named.scala"),
        token_set("a/src/pkga/Named.scala", "title"),
        "{r:?}"
    );
    assert_eq!(
        spans_in(&r, "b/src/pkgb/UseB.scala"),
        token_set("b/src/pkgb/UseB.scala", "title")
    );
    assert_eq!(
        uris_of(&r),
        uri_set(&["a/src/pkga/Named.scala", "b/src/pkgb/UseB.scala"])
    );
}

#[test]
fn private_member_in_file_only() {
    let stack = new_stack();
    for token in ["helper", "state"] {
        let r = refs(&stack, "a/src/pkga/Private.scala", token, 0, true);
        assert_eq!(
            uris_of(&r),
            uri_set(&["a/src/pkga/Private.scala"]),
            "{token}"
        );
        assert_eq!(
            spans_in(&r, "a/src/pkga/Private.scala"),
            token_set("a/src/pkga/Private.scala", token),
            "{token}: {r:?}"
        );
    }
}

#[test]
fn nested_local_def_stays_in_document() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/LocalDef.scala", "loop", 0, true);
    assert_eq!(uris_of(&r), uri_set(&["a/src/pkga/LocalDef.scala"]));
    assert_eq!(
        spans_in(&r, "a/src/pkga/LocalDef.scala"),
        token_set("a/src/pkga/LocalDef.scala", "loop"),
        "{r:?}"
    );
}

#[test]
fn opaque_type_references_exact() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Opaque.scala", "UserId", 0, true);
    assert_eq!(uris_of(&r), uri_set(&["a/src/pkga/Opaque.scala"]));
    assert_eq!(
        spans_in(&r, "a/src/pkga/Opaque.scala"),
        token_set("a/src/pkga/Opaque.scala", "UserId"),
        "{r:?}"
    );
}

// ------------------------------------------------------------ includeDeclaration

#[test]
fn include_declaration_adds_definition_site() {
    let stack = new_stack();
    let def_span = token_span("a/src/pkga/Item.scala", "Item", 0);
    let without = refs(&stack, "a/src/pkga/Item.scala", "Item", 1, false);
    let with_decl = refs(&stack, "a/src/pkga/Item.scala", "Item", 1, true);
    let def_loc = Loc::new("a/src/pkga/Item.scala".to_string(), def_span);
    assert!(!without.locations().contains(&def_loc));
    assert!(with_decl.locations().contains(&def_loc));
    assert!(with_decl
        .hits
        .iter()
        .any(|h| h.loc.span == def_span && h.role == Role::Definition));
    assert!(with_decl.locations().len() > without.locations().len());
}

#[test]
fn results_deduped_and_sorted() {
    let stack = new_stack();
    let r = refs(&stack, "a/src/pkga/Item.scala", "Item", 0, true);
    let locs = r.locations();
    let mut distinct = locs.clone();
    distinct.dedup();
    assert_eq!(locs, distinct);
    let keys: Vec<(String, u32, u32, u32, u32)> = locs
        .iter()
        .map(|l| {
            (
                l.uri.clone(),
                l.span.start_line,
                l.span.start_char,
                l.span.end_line,
                l.span.end_char,
            )
        })
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

// ------------------------------------------------------------ workspace symbol

#[test]
fn workspace_symbol_prefix_query() {
    let stack = new_stack();
    let core = stack.orch.workspace_symbol("Cor", 50);
    assert!(core.iter().any(|h| h.display == "Core"), "{core:?}");
    let shared = stack.orch.workspace_symbol("SharedTh", 50);
    assert!(
        shared.iter().any(|h| h.display == "SharedThing"),
        "{shared:?}"
    );
    assert!(stack
        .orch
        .workspace_symbol("NoSuchSymbolXyz", 50)
        .is_empty());
}

// ------------------------------------------------------------ document highlight

#[test]
fn document_highlight_splits_read_write_by_role() {
    let stack = new_stack();
    let spans = token_spans("a/src/pkga/Vars.scala", "value");
    let (line, ch) = cursor("a/src/pkga/Vars.scala", "value", 1);
    let hs = DocumentHighlightService::new(&stack.orch)
        .highlights("a/src/pkga/Vars.scala", line, ch)
        .unwrap();
    assert!(
        hs.iter()
            .any(|h| h.span == spans[0] && h.kind == HighlightKind::Write),
        "{hs:?}"
    );
    assert!(
        hs.iter()
            .any(|h| h.span == spans[1] && h.kind == HighlightKind::Read),
        "{hs:?}"
    );
    assert!(hs.iter().filter(|h| h.kind == HighlightKind::Write).count() >= 1);
    assert!(
        hs.iter().filter(|h| h.kind == HighlightKind::Read).count() >= 2,
        "read + setter site: {hs:?}"
    );
    let keys: Vec<(u32, u32)> = hs
        .iter()
        .map(|h| (h.span.start_line, h.span.start_char))
        .collect();
    let mut sorted = keys.clone();
    sorted.sort();
    assert_eq!(keys, sorted);
}

// ------------------------------------------------------------ overlay hooks

struct ContributingOverlay {
    symbol: String,
    extra: Loc,
    queried: Arc<Mutex<BTreeSet<String>>>,
    prefix_match: bool,
}

impl DirtyBufferOverlay for ContributingOverlay {
    fn is_dirty(&self, _uri: &str) -> bool {
        false
    }
    fn symbol_at(&self, _uri: &str, _line: u32, _character: u32) -> Option<OverlayHit> {
        None
    }
    fn contributes_occurrences(&self) -> bool {
        true
    }
    fn occurrences_of(&self, semantic_symbol: &str) -> Option<Vec<Loc>> {
        self.queried
            .lock()
            .unwrap()
            .insert(semantic_symbol.to_string());
        let hit = if self.prefix_match {
            semantic_symbol.starts_with(&self.symbol)
        } else {
            semantic_symbol == self.symbol
        };
        if hit {
            Some(vec![self.extra.clone()])
        } else {
            None
        }
    }
}

#[test]
fn overlay_contributes_distinct_dirty_references() {
    let extra = Loc::new("virtual/Dirty.scala".to_string(), Span::new(0, 0, 0, 4));
    let overlay = Box::new(ContributingOverlay {
        symbol: "pkga/Item#".to_string(),
        extra: extra.clone(),
        queried: Arc::new(Mutex::new(BTreeSet::new())),
        prefix_match: false,
    });
    let stack = new_stack_with_overlay(overlay);
    let (line, ch) = cursor("a/src/pkga/Item.scala", "Item", 0);
    let r = ReferencesEngine::new(&stack.orch)
        .references("a/src/pkga/Item.scala", line, ch, false)
        .unwrap();
    let overlay_hits: Vec<Loc> = r
        .hits
        .iter()
        .filter(|h| h.from_overlay)
        .map(|h| h.loc.clone())
        .collect();
    assert_eq!(overlay_hits, vec![extra]);
    assert!(r.hits.iter().any(|h| !h.from_overlay));
}

#[test]
fn overlay_group_fanout_queries_companion_members() {
    // Cursor is the class `pkga/Item#`, but the overlay only answers for a
    // companion-side member (`pkga/Item.` prefix). A group-keyed fan-out must
    // still surface it; a cursor-symbol-only query would miss it.
    let extra = Loc::new("virtual/Dirty.scala".to_string(), Span::new(1, 2, 1, 6));
    let queried = Arc::new(Mutex::new(BTreeSet::new()));
    let overlay = Box::new(ContributingOverlay {
        symbol: "pkga/Item.".to_string(),
        extra: extra.clone(),
        queried: queried.clone(),
        prefix_match: true,
    });
    let stack = new_stack_with_overlay(overlay);
    let (line, ch) = cursor("a/src/pkga/Item.scala", "Item", 0);
    let r = ReferencesEngine::new(&stack.orch)
        .references("a/src/pkga/Item.scala", line, ch, false)
        .unwrap();
    let overlay_hits: Vec<Loc> = r
        .hits
        .iter()
        .filter(|h| h.from_overlay)
        .map(|h| h.loc.clone())
        .collect();
    assert_eq!(overlay_hits, vec![extra]);
    let q = queried.lock().unwrap();
    assert!(q.contains("pkga/Item#"), "cursor symbol queried: {q:?}");
    assert!(
        q.iter()
            .any(|s| s != "pkga/Item#" && s.starts_with("pkga/Item.")),
        "group fan-out must query companion members: {q:?}"
    );
}

struct FlippableOverlay {
    dirty_uri: String,
    answer: Arc<Mutex<Option<OverlayHit>>>,
}

impl DirtyBufferOverlay for FlippableOverlay {
    fn is_dirty(&self, uri: &str) -> bool {
        uri == self.dirty_uri
    }
    fn symbol_at(&self, _uri: &str, _line: u32, _character: u32) -> Option<OverlayHit> {
        self.answer.lock().unwrap().clone()
    }
    fn occurrences_of(&self, _semantic_symbol: &str) -> Option<Vec<Loc>> {
        None
    }
}

#[test]
fn dirty_buffer_overlay_answers_cursor_and_degrades() {
    let dirty_uri = "a/src/pkga/Item.scala";
    let answer = Arc::new(Mutex::new(Some(OverlayHit {
        semantic_symbol: "pkga/Item#".to_string(),
        span: Span::new(2, 11, 2, 15),
        role: Role::Definition,
        pc_only: false,
    })));
    let overlay = Box::new(FlippableOverlay {
        dirty_uri: dirty_uri.to_string(),
        answer: answer.clone(),
    });
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let orch = QueryOrchestrator::new(store, overlay, true);
    orch.ingest(workspace_for()).unwrap();

    let cursor = orch.symbol_at_cursor(dirty_uri, 2, 12).unwrap();
    assert_eq!(cursor.source, ResolutionSource::Overlay);
    assert_eq!(cursor.semantic_symbol, "pkga/Item#");

    // An unanswerable dirty query degrades to StaleIndex (never the snapshot).
    *answer.lock().unwrap() = None;
    let err = orch.symbol_at_cursor(dirty_uri, 2, 12).unwrap_err();
    assert!(matches!(err, LsError::StaleIndex { .. }), "{err:?}");
}

// ------------------------------------------------------------ errors

#[test]
fn no_symbol_at_cursor_errors() {
    let stack = new_stack();
    let err = ReferencesEngine::new(&stack.orch)
        .references("a/src/pkga/Item.scala", 1, 0, false)
        .unwrap_err();
    assert!(matches!(err, LsError::NoSymbolAtCursor { .. }), "{err:?}");
}

#[test]
fn unknown_uri_errors_not_indexed() {
    let stack = new_stack();
    let err = ReferencesEngine::new(&stack.orch)
        .references("nope/Missing.scala", 0, 0, false)
        .unwrap_err();
    assert!(matches!(err, LsError::NotIndexed { .. }), "{err:?}");
}

// ------------------------------------------------------------ re-ingest supersede

#[test]
fn re_ingest_supersedes_cleanly_same_answers() {
    let stack = new_stack();
    let before = stack.orch.current_snapshot().unwrap();
    let before_gen = before.generation();
    let r1 = refs(&stack, "a/src/pkga/Item.scala", "Item", 0, false).locations();

    let report2 = stack.orch.ingest(workspace_for()).unwrap();
    assert!(report2.segment_id >= before.segment_id());
    let after = stack.orch.current_snapshot().unwrap();
    assert!(after.generation() > before_gen);

    let r2 = refs(&stack, "a/src/pkga/Item.scala", "Item", 0, false).locations();
    assert_eq!(r2, r1, "identical corpus must produce identical references");
}
