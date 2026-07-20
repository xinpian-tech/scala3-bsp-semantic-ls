//! `QueryOrchestrator::{prepare_call_hierarchy, incoming_calls, outgoing_calls}`
//! — the index-backed call hierarchy under USAGE-HIERARCHY semantics (the
//! ratified Plan C).
//!
//! A "call" is any REFERENCE occurrence of the item's reference group (the
//! index persists no call-site facts, so eta-expansions and type-position uses
//! count), with exactly ONE noise filter: a reference whose source line begins
//! with the `import` token is dropped. Incoming scans the whole reference group
//! with NO closure pruning (downstream/disconnected callers are legitimate),
//! grouping by ENCLOSING DEFINITION synthesized from the `document_symbols`
//! entry set. Outgoing approximates the item's body as its SUCCESSOR-BASED
//! extent (the §7.1-rejected-for-outlines heuristic, accepted here as a query
//! best-effort).
//!
//! The natural call graph (prepare on def/ref, multi-caller grouping, the
//! no-pruning disconnected caller, real outgoing edges) is pinned over the
//! committed pinned-scalac corpus; the enclosing-definition RULE MATRIX, the
//! import-line filter, and the outgoing extent heuristic's trailing-code
//! misattribution are pinned over synthetic docs that place occurrences at
//! controlled positions (the `implementations` suite's synthetic discipline).

mod common;
mod fixture;

use std::collections::BTreeSet;
use std::sync::Arc;

use common::*;
use fixture::*;
use ls_engine::{CallEdge, CallItem, QueryOrchestrator, TargetSpec, WorkspaceTargets};
use ls_index_model::{Span, SymKind};
use ls_semanticdb::SdbSymbolInfo;
use ls_store::Store;

const CORE: &str = "a/src/pkga/Core.scala";
const IMPL: &str = "a/src/pkga/Impl.scala";
const USEB: &str = "b/src/pkgb/UseB.scala";
const COPY: &str = "c/src/pkga/CopyCore.scala";

fn prep(orch: &QueryOrchestrator, uri: &str, token: &str, nth: usize) -> Option<CallItem> {
    let (line, ch) = cursor(uri, token, nth);
    orch.prepare_call_hierarchy(uri, line, ch).unwrap()
}

/// The (uri, caller-name) pairs of an edge list, order-preserving.
fn edge_pairs(edges: &[CallEdge]) -> Vec<(String, String)> {
    edges
        .iter()
        .map(|e| (e.item.uri.clone(), e.item.name.clone()))
        .collect()
}

// ------------------------------------------------------------ prepare

// prepare on a DEFINITION and on a REFERENCE both answer the SAME definition-
// side item (name/kind/uri/name-span/symbol) — a call-hierarchy item is always
// anchored at the callable's definition, whichever occurrence the cursor sat on.
#[test]
fn prepare_answers_the_same_definition_item_on_a_def_and_on_a_ref() {
    let stack = new_stack();
    let on_def = prep(&stack.orch, CORE, "make", 0).expect("make def resolves");
    assert_eq!(on_def.name, "make");
    assert_eq!(on_def.kind, SymKind::Method);
    assert_eq!(on_def.uri, CORE);
    assert_eq!(on_def.span, token_span(CORE, "make", 0));
    assert_eq!(on_def.symbol, "pkga/Core.make().");

    // The `Core.make("a")` CALL SITE in Impl.scala answers the identical item.
    let on_ref = prep(&stack.orch, IMPL, "make", 0).expect("make ref resolves");
    assert_eq!(on_ref, on_def, "a ref answers the definition-side item");
}

