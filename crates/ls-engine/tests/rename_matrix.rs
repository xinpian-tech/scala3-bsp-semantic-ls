//! §18.1 rename correctness matrix over the pinned-scalac `FixtureWorkspace`
//! corpus. Port of the Scala `RenameSuite` (non-mutating: exact edit spans +
//! every file-preserving rejection) and `RenameMutationSuite` (stale-md5,
//! fresh-snapshot stale cursor doc, shared-source disagreement) over an
//! isolated temp copy of the fixtures.

mod fixture;

use std::collections::BTreeSet;
use std::sync::Mutex;

use fixture::*;
use ls_engine::{
    CompileOutcome, CompileService, DirtyBufferOverlay, OverlayHit, RenameEngine, WorkspaceEditPlan,
};
use ls_index_model::{Loc, LsError, Role, Span};

type SpanKey = (u32, u32, u32, u32);

fn key(s: Span) -> SpanKey {
    (s.start_line, s.start_char, s.end_line, s.end_char)
}

// ---- compile services ----

struct OkCompiler;
impl CompileService for OkCompiler {
    fn compile(&self, _targets: &[String]) -> CompileOutcome {
        CompileOutcome::Ok
    }
}

struct FailCompiler;
impl CompileService for FailCompiler {
    fn compile(&self, _targets: &[String]) -> CompileOutcome {
        CompileOutcome::Failed {
            reason: "compile failed".to_string(),
        }
    }
}

#[derive(Default)]
struct RecordingCompiler {
    calls: Mutex<Vec<Vec<String>>>,
}
impl RecordingCompiler {
    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().unwrap().clone()
    }
}
impl CompileService for RecordingCompiler {
    fn compile(&self, targets: &[String]) -> CompileOutcome {
        self.calls.lock().unwrap().push(targets.to_vec());
        CompileOutcome::Ok
    }
}

// ---- helpers ----

fn plan_of(stack: &Stack, uri: &str, token: &str, nth: usize, new_name: &str) -> WorkspaceEditPlan {
    let (line, ch) = cursor(uri, token, nth);
    RenameEngine::new(&stack.orch, &OkCompiler)
        .rename(uri, line, ch, new_name)
        .unwrap_or_else(|e| panic!("rename {uri} {token} -> {new_name}: {e:?}"))
}

fn rejection(stack: &Stack, uri: &str, token: &str, nth: usize, new_name: &str) -> LsError {
    let (line, ch) = cursor(uri, token, nth);
    RenameEngine::new(&stack.orch, &OkCompiler)
        .rename(uri, line, ch, new_name)
        .unwrap_err()
}

fn edit_uris(plan: &WorkspaceEditPlan) -> BTreeSet<String> {
    plan.edits.keys().cloned().collect()
}

fn span_set(plan: &WorkspaceEditPlan, uri: &str) -> BTreeSet<SpanKey> {
    plan.edits
        .get(uri)
        .map(|v| v.iter().map(|e| key(e.span)).collect())
        .unwrap_or_default()
}

fn token_set(uri: &str, token: &str) -> BTreeSet<SpanKey> {
    token_spans(uri, token).into_iter().map(key).collect()
}

fn uri_set(uris: &[&str]) -> BTreeSet<String> {
    uris.iter().map(|s| s.to_string()).collect()
}

fn reasons_contain(err: &LsError, needle: &str) -> bool {
    matches!(err, LsError::RenameRejected { reasons } if reasons.iter().any(|r| r.contains(needle)))
}

// ------------------------------------------------------------ happy paths

#[test]
fn rename_case_class_every_token_never_apply() {
    let stack = new_stack();
    let plan = plan_of(&stack, "a/src/pkga/Item.scala", "Item", 0, "Thing");
    assert_eq!(
        edit_uris(&plan),
        uri_set(&["a/src/pkga/Item.scala", "b/src/pkgb/UseB.scala"])
    );
    assert_eq!(
        span_set(&plan, "a/src/pkga/Item.scala"),
        token_set("a/src/pkga/Item.scala", "Item")
    );
    assert_eq!(
        span_set(&plan, "b/src/pkgb/UseB.scala"),
        token_set("b/src/pkgb/UseB.scala", "Item")
    );
    let apply_span = key(token_span("a/src/pkga/Item.scala", "apply", 0));
    assert!(!span_set(&plan, "a/src/pkga/Item.scala").contains(&apply_span));
    assert!(plan.edits.values().flatten().all(|e| e.new_text == "Thing"));
    let sum: usize = plan.edits.values().map(Vec::len).sum();
    assert_eq!(plan.occurrence_count, sum);
}

