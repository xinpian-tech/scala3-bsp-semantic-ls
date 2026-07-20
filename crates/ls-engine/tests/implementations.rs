//! `QueryOrchestrator::implementations` — the index-backed method
//! override-family query — over the pinned-scalac fixture corpus (real dotty
//! `overridden_symbols` edges) and a synthetic two-target family proving
//! transitive edges and requesting-forward-closure pruning.
//!
//! The honest scope (see the orchestrator doc): the alias groups do NOT union
//! override families — the store carries only the per-rename-group
//! `has_override_family` FLAG — so the family is resolved at query time from
//! the index candidates (same method name, override-flagged groups) verified
//! against the `overridden_symbols` edges of each candidate's defining
//! document's raw `.semanticdb`. Type symbols answer the honest empty: no
//! type-hierarchy/sealed-subtype edge exists anywhere in the index.

mod common;
mod fixture;

use std::sync::Arc;

use common::*;
use fixture::*;
use ls_engine::{QueryOrchestrator, TargetSpec, WorkspaceTargets};
use ls_index_model::Loc;
use ls_semanticdb::SdbSymbolInfo;
use ls_store::Store;

const CORE: &str = "a/src/pkga/Core.scala";
const IMPL: &str = "a/src/pkga/Impl.scala";
const LOCAL: &str = "a/src/pkga/LocalDef.scala";

fn impls(stack: &Stack, uri: &str, token: &str, nth: usize) -> Vec<Loc> {
    let (line, character) = cursor(uri, token, nth);
    stack.orch.implementations(uri, line, character).unwrap()
}

// The real-corpus positive: on the trait declaration `Greeter#greet` the
// implementations are the overriders' def sites — `LoudGreeter#greet` in
// Impl.scala, located at its exact name span (dotty's `overridden_symbols`
// edge, verified from the pinned `.semanticdb`).
#[test]
fn an_abstract_method_answers_its_overriders_def_sites() {
    let stack = new_stack();
    assert_eq!(
        impls(&stack, CORE, "greet", 0),
        vec![Loc::new(IMPL, token_span(IMPL, "greet", 0))]
    );
}

// The leaf override has no overriders of its own: the honest empty (never the
// abstract parent — that jump is definition/super, not implementation).
#[test]
fn a_leaf_override_answers_empty() {
    let stack = new_stack();
    assert_eq!(impls(&stack, IMPL, "greet", 0), Vec::<Loc>::new());
}

// A type symbol answers the honest empty: neither the SemanticDB nor the
// index carries subtype edges (dotty emits `overridden_symbols` for methods
// only), so implementors of a trait are not enumerable from index truth.
#[test]
fn a_trait_type_symbol_answers_the_honest_empty() {
    let stack = new_stack();
    assert_eq!(impls(&stack, CORE, "Greeter", 0), Vec::<Loc>::new());
}

// A method outside any override family short-circuits on the store's
// `has_override_family` flag — the empty answer costs no `.semanticdb` read.
#[test]
fn a_method_without_an_override_family_answers_empty() {
    let stack = new_stack();
    assert_eq!(impls(&stack, CORE, "ping", 0), Vec::<Loc>::new());
}

// Locals are never overridable: empty.
#[test]
fn a_local_symbol_answers_empty() {
    let stack = new_stack();
    assert_eq!(impls(&stack, LOCAL, "loop", 0), Vec::<Loc>::new());
}

// A cursor with no symbol stays the typed references-style error.
#[test]
fn a_symbol_free_cursor_is_a_typed_error() {
    let stack = new_stack();
    let err = stack.orch.implementations(CORE, 1, 0).unwrap_err();
    assert!(
        matches!(err, ls_index_model::LsError::NoSymbolAtCursor { .. }),
        "{err:?}"
    );
}

// ---- synthetic two-target family: transitive edges + closure pruning ----

fn info(symbol: &str, kind: i32, display: &str, overridden: &[&str]) -> SdbSymbolInfo {
    SdbSymbolInfo {
        overridden_symbols: overridden.iter().map(|s| s.to_string()).collect(),
        ..sym(symbol, kind, 0, display)
    }
}