// A non-callable cursor answers its ENCLOSING callable, or None: a toplevel
// trait/class/object declaration (no enclosing callable) is None; a type
// reference INSIDE a method (`: Core`, `new Core`) answers that method.
#[test]
fn prepare_a_non_callable_answers_its_enclosing_callable_or_none() {
    let stack = new_stack();
    // The trait declaration itself: non-callable, nothing encloses it.
    assert_eq!(prep(&stack.orch, CORE, "Greeter", 0), None);
    // `class Core` / `object Core` declarations: likewise None.
    assert_eq!(prep(&stack.orch, CORE, "Core", 0), None, "class Core decl");
    assert_eq!(prep(&stack.orch, CORE, "Core", 1), None, "object Core decl");

    // `def make(l: String): Core = new Core(l)` — the `Core` type reference and
    // the `new Core` constructor reference are non-callable, but both sit
    // inside `make`, so prepare answers the enclosing `make` item.
    let make = prep(&stack.orch, CORE, "make", 0).unwrap();
    assert_eq!(
        prep(&stack.orch, CORE, "Core", 2),
        Some(make.clone()),
        "the `: Core` return-type ref answers the enclosing make"
    );
    assert_eq!(
        prep(&stack.orch, CORE, "Core", 3),
        Some(make),
        "the `new Core` ref answers the enclosing make"
    );
}

// ------------------------------------------------------------ incoming

// Multi-caller, multi-file grouping: `Core.make` is called from four call sites
// across three targets; each caller is its ENCLOSING definition with the call
// site as its `fromRange`. Impl/UseB/CopyCore all enclose the call in a `val
// core` initializer (the val is the encloser); Core.scala's call sits in the
// `given defaultCore` initializer.
#[test]
fn incoming_groups_callers_by_their_enclosing_definition() {
    let stack = new_stack();
    let make = prep(&stack.orch, CORE, "make", 0).unwrap();
    let edges = stack.orch.incoming_calls(&make.symbol);

    let pairs: BTreeSet<(String, String)> = edge_pairs(&edges).into_iter().collect();
    let expected: BTreeSet<(String, String)> = [
        (CORE.to_string(), "defaultCore".to_string()),
        (IMPL.to_string(), "core".to_string()),
        (USEB.to_string(), "core".to_string()),
        (COPY.to_string(), "core".to_string()),
    ]
    .into_iter()
    .collect();
    assert_eq!(pairs, expected, "one caller per enclosing definition");

    // Every caller carries exactly one fromRange, and it is a `make` call-site
    // token in that caller's own doc (Core.scala's caller is `defaultCore`,
    // whose call site is the `make` on the `given` line, not the def token).
    for edge in &edges {
        let make_tokens: Vec<Span> = token_spans(&edge.item.uri, "make");
        assert_eq!(edge.call_sites.len(), 1, "{}", edge.item.uri);
        assert!(
            make_tokens.contains(&edge.call_sites[0]),
            "{}: fromRange {:?} is a make token",
            edge.item.uri,
            edge.call_sites[0]
        );
    }
}

// The deliberate NO-closure-pruning difference from `references`: CopyCore.scala
// lives in the DISCONNECTED target C (no dependency edge to A) and REDEFINES the
// same `pkga/Core.make().` symbol string, so `references` PRUNES it
// (`references_matrix::target_pruning_disconnected_c_excluded`) — but incoming
// call hierarchy keeps its caller, the accepted usage-hierarchy noise.
#[test]
fn incoming_includes_disconnected_target_callers_without_closure_pruning() {
    let stack = new_stack();
    let make = prep(&stack.orch, CORE, "make", 0).unwrap();
    let edges = stack.orch.incoming_calls(&make.symbol);
    assert!(
        edges.iter().any(|e| e.item.uri == COPY),
        "the disconnected target-C caller must appear (no closure pruning): {:?}",
        edge_pairs(&edges)
    );
}

// A local/empty/unknown item symbol answers the empty caller list (never a
// panic); the reference group of a genuine callable is non-empty.
#[test]
fn incoming_on_an_unknown_or_empty_symbol_is_empty() {
    let stack = new_stack();
    assert!(stack.orch.incoming_calls("").is_empty());
    assert!(stack.orch.incoming_calls("local7").is_empty());
    assert!(stack
        .orch
        .incoming_calls("pkga/DoesNotExist#nope().")
        .is_empty());
}

// ------------------------------------------------------------ outgoing