#[test]
fn rename_compiles_reverse_dependency_closure_of_definition() {
    let stack = new_stack();
    let compiler = RecordingCompiler::default();
    let (line, ch) = cursor("a/src/pkga/Item.scala", "Item", 0);
    RenameEngine::new(&stack.orch, &compiler)
        .rename("a/src/pkga/Item.scala", line, ch, "Thing")
        .unwrap();
    assert_eq!(
        compiler.calls(),
        vec![vec!["fixture-a".to_string(), "fixture-b".to_string()]]
    );
}

#[test]
fn rename_method_in_shared_source() {
    let stack = new_stack();
    let plan = plan_of(&stack, "shared/src/shared/Shared.scala", "tag", 0, "label");
    assert_eq!(
        span_set(&plan, "shared/src/shared/Shared.scala"),
        token_set("shared/src/shared/Shared.scala", "tag")
    );
    assert_eq!(
        span_set(&plan, "b/src/pkgb/UseB.scala"),
        token_set("b/src/pkgb/UseB.scala", "tag")
    );
    assert_eq!(
        edit_uris(&plan),
        uri_set(&["shared/src/shared/Shared.scala", "b/src/pkgb/UseB.scala"])
    );
}

#[test]
fn rename_var_getter_setter_definition_together() {
    let stack = new_stack();
    let plan = plan_of(&stack, "a/src/pkga/Vars.scala", "value", 0, "count");
    assert_eq!(
        span_set(&plan, "a/src/pkga/Vars.scala"),
        token_set("a/src/pkga/Vars.scala", "value")
    );
    assert_eq!(edit_uris(&plan), uri_set(&["a/src/pkga/Vars.scala"]));
}

#[test]
fn rename_local_val_touches_only_its_document() {
    let stack = new_stack();
    let plan = plan_of(&stack, "a/src/pkga/Vars.scala", "tmp", 0, "next");
    assert_eq!(
        span_set(&plan, "a/src/pkga/Vars.scala"),
        token_set("a/src/pkga/Vars.scala", "tmp")
    );
    assert_eq!(edit_uris(&plan), uri_set(&["a/src/pkga/Vars.scala"]));
}

#[test]
fn rename_one_overload_leaves_the_other_alone() {
    let stack = new_stack();
    let plan = plan_of(&stack, "a/src/pkga/Over.scala", "fmt", 0, "fmtInt");
    let fmt = token_spans("a/src/pkga/Over.scala", "fmt");
    let expected: BTreeSet<SpanKey> = [key(fmt[0]), key(fmt[2])].into_iter().collect();
    assert_eq!(span_set(&plan, "a/src/pkga/Over.scala"), expected);
}

#[test]
fn rename_inline_def_across_targets() {
    let stack = new_stack();
    let plan = plan_of(&stack, "a/src/pkga/Inline.scala", "twice", 0, "doubled");
    assert_eq!(
        span_set(&plan, "a/src/pkga/Inline.scala"),
        token_set("a/src/pkga/Inline.scala", "twice")
    );
    assert_eq!(
        span_set(&plan, "b/src/pkgb/UseB.scala"),
        token_set("b/src/pkgb/UseB.scala", "twice")
    );
    assert_eq!(
        edit_uris(&plan),
        uri_set(&["a/src/pkga/Inline.scala", "b/src/pkgb/UseB.scala"])
    );
}

#[test]
fn rename_private_method_in_file_only() {
    let stack = new_stack();
    let plan = plan_of(&stack, "a/src/pkga/Private.scala", "helper", 0, "compute");
    assert_eq!(
        span_set(&plan, "a/src/pkga/Private.scala"),
        token_set("a/src/pkga/Private.scala", "helper")
    );
    assert_eq!(edit_uris(&plan), uri_set(&["a/src/pkga/Private.scala"]));
}

#[test]
fn rename_private_val_in_file_only() {
    let stack = new_stack();
    let plan = plan_of(&stack, "a/src/pkga/Private.scala", "state", 0, "seed");
    assert_eq!(
        span_set(&plan, "a/src/pkga/Private.scala"),
        token_set("a/src/pkga/Private.scala", "state")
    );
    assert_eq!(edit_uris(&plan), uri_set(&["a/src/pkga/Private.scala"]));
}

#[test]
fn rename_nested_local_def_only_its_document() {
    let stack = new_stack();
    let plan = plan_of(&stack, "a/src/pkga/LocalDef.scala", "loop", 0, "step");
    assert_eq!(
        span_set(&plan, "a/src/pkga/LocalDef.scala"),
        token_set("a/src/pkga/LocalDef.scala", "loop")
    );
    assert_eq!(edit_uris(&plan), uri_set(&["a/src/pkga/LocalDef.scala"]));
}

