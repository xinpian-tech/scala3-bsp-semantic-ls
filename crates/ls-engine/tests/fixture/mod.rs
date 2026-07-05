//! Rust analogue of the Scala `FixtureWorkspace`: drives the engines over the
//! committed pinned-scalac corpus (three targets: `fixture-a`; `fixture-b` with
//! a B -> A edge; disconnected `fixture-c`; one source shared by A and B;
//! generated/readonly/dependency doc-facts on A). The corpus is compiled once
//! (see the fixtures README) and committed under `tests/fixtures/`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ls_engine::{
    DirtyBufferOverlay, IngestReport, QueryOrchestrator, TargetSpec, WorkspaceTargets,
};
use ls_index_model::Span;
use ls_semanticdb::DocFacts;
use ls_store::Store;

pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

pub fn sources_root() -> PathBuf {
    fixtures_root().join("sources")
}

pub fn source_text(uri: &str) -> String {
    std::fs::read_to_string(sources_root().join(uri)).unwrap_or_else(|e| panic!("read {uri}: {e}"))
}

fn is_ident_part(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$'
}

/// All whole-word occurrences of `token`, in source order (0-based
/// line/char), mirroring the Scala `tokenSpans` helper. Sources are ASCII so
/// byte offsets equal character columns.
pub fn token_spans(uri: &str, token: &str) -> Vec<Span> {
    let text = source_text(uri);
    let mut out = Vec::new();
    for (ln, line) in text.lines().enumerate() {
        let bytes = line.as_bytes();
        let mut from = 0;
        while let Some(rel) = line[from..].find(token) {
            let i = from + rel;
            let before_ok = i == 0 || !is_ident_part(bytes[i - 1] as char);
            let after = i + token.len();
            let after_ok = after >= line.len() || !is_ident_part(bytes[after] as char);
            if before_ok && after_ok {
                out.push(Span::new(ln as u32, i as u32, ln as u32, after as u32));
            }
            from = i + 1;
        }
    }
    out
}

pub fn token_span(uri: &str, token: &str, nth: usize) -> Span {
    let spans = token_spans(uri, token);
    spans
        .get(nth)
        .copied()
        .unwrap_or_else(|| panic!("token '{token}' (occurrence {nth}) not found in {uri}"))
}

/// Cursor position (line, character) inside the nth occurrence of `token`.
pub fn cursor(uri: &str, token: &str, nth: usize) -> (u32, u32) {
    let span = token_span(uri, token, nth);
    (span.start_line, span.start_char + 1)
}

/// DocFacts for target A: the generated/readonly/dependency-marked fixtures.
pub fn facts_a(uri: &str) -> DocFacts {
    if uri == "a/src/pkga/GeneratedUse.scala" {
        DocFacts {
            generated: true,
            readonly: false,
            is_dependency_source: false,
        }
    } else if uri == "a/src/pkga/ReadonlyUse.scala" {
        DocFacts {
            generated: false,
            readonly: true,
            is_dependency_source: false,
        }
    } else if uri.starts_with("dep/") {
        DocFacts {
            generated: false,
            readonly: false,
            is_dependency_source: true,
        }
    } else {
        DocFacts::workspace_source()
    }
}

pub fn workspace_for() -> Arc<WorkspaceTargets> {
    let fx = fixtures_root();
    let src = sources_root();
    Arc::new(WorkspaceTargets::new(vec![
        TargetSpec::new("fixture-a", fx.join("out-a"), src.clone())
            .with_doc_facts(Arc::new(facts_a)),
        TargetSpec::new("fixture-b", fx.join("out-b"), src.clone())
            .with_deps(vec!["fixture-a".to_string()]),
        TargetSpec::new("fixture-c", fx.join("out-c"), src),
    ]))
}

/// A store stack over the master corpus: an isolated store dir + an
/// orchestrator that has ingested the workspace once.
pub struct Stack {
    _dir: tempfile::TempDir,
    pub orch: QueryOrchestrator,
    pub report: IngestReport,
}

pub fn new_stack() -> Stack {
    build_stack(QueryOrchestrator::with_defaults)
}

pub fn new_stack_with_overlay(overlay: Box<dyn DirtyBufferOverlay + Send + Sync>) -> Stack {
    build_stack(move |store| QueryOrchestrator::new(store, overlay, true))
}

fn build_stack(make: impl FnOnce(Store) -> QueryOrchestrator) -> Stack {
    let dir = tempfile::tempdir().unwrap();
    let store = Store::open(dir.path()).unwrap();
    let orch = make(store);
    let report = orch.ingest(workspace_for()).unwrap();
    Stack {
        _dir: dir,
        orch,
        report,
    }
}

/// The whole corpus source uri set size (docs indexed count).
pub const SOURCE_COUNT: usize = 25;