// Outgoing collects the REFERENCE occurrences inside the item's extent, grouped
// by target: `make`'s body `new Core(l)` (plus the `: Core` return type) calls
// `Core`; the extension `shout`'s body `c.ping.toUpperCase` calls `ping` (the
// external `toUpperCase` has no workspace definition and is dropped).
#[test]
fn outgoing_collects_body_references_grouped_by_target() {
    let stack = new_stack();

    let make = prep(&stack.orch, CORE, "make", 0).unwrap();
    let make_out = stack.orch.outgoing_calls(&make.symbol);
    assert_eq!(
        edge_pairs(&make_out),
        vec![(CORE.to_string(), "Core".to_string())],
        "make calls Core"
    );
    assert_eq!(
        make_out[0].call_sites,
        vec![
            span(6, 23, 6, 27), // the `: Core` return type
            span(6, 34, 6, 38), // the `new Core` constructor
        ],
    );

    let shout = prep(&stack.orch, CORE, "shout", 0).unwrap();
    let shout_out = stack.orch.outgoing_calls(&shout.symbol);
    assert_eq!(
        edge_pairs(&shout_out),
        vec![(CORE.to_string(), "ping".to_string())],
        "shout calls ping; the external toUpperCase is dropped"
    );
    assert_eq!(shout_out[0].call_sites, vec![token_span(CORE, "ping", 1)]);
}

// ------------------------------------------------------------ synthetic: the enclosing-definition RULE MATRIX

/// A synthetic single-target workspace over one hand-encoded doc, ingested once.
fn synthetic(tag: &str, doc: DocFixture) -> (TempDir, QueryOrchestrator) {
    let dir = TempDir::new(tag);
    let t = dir.sub("t");
    let s = dir.sub("s");
    write_doc(&t, &s, &doc);
    let ws = Arc::new(WorkspaceTargets::new(vec![TargetSpec::new("main", t, s)]));
    let store = Store::open(&dir.sub("store")).unwrap();
    let orch = QueryOrchestrator::with_defaults(store);
    orch.ingest(ws).unwrap();
    (dir, orch)
}

fn info(symbol: &str, kind: i32, display: &str) -> SdbSymbolInfo {
    sym(symbol, kind, 0, display)
}

const KIND_OBJECT: i32 = 10;