#[test]
fn rename_val_member_cross_file() {
    let stack = new_stack();
    let plan = plan_of(&stack, "a/src/pkga/Named.scala", "title", 0, "name");
    assert_eq!(
        span_set(&plan, "a/src/pkga/Named.scala"),
        token_set("a/src/pkga/Named.scala", "title")
    );
    assert_eq!(
        span_set(&plan, "b/src/pkgb/UseB.scala"),
        token_set("b/src/pkgb/UseB.scala", "title")
    );
    assert_eq!(
        edit_uris(&plan),
        uri_set(&["a/src/pkga/Named.scala", "b/src/pkgb/UseB.scala"])
    );
}

#[test]
fn rename_top_level_def_cross_file() {
    let stack = new_stack();
    let plan = plan_of(
        &stack,
        "a/src/pkga/TopLevel.scala",
        "topHelper",
        0,
        "topDouble",
    );
    assert_eq!(
        span_set(&plan, "a/src/pkga/TopLevel.scala"),
        token_set("a/src/pkga/TopLevel.scala", "topHelper")
    );
    assert_eq!(
        span_set(&plan, "b/src/pkgb/UseB.scala"),
        token_set("b/src/pkgb/UseB.scala", "topHelper")
    );
    assert_eq!(
        edit_uris(&plan),
        uri_set(&["a/src/pkga/TopLevel.scala", "b/src/pkgb/UseB.scala"])
    );
}

#[test]
fn rename_top_level_val_cross_file() {
    let stack = new_stack();
    let plan = plan_of(
        &stack,
        "a/src/pkga/TopLevel.scala",
        "topConst",
        0,
        "topValue",
    );
    assert_eq!(
        span_set(&plan, "a/src/pkga/TopLevel.scala"),
        token_set("a/src/pkga/TopLevel.scala", "topConst")
    );
    assert_eq!(
        span_set(&plan, "b/src/pkgb/UseB.scala"),
        token_set("b/src/pkgb/UseB.scala", "topConst")
    );
    assert_eq!(
        edit_uris(&plan),
        uri_set(&["a/src/pkga/TopLevel.scala", "b/src/pkgb/UseB.scala"])
    );
}

#[test]
fn rename_extension_method_across_targets() {
    let stack = new_stack();
    let plan = plan_of(&stack, "a/src/pkga/Core.scala", "shout", 0, "yell");
    for uri in [
        "a/src/pkga/Core.scala",
        "a/src/pkga/Impl.scala",
        "b/src/pkgb/UseB.scala",
    ] {
        assert_eq!(span_set(&plan, uri), token_set(uri, "shout"), "{uri}");
    }
    assert_eq!(
        edit_uris(&plan),
        uri_set(&[
            "a/src/pkga/Core.scala",
            "a/src/pkga/Impl.scala",
            "b/src/pkgb/UseB.scala",
        ])
    );
}

#[test]
fn rename_given_every_by_name_use() {
    let stack = new_stack();
    let plan = plan_of(
        &stack,
        "a/src/pkga/Core.scala",
        "defaultCore",
        0,
        "appDefault",
    );
    let files = [
        "a/src/pkga/Core.scala",
        "a/src/pkga/Impl.scala",
        "a/src/pkga/Using.scala",
        "b/src/pkgb/UseB.scala",
    ];
    for uri in files {
        assert_eq!(span_set(&plan, uri), token_set(uri, "defaultCore"), "{uri}");
    }
    assert_eq!(edit_uris(&plan), uri_set(&files));
    assert!(plan
        .edits
        .values()
        .flatten()
        .all(|e| e.new_text == "appDefault"));
}

#[test]
fn keyword_new_name_is_backtick_quoted() {
    let stack = new_stack();
    let plan = plan_of(&stack, "a/src/pkga/Vars.scala", "tmp", 0, "type");
    assert!(plan
        .edits
        .get("a/src/pkga/Vars.scala")
        .unwrap()
        .iter()
        .all(|e| e.new_text == "`type`"));
}

// ------------------------------------------------------------ rejections

#[test]
fn compile_failure_rejects_rename() {
    let stack = new_stack();
    let (line, ch) = cursor("a/src/pkga/Item.scala", "Item", 0);
    let err = RenameEngine::new(&stack.orch, &FailCompiler)
        .rename("a/src/pkga/Item.scala", line, ch, "Thing")
        .unwrap_err();
    assert!(matches!(err, LsError::CompileFailed { .. }), "{err:?}");
}