/// Two targets, `app -> core`:
///   core/Base.scala   trait Base    { def m }            (the family base)
///   core/ImpA.scala   class ImpA    override m           (direct edge)
///   core/Deep.scala   class Deep    override m           (edge list names ImpA
///                                                         AND Base — the dotty
///                                                         transitive chain)
///   app/ImpB.scala    class ImpB    override m + a Base.m reference
fn family_workspace(dir: &TempDir) -> (Arc<WorkspaceTargets>, QueryOrchestrator) {
    let core_t = dir.sub("core-t");
    let core_s = dir.sub("core-s");
    let app_t = dir.sub("app-t");
    let app_s = dir.sub("app-s");

    let base = DocFixture::new("base/Base.scala", "trait Base:\n  def m: Int\n")
        .symbol(info("pkg/Base#", 14, "Base", &[]))
        .symbol(info("pkg/Base#m().", KIND_METHOD, "m", &[]))
        .occurrence(occ(rng(0, 6, 0, 10), "pkg/Base#", DEFINITION))
        .occurrence(occ(rng(1, 6, 1, 7), "pkg/Base#m().", DEFINITION));
    let imp_a = DocFixture::new(
        "impa/ImpA.scala",
        "class ImpA extends Base:\n  def m: Int = 1\n",
    )
    .symbol(info("pkg/ImpA#", KIND_CLASS, "ImpA", &[]))
    .symbol(info("pkg/ImpA#m().", KIND_METHOD, "m", &["pkg/Base#m()."]))
    .occurrence(occ(rng(0, 6, 0, 10), "pkg/ImpA#", DEFINITION))
    .occurrence(occ(rng(0, 19, 0, 23), "pkg/Base#", REFERENCE))
    .occurrence(occ(rng(1, 6, 1, 7), "pkg/ImpA#m().", DEFINITION));
    let deep = DocFixture::new(
        "deep/Deep.scala",
        "class Deep extends ImpA:\n  def m: Int = 2\n",
    )
    .symbol(info("pkg/Deep#", KIND_CLASS, "Deep", &[]))
    .symbol(info(
        "pkg/Deep#m().",
        KIND_METHOD,
        "m",
        // dotty lists the FULL transitive chain, nearest first.
        &["pkg/ImpA#m().", "pkg/Base#m()."],
    ))
    .occurrence(occ(rng(0, 6, 0, 10), "pkg/Deep#", DEFINITION))
    .occurrence(occ(rng(1, 6, 1, 7), "pkg/Deep#m().", DEFINITION));
    let imp_b = DocFixture::new(
        "impb/ImpB.scala",
        "class ImpB extends Base:\n  def m: Int = 3\nval q = (new ImpB).m\n",
    )
    .symbol(info("pkg/ImpB#", KIND_CLASS, "ImpB", &[]))
    .symbol(info("pkg/ImpB#m().", KIND_METHOD, "m", &["pkg/Base#m()."]))
    .occurrence(occ(rng(0, 6, 0, 10), "pkg/ImpB#", DEFINITION))
    .occurrence(occ(rng(1, 6, 1, 7), "pkg/ImpB#m().", DEFINITION))
    // A reference to the BASE method from the app target, so the family can
    // be queried from a downstream buffer.
    .occurrence(occ(rng(2, 19, 2, 20), "pkg/Base#m().", REFERENCE));

    write_doc(&core_t, &core_s, &base);
    write_doc(&core_t, &core_s, &imp_a);
    write_doc(&core_t, &core_s, &deep);
    write_doc(&app_t, &app_s, &imp_b);

    let ws = Arc::new(WorkspaceTargets::new(vec![
        TargetSpec::new("core", core_t, core_s),
        TargetSpec::new("app", app_t, app_s).with_deps(vec!["core".to_string()]),
    ]));
    let store = Store::open(&dir.sub("store")).unwrap();
    let orch = QueryOrchestrator::with_defaults(store);
    orch.ingest(Arc::clone(&ws)).unwrap();
    (ws, orch)
}

// Queried from the DOWNSTREAM buffer (app, forward closure {app, core}): the
// whole family answers — the direct core overrider, the transitive one (its
// edge list names the base directly, the dotty chain), and the app overrider.
#[test]
fn a_downstream_buffer_sees_the_whole_family_including_transitive_overrides() {
    let dir = TempDir::new("impl-family");
    let (_ws, orch) = family_workspace(&dir);
    // Cursor on the Base.m REFERENCE in the app doc.
    let locs = orch.implementations("impb/ImpB.scala", 2, 19).unwrap();
    assert_eq!(
        locs,
        vec![
            Loc::new("deep/Deep.scala", span(1, 6, 1, 7)),
            Loc::new("impa/ImpA.scala", span(1, 6, 1, 7)),
            Loc::new("impb/ImpB.scala", span(1, 6, 1, 7)),
        ],
        "deduped, sorted by (uri, span)"
    );
}

// Queried from the UPSTREAM buffer (core, forward closure {core}): the app
// overrider is pruned — the requesting buffer cannot SEE the app target, so
// its implementor never leaks (the shared symbol_definition visibility rule).
#[test]
fn an_upstream_buffer_is_pruned_to_its_forward_closure() {
    let dir = TempDir::new("impl-prune");
    let (_ws, orch) = family_workspace(&dir);
    // Cursor on the Base.m DEFINITION in the core doc.
    let locs = orch.implementations("base/Base.scala", 1, 6).unwrap();
    assert_eq!(
        locs,
        vec![
            Loc::new("deep/Deep.scala", span(1, 6, 1, 7)),
            Loc::new("impa/ImpA.scala", span(1, 6, 1, 7)),
        ],
        "the app-target overrider must be pruned"
    );
}

// On the MIDDLE of the chain (ImpA.m): only its own overrider (Deep, whose
// edge list names ImpA directly) — never the base above it.
#[test]
fn a_mid_chain_method_answers_only_its_own_overriders() {
    let dir = TempDir::new("impl-mid");
    let (_ws, orch) = family_workspace(&dir);
    let locs = orch.implementations("impa/ImpA.scala", 1, 6).unwrap();
    assert_eq!(locs, vec![Loc::new("deep/Deep.scala", span(1, 6, 1, 7))]);
}

fn span(sl: u32, sc: u32, el: u32, ec: u32) -> ls_index_model::Span {
    ls_index_model::Span::new(sl, sc, el, ec)
}