// The four documented containment outcomes, pinned in one doc via incoming
// grouping of references to `Sink.hit` placed at controlled positions:
//   L0  before any definition   -> the synthetic FILE-LEVEL item (empty symbol)
//   L5  inside `def outer`       -> outer (the nearest/deepest enclosing def)
//   L6  inside `val field` init  -> field (the val is the encloser)
//   L7  toplevel after the body  -> field (the name-span-only FALSE POSITIVE:
//                                   no later entry exists to close field's extent)
#[test]
fn the_enclosing_definition_rule_matrix() {
    // 0: package mtx
    // 1: object Sink:
    // 2:   def hit(x: Int): Int = x
    // 3: refBetween here
    // 4: class Box:
    // 5:   def outer(n: Int): Int = Sink.hit(1)
    // 6:   val field: Int = Sink.hit(2)
    // 7: refAfterBody here
    let src = "package mtx\nobject Sink:\n  def hit(x: Int): Int = x\nrefBetween here\nclass Box:\n  def outer(n: Int): Int = Sink.hit(1)\n  val field: Int = Sink.hit(2)\nrefAfterBody here\n";
    let doc = DocFixture::new("mtx/Mtx.scala", src)
        .symbol(info("mtx/Sink.", KIND_OBJECT, "Sink"))
        .symbol(info("mtx/Sink.hit().", KIND_METHOD, "hit"))
        .symbol(info("mtx/Box#", KIND_CLASS, "Box"))
        .symbol(info("mtx/Box#outer().", KIND_METHOD, "outer"))
        .symbol(info("mtx/Box#field.", KIND_METHOD, "field"))
        .occurrence(occ(rng(1, 7, 1, 11), "mtx/Sink.", DEFINITION))
        .occurrence(occ(rng(2, 6, 2, 9), "mtx/Sink.hit().", DEFINITION))
        .occurrence(occ(rng(4, 6, 4, 9), "mtx/Box#", DEFINITION))
        .occurrence(occ(rng(5, 6, 5, 11), "mtx/Box#outer().", DEFINITION))
        .occurrence(occ(rng(6, 6, 6, 11), "mtx/Box#field.", DEFINITION))
        // references to `hit` at the four controlled positions:
        .occurrence(occ(rng(0, 8, 0, 11), "mtx/Sink.hit().", REFERENCE)) // before any def
        .occurrence(occ(rng(5, 28, 5, 31), "mtx/Sink.hit().", REFERENCE)) // in outer
        .occurrence(occ(rng(6, 19, 6, 22), "mtx/Sink.hit().", REFERENCE)) // in field init
        .occurrence(occ(rng(7, 0, 7, 3), "mtx/Sink.hit().", REFERENCE)); // trailing after body
    let (_dir, orch) = synthetic("ch-matrix", doc);

    let edges = orch.incoming_calls("mtx/Sink.hit().");
    // Sorted by (uri, def-span start): the file item (0,0) first, then outer
    // (L5), then field (L6).
    assert_eq!(
        edge_pairs(&edges),
        vec![
            ("mtx/Mtx.scala".to_string(), "Mtx.scala".to_string()),
            ("mtx/Mtx.scala".to_string(), "outer".to_string()),
            ("mtx/Mtx.scala".to_string(), "field".to_string()),
        ]
    );
    // The file-level item has no symbol (incoming/outgoing on it answer empty)
    // and a zero span.
    assert_eq!(edges[0].item.symbol, "");
    assert_eq!(edges[0].item.kind, SymKind::UnknownKind);
    assert_eq!(edges[0].item.span, Span::new(0, 0, 0, 0));
    assert_eq!(
        edges[0].call_sites,
        vec![span(0, 8, 0, 11)],
        "before-any-def"
    );
    assert_eq!(edges[1].call_sites, vec![span(5, 28, 5, 31)], "in outer");
    // The val-initializer ref AND the trailing after-body ref BOTH group under
    // field — the false positive shares field's edge with the honest call.
    assert_eq!(
        edges[2].call_sites,
        vec![span(6, 19, 6, 22), span(7, 0, 7, 3)],
        "val init + trailing after-body both attribute to field"
    );

    // The synthetic file-level item answers nothing (empty symbol short-circuits).
    assert!(orch.incoming_calls(&edges[0].item.symbol).is_empty());
    assert!(orch.outgoing_calls(&edges[0].item.symbol).is_empty());
}

// ------------------------------------------------------------ synthetic: the import-line filter

// The one Plan-C noise filter: a reference sitting on an `import` line is
// dropped, so a file that IMPORTS the symbol and also CALLS it contributes only
// the real call site — the import is not a caller.
#[test]
fn incoming_drops_import_line_references() {
    // 0: package imp
    // 1: import imp.Api.run
    // 2: object Api:
    // 3:   def run(x: Int): Int = x
    // 4: object Client:
    // 5:   def go(): Int = run(1)
    let src = "package imp\nimport imp.Api.run\nobject Api:\n  def run(x: Int): Int = x\nobject Client:\n  def go(): Int = run(1)\n";
    let doc = DocFixture::new("imp/Imp.scala", src)
        .symbol(info("imp/Api.", KIND_OBJECT, "Api"))
        .symbol(info("imp/Api.run().", KIND_METHOD, "run"))
        .symbol(info("imp/Client.", KIND_OBJECT, "Client"))
        .symbol(info("imp/Client.go().", KIND_METHOD, "go"))
        .occurrence(occ(rng(2, 7, 2, 10), "imp/Api.", DEFINITION))
        .occurrence(occ(rng(3, 6, 3, 9), "imp/Api.run().", DEFINITION))
        .occurrence(occ(rng(4, 7, 4, 13), "imp/Client.", DEFINITION))
        .occurrence(occ(rng(5, 6, 5, 8), "imp/Client.go().", DEFINITION))
        .occurrence(occ(rng(1, 15, 1, 18), "imp/Api.run().", REFERENCE)) // the import line
        .occurrence(occ(rng(5, 18, 5, 21), "imp/Api.run().", REFERENCE)); // the real call
    let (_dir, orch) = synthetic("ch-import", doc);

    let edges = orch.incoming_calls("imp/Api.run().");
    assert_eq!(
        edge_pairs(&edges),
        vec![("imp/Imp.scala".to_string(), "go".to_string())],
        "only the real call site remains; the import ref is filtered"
    );
    assert_eq!(edges[0].call_sites, vec![span(5, 18, 5, 21)]);
    assert!(
        !edges
            .iter()
            .flat_map(|e| &e.call_sites)
            .any(|s| s.start_line == 1),
        "no caller carries a call site on the import line"
    );
}