#[test]
fn invalid_new_identifier_rejected_before_compile() {
    let stack = new_stack();
    let compiler = RecordingCompiler::default();
    let engine = RenameEngine::new(&stack.orch, &compiler);
    let (line, ch) = cursor("a/src/pkga/Item.scala", "Item", 0);
    assert!(matches!(
        engine.rename("a/src/pkga/Item.scala", line, ch, "has`tick"),
        Err(LsError::RenameRejected { .. })
    ));
    assert!(matches!(
        engine.rename("a/src/pkga/Item.scala", line, ch, ""),
        Err(LsError::RenameRejected { .. })
    ));
    assert!(compiler.calls().is_empty());
}

#[test]
fn no_symbol_at_cursor_rejected_before_compile() {
    let stack = new_stack();
    let compiler = RecordingCompiler::default();
    let err = RenameEngine::new(&stack.orch, &compiler)
        .rename("a/src/pkga/Item.scala", 1, 0, "Thing")
        .unwrap_err();
    assert!(matches!(err, LsError::NoSymbolAtCursor { .. }), "{err:?}");
    assert!(compiler.calls().is_empty());
}

#[test]
fn override_family_rejected() {
    let stack = new_stack();
    let err = rejection(&stack, "a/src/pkga/Core.scala", "greet", 0, "salute");
    assert!(reasons_contain(&err, "override"), "{err:?}");
}

#[test]
fn exported_symbol_rejected() {
    let stack = new_stack();
    let err = rejection(
        &stack,
        "a/src/pkga/Exported.scala",
        "exported",
        0,
        "renamed",
    );
    assert!(reasons_contain(&err, "exported symbol"), "{err:?}");
}

#[test]
fn synthetic_only_rejected() {
    let stack = new_stack();
    let err = rejection(&stack, "a/src/pkga/Copyable.scala", "copy", 0, "duplicate");
    assert!(reasons_contain(&err, "synthetic"), "{err:?}");
}

#[test]
fn generated_occurrences_rejected() {
    let stack = new_stack();
    let err = rejection(&stack, "a/src/pkga/Widget.scala", "Widget", 0, "Gizmo");
    assert!(reasons_contain(&err, "generated"), "{err:?}");
}

#[test]
fn readonly_occurrences_rejected() {
    let stack = new_stack();
    let err = rejection(&stack, "a/src/pkga/Gadget.scala", "Gadget", 0, "Gizmo");
    assert!(reasons_contain(&err, "readonly"), "{err:?}");
}

#[test]
fn readonly_cursor_document_rejected_before_compile() {
    let stack = new_stack();
    let compiler = RecordingCompiler::default();
    let (line, ch) = cursor("a/src/pkga/ReadonlyUse.scala", "Gadget", 0);
    let err = RenameEngine::new(&stack.orch, &compiler)
        .rename("a/src/pkga/ReadonlyUse.scala", line, ch, "Gizmo")
        .unwrap_err();
    assert!(matches!(err, LsError::RenameRejected { .. }), "{err:?}");
    assert!(compiler.calls().is_empty());
}

#[test]
fn opaque_type_rejected() {
    let stack = new_stack();
    let err = rejection(&stack, "a/src/pkga/Opaque.scala", "UserId", 0, "AccountId");
    assert!(reasons_contain(&err, "opaque"), "{err:?}");
}

#[test]
fn external_library_symbol_rejected() {
    let stack = new_stack();
    let err = rejection(&stack, "a/src/pkga/Externals.scala", "List", 0, "Seq");
    assert!(reasons_contain(&err, "outside the workspace"), "{err:?}");
}

#[test]
fn dependency_sources_rejected() {
    let stack = new_stack();
    let err = rejection(
        &stack,
        "dep/src/pkgdep/DepThing.scala",
        "DepThing",
        0,
        "Other",
    );
    assert!(reasons_contain(&err, "dependency"), "{err:?}");
}

struct PcOnlyOverlay;
impl DirtyBufferOverlay for PcOnlyOverlay {
    fn is_dirty(&self, uri: &str) -> bool {
        uri == "a/src/pkga/Item.scala"
    }
    fn symbol_at(&self, _uri: &str, _line: u32, _character: u32) -> Option<OverlayHit> {
        Some(OverlayHit {
            semantic_symbol: "pcplugin/Synthetic#".to_string(),
            span: Span::new(0, 0, 0, 4),
            role: Role::Reference,
            pc_only: true,
        })
    }
    fn occurrences_of(&self, _semantic_symbol: &str) -> Option<Vec<Loc>> {
        None
    }
}

