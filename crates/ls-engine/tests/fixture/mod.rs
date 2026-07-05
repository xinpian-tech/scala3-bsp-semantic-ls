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

/// The three-target workspace rooted at an arbitrary fixtures dir (its `out-a`/
/// `out-b`/`out-c` SemanticDB roots) sharing one `sources` sourceroot.
pub fn workspace_at(fx_root: &Path, src: &Path) -> Arc<WorkspaceTargets> {
    Arc::new(WorkspaceTargets::new(vec![
        TargetSpec::new("fixture-a", fx_root.join("out-a"), src.to_path_buf())
            .with_doc_facts(Arc::new(facts_a)),
        TargetSpec::new("fixture-b", fx_root.join("out-b"), src.to_path_buf())
            .with_deps(vec!["fixture-a".to_string()]),
        TargetSpec::new("fixture-c", fx_root.join("out-c"), src.to_path_buf()),
    ]))
}

pub fn workspace_for() -> Arc<WorkspaceTargets> {
    workspace_at(&fixtures_root(), &sources_root())
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

// ---- mutable temp copy (for rename mutation tests) ----

fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&from, &to);
        } else {
            std::fs::copy(&from, &to).unwrap();
        }
    }
}

/// An isolated deep copy of the committed corpus (sources + `out-a`/`out-b`/
/// `out-c`) under a temp dir, plus an orchestrator that has ingested it — for
/// tests that mutate sources or SemanticDB after ingest.
pub struct MutableStack {
    fx_root: tempfile::TempDir,
    _store: tempfile::TempDir,
    pub orch: QueryOrchestrator,
}

impl MutableStack {
    pub fn source_path(&self, uri: &str) -> PathBuf {
        self.fx_root.path().join("sources").join(uri)
    }

    pub fn semanticdb_path(&self, out: &str, uri: &str) -> PathBuf {
        self.fx_root
            .path()
            .join(out)
            .join("META-INF/semanticdb")
            .join(format!("{uri}.semanticdb"))
    }

    pub fn workspace(&self) -> Arc<WorkspaceTargets> {
        workspace_at(self.fx_root.path(), &self.fx_root.path().join("sources"))
    }
}

/// Deep-copies the committed corpus into temp dirs and ingests it once.
pub fn clone_and_ingest() -> MutableStack {
    let fx_root = tempfile::tempdir().unwrap();
    let src = fixtures_root();
    for name in ["sources", "out-a", "out-b", "out-c"] {
        copy_dir_recursive(&src.join(name), &fx_root.path().join(name));
    }
    let store = tempfile::tempdir().unwrap();
    let orch = QueryOrchestrator::with_defaults(Store::open(store.path()).unwrap());
    orch.ingest(workspace_at(
        fx_root.path(),
        &fx_root.path().join("sources"),
    ))
    .unwrap();
    MutableStack {
        fx_root,
        _store: store,
        orch,
    }
}