// ------------------------------------------------------------ synthetic: the outgoing extent heuristic

// The outgoing SUCCESSOR-BASED extent is the §7.1-rejected-for-outlines
// heuristic, accepted here: `m`'s extent runs to the next NON-descendant entry
// (`tail`), so a trailing statement `T.b` that textually belongs to the object
// `U` (not to `m`) is MISATTRIBUTED to `m`'s outgoing calls. Pinned as ACTUAL
// behavior, honestly documented as a best-effort projection.
#[test]
fn outgoing_extent_heuristic_misattributes_trailing_code_after_the_body() {
    // 0: package oh
    // 1: object T:
    // 2:   def a: Int = 0
    // 3:   def b: Int = 0
    // 4: object U:
    // 5:   def m(): Int =
    // 6:     T.a
    // 7:   T.b
    // 8: def tail: Int = 0
    let src = "package oh\nobject T:\n  def a: Int = 0\n  def b: Int = 0\nobject U:\n  def m(): Int =\n    T.a\n  T.b\ndef tail: Int = 0\n";
    let doc = DocFixture::new("oh/Oh.scala", src)
        .symbol(info("oh/T.", KIND_OBJECT, "T"))
        .symbol(info("oh/T.a().", KIND_METHOD, "a"))
        .symbol(info("oh/T.b().", KIND_METHOD, "b"))
        .symbol(info("oh/U.", KIND_OBJECT, "U"))
        .symbol(info("oh/U.m().", KIND_METHOD, "m"))
        .symbol(info("oh/tail().", KIND_METHOD, "tail"))
        .occurrence(occ(rng(1, 7, 1, 8), "oh/T.", DEFINITION))
        .occurrence(occ(rng(2, 6, 2, 7), "oh/T.a().", DEFINITION))
        .occurrence(occ(rng(3, 6, 3, 7), "oh/T.b().", DEFINITION))
        .occurrence(occ(rng(4, 7, 4, 8), "oh/U.", DEFINITION))
        .occurrence(occ(rng(5, 6, 5, 7), "oh/U.m().", DEFINITION))
        .occurrence(occ(rng(8, 4, 8, 8), "oh/tail().", DEFINITION))
        .occurrence(occ(rng(6, 6, 6, 7), "oh/T.a().", REFERENCE)) // m's real body call
        .occurrence(occ(rng(7, 4, 7, 5), "oh/T.b().", REFERENCE)); // trailing U statement
    let (_dir, orch) = synthetic("ch-extent", doc);

    let edges = orch.outgoing_calls("oh/U.m().");
    assert_eq!(
        edge_pairs(&edges),
        vec![
            ("oh/Oh.scala".to_string(), "a".to_string()),
            ("oh/Oh.scala".to_string(), "b".to_string()),
        ],
        "m's extent swallows the trailing T.b that belongs to object U"
    );
    assert_eq!(
        edges[0].call_sites,
        vec![span(6, 6, 6, 7)],
        "the honest body call"
    );
    assert_eq!(
        edges[1].call_sites,
        vec![span(7, 4, 7, 5)],
        "the trailing after-body call, misattributed by the extent heuristic"
    );
}

fn span(sl: u32, sc: u32, el: u32, ec: u32) -> Span {
    Span::new(sl, sc, el, ec)
}