#[test]
fn pc_only_symbol_rejected() {
    let stack = new_stack_with_overlay(Box::new(PcOnlyOverlay));
    let err = RenameEngine::new(&stack.orch, &OkCompiler)
        .rename("a/src/pkga/Item.scala", 2, 12, "Thing")
        .unwrap_err();
    assert!(matches!(err, LsError::PcOnlySymbol), "{err:?}");
}

struct DirtyOverlay;
impl DirtyBufferOverlay for DirtyOverlay {
    fn is_dirty(&self, uri: &str) -> bool {
        uri == "a/src/pkga/Item.scala"
    }
    fn symbol_at(&self, _uri: &str, _line: u32, _character: u32) -> Option<OverlayHit> {
        Some(OverlayHit {
            semantic_symbol: "pkga/Item#".to_string(),
            span: Span::new(2, 11, 2, 15),
            role: Role::Definition,
            pc_only: false,
        })
    }
    fn occurrences_of(&self, _semantic_symbol: &str) -> Option<Vec<Loc>> {
        None
    }
}

#[test]
fn dirty_unsaved_buffer_rejected() {
    let stack = new_stack_with_overlay(Box::new(DirtyOverlay));
    let err = RenameEngine::new(&stack.orch, &OkCompiler)
        .rename("a/src/pkga/Item.scala", 2, 12, "Thing")
        .unwrap_err();
    assert!(reasons_contain(&err, "unsaved"), "{err:?}");
}

#[test]
fn prepare_rename_returns_occurrence_span() {
    let stack = new_stack();
    let span = token_span("a/src/pkga/Item.scala", "Item", 0);
    let (line, ch) = cursor("a/src/pkga/Item.scala", "Item", 0);
    let got = RenameEngine::new(&stack.orch, &OkCompiler)
        .prepare_rename("a/src/pkga/Item.scala", line, ch)
        .unwrap();
    assert_eq!(got, span);
}

// ------------------------------------------------------------ mutation suite

#[test]
fn stale_md5_edited_downstream_file_rejected_before_emit() {
    let stack = clone_and_ingest();
    // Edit a file that will receive edits (Beta references Alpha) after ingest.
    let beta = stack.source_path("a/src/pkga/Beta.scala");
    let orig = std::fs::read_to_string(&beta).unwrap();
    std::fs::write(&beta, format!("{orig}\n// edited after compile\n")).unwrap();

    let (line, ch) = cursor("a/src/pkga/Alpha.scala", "Alpha", 0);
    let err = RenameEngine::new(&stack.orch, &OkCompiler)
        .rename("a/src/pkga/Alpha.scala", line, ch, "Omega")
        .unwrap_err();
    assert!(
        matches!(&err, LsError::StaleIndex { uri } if uri == "a/src/pkga/Beta.scala"),
        "{err:?}"
    );
}

#[test]
fn fresh_snapshot_stale_cursor_document_rejected() {
    let stack = clone_and_ingest();
    // Edit the CURSOR document after ingest so its source no longer matches the
    // ingested SemanticDB; the resolve is non-Snapshot and must reject.
    let alpha = stack.source_path("a/src/pkga/Alpha.scala");
    let orig = std::fs::read_to_string(&alpha).unwrap();
    std::fs::write(&alpha, format!("{orig}\n// edited after compile\n")).unwrap();

    let (line, ch) = cursor("a/src/pkga/Alpha.scala", "Alpha", 0);
    let err = RenameEngine::new(&stack.orch, &OkCompiler)
        .rename("a/src/pkga/Alpha.scala", line, ch, "Omega")
        .unwrap_err();
    assert!(
        matches!(&err, LsError::StaleIndex { uri } if uri == "a/src/pkga/Alpha.scala"),
        "{err:?}"
    );
}

#[test]
fn shared_source_disagreement_rejected() {
    let stack = clone_and_ingest();
    // Target B's view of the shared source becomes unreadable: the two targets
    // can no longer be proven to agree on the rename group at the edit spans.
    let shared_b = stack.semanticdb_path("out-b", "shared/src/shared/Shared.scala");
    assert!(shared_b.is_file(), "{shared_b:?}");
    std::fs::write(&shared_b, [0x0A, 0x7F]).unwrap(); // present but truncated

    let (line, ch) = cursor("shared/src/shared/Shared.scala", "tag", 0);
    let err = RenameEngine::new(&stack.orch, &OkCompiler)
        .rename("shared/src/shared/Shared.scala", line, ch, "label")
        .unwrap_err();
    assert!(reasons_contain(&err, "disagree"), "{err:?}");
}
