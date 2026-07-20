//! `QueryOrchestrator::document_symbols` — the index-backed nested outline —
//! over the pinned-scalac fixture corpus (real dotty SemanticDB) plus the
//! index-truth-only dirty-buffer contract.
//!
//! The outline carries only definition NAME spans (`range == selectionRange`
//! at the LSP edge — the documented limitation), nests by SemanticDB owner
//! chain with the companion fallback (enum cases live under the enum class
//! node), excludes locals/parameters/constructors/setters, and preserves
//! source order at every level.

mod common;
mod fixture;

use common::TestOverlay;
use fixture::*;
use ls_engine::DocSymbolEntry;

const CORE: &str = "a/src/pkga/Core.scala";
const OVER: &str = "a/src/pkga/Over.scala";
const LOCAL: &str = "a/src/pkga/LocalDef.scala";

/// `name(kind)` with nested children in brackets — a compact, exact tree
/// rendering for assertions.
fn render(nodes: &[DocSymbolEntry]) -> String {
    nodes
        .iter()
        .map(|n| {
            let kind = format!("{:?}", n.kind);
            if n.children.is_empty() {
                format!("{}({kind})", n.name)
            } else {
                format!("{}({kind})[{}]", n.name, render(&n.children))
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// Core.scala holds the shape zoo: a class with members, its companion object,
// a trait, an enum with cases (owned by the synthetic companion — the
// companion FALLBACK attaches them under the enum class node), a toplevel
// extension method and a toplevel given (both owned by the synthetic
// `Core$package.`, which has no definition occurrence — they surface as
// toplevels). Parameters and `<init>` constructors never appear.
#[test]
fn core_outline_nests_class_object_trait_and_enum_members() {
    let stack = new_stack();
    let outline = stack.orch.document_symbols(CORE);
    assert_eq!(
        render(&outline),
        "Core(Class)[label(Method) ping(Method)] \
         Core(Object)[make(Method)] \
         Greeter(Trait)[greet(Method)] \
         Color(Class)[Red(Method) Green(Method) Blue(Method)] \
         shout(Method) defaultCore(Method)"
    );
}

// Every node's span is exactly the definition NAME token span (the index has
// no fuller extent to offer), and root nodes arrive in source order.
#[test]
fn outline_spans_are_the_name_token_spans_in_source_order() {
    let stack = new_stack();
    let outline = stack.orch.document_symbols(CORE);
    assert_eq!(outline[0].span, token_span(CORE, "Core", 0), "class Core");
    assert_eq!(outline[1].span, token_span(CORE, "Core", 1), "object Core");
    assert_eq!(
        outline[2].children[0].span,
        token_span(CORE, "greet", 0),
        "Greeter#greet"
    );
    let starts: Vec<(u32, u32)> = outline
        .iter()
        .map(|n| (n.span.start_line, n.span.start_char))
        .collect();
    let mut sorted = starts.clone();
    sorted.sort_unstable();
    assert_eq!(starts, sorted, "roots in source order");
}

// Overloads are distinct SemanticDB symbols: both `fmt` definitions surface
// under `Over`, in source order, alongside the vals.
#[test]
fn overloads_surface_once_per_symbol() {
    let stack = new_stack();
    let outline = stack.orch.document_symbols(OVER);
    assert_eq!(
        render(&outline),
        "Over(Object)[fmt(Method) fmt(Method) a(Method) b(Method)]"
    );
}

// Locals never surface: `loop` lives inside `countdown`'s body and is a
// local symbol — the outline stops at the member level.
#[test]
fn locals_are_excluded_from_the_outline() {
    let stack = new_stack();
    let outline = stack.orch.document_symbols(LOCAL);
    assert_eq!(
        render(&outline),
        "LocalDefs(Object)[countdown(Method)]",
        "the local `loop` must not appear"
    );
}

// Index-truth-only by decision: a DIRTY buffer still answers from the index —
// the outline lags the buffer until save, and is never an error.
#[test]
fn a_dirty_buffer_still_answers_the_indexed_outline() {
    let clean = new_stack();
    let expected = clean.orch.document_symbols(CORE);
    let dirty = new_stack_with_overlay(Box::new(TestOverlay::dirty(CORE, None)));
    assert!(dirty.orch.overlay().is_dirty(CORE), "the overlay is armed");
    assert_eq!(
        dirty.orch.document_symbols(CORE),
        expected,
        "dirty answers index truth, not an error or an empty"
    );
}

// A uri the snapshot does not hold answers the empty outline.
#[test]
fn an_unknown_uri_answers_the_empty_outline() {
    let stack = new_stack();
    assert!(stack.orch.document_symbols("nope/Missing.scala").is_empty());
}
