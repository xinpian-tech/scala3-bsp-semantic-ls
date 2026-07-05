//! The workspace description consumed by the ingest pipeline: the indexable
//! build targets in a deterministic order plus the dependency edges between them.
//!
//! `semanticdb_root` is the SemanticDB *targetroot* (the locator appends
//! `META-INF/semanticdb` itself); `sourceroot` is the root that
//! `TextDocument.uri` values are relative to. `doc_facts` supplies the
//! per-document generated/readonly/dependency-source knowledge, keyed by the
//! SemanticDB uri (sourceroot-relative, forward slashes).

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;

use ls_semanticdb::DocFacts;

pub type DocFactsFn = Arc<dyn Fn(&str) -> DocFacts + Send + Sync>;

#[derive(Clone)]
pub struct TargetSpec {
    pub bsp_id: String,
    pub semanticdb_root: PathBuf,
    pub sourceroot: PathBuf,
    pub direct_deps: Vec<String>,
    pub scala_version: String,
    pub content_hash: i64,
    pub options_hash: i64,
    pub doc_facts: DocFactsFn,
}

impl TargetSpec {
    pub fn new(
        bsp_id: impl Into<String>,
        semanticdb_root: impl Into<PathBuf>,
        sourceroot: impl Into<PathBuf>,
    ) -> Self {
        TargetSpec {
            bsp_id: bsp_id.into(),
            semanticdb_root: semanticdb_root.into(),
            sourceroot: sourceroot.into(),
            direct_deps: Vec::new(),
            scala_version: "3".to_string(),
            content_hash: 0,
            options_hash: 0,
            doc_facts: Arc::new(|_| DocFacts::workspace_source()),
        }
    }

    pub fn with_deps(mut self, deps: Vec<String>) -> Self {
        self.direct_deps = deps;
        self
    }

    pub fn with_doc_facts(mut self, f: DocFactsFn) -> Self {
        self.doc_facts = f;
        self
    }

    pub fn facts(&self, uri: &str) -> DocFacts {
        (self.doc_facts)(uri)
    }
}

pub struct WorkspaceTargets {
    pub targets: Vec<TargetSpec>,
    by_id: HashMap<String, usize>,
    reverse_edges: HashMap<String, Vec<String>>,
}

impl WorkspaceTargets {
    /// Panics on a duplicate `bsp_id` (a programming error, as in the Scala
    /// `require`).
    pub fn new(targets: Vec<TargetSpec>) -> Self {
        let mut by_id = HashMap::new();
        for (i, t) in targets.iter().enumerate() {
            assert!(
                by_id.insert(t.bsp_id.clone(), i).is_none(),
                "duplicate bspId in workspace targets"
            );
        }
        let mut acc: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for t in &targets {
            let mut seen = HashSet::new();
            for dep in &t.direct_deps {
                if !seen.insert(dep.clone()) {
                    continue;
                }
                if by_id.contains_key(dep) {
                    acc.entry(dep.clone()).or_default().push(t.bsp_id.clone());
                }
            }
        }
        let reverse_edges = acc
            .into_iter()
            .map(|(k, mut v)| {
                v.sort();
                (k, v)
            })
            .collect();
        WorkspaceTargets {
            targets,
            by_id,
            reverse_edges,
        }
    }

    pub fn empty() -> Self {
        WorkspaceTargets::new(Vec::new())
    }

    pub fn spec_of(&self, bsp_id: &str) -> Option<&TargetSpec> {
        self.by_id.get(bsp_id).map(|&i| &self.targets[i])
    }

    pub fn dependents_of(&self, bsp_id: &str) -> Vec<String> {
        self.reverse_edges.get(bsp_id).cloned().unwrap_or_default()
    }

    /// `bsp_id` plus every target that transitively depends on it: the exact
    /// upper bound of targets that can reference a symbol defined in `bsp_id`.
    /// Empty for unknown ids.
    pub fn reverse_dependency_closure(&self, bsp_id: &str) -> HashSet<String> {
        self.closure(bsp_id, |id| self.dependents_of(id))
    }

    /// `bsp_id` plus every target it transitively depends on (via `direct_deps`):
    /// the exact set of targets a source in `bsp_id` can SEE. Empty for unknown
    /// ids.
    pub fn forward_dependency_closure(&self, bsp_id: &str) -> HashSet<String> {
        self.closure(bsp_id, |id| {
            self.spec_of(id)
                .map(|t| {
                    t.direct_deps
                        .iter()
                        .filter(|d| self.by_id.contains_key(*d))
                        .cloned()
                        .collect()
                })
                .unwrap_or_default()
        })
    }

    fn closure(&self, bsp_id: &str, next: impl Fn(&str) -> Vec<String>) -> HashSet<String> {
        if !self.by_id.contains_key(bsp_id) {
            return HashSet::new();
        }
        let mut seen = HashSet::new();
        seen.insert(bsp_id.to_string());
        let mut queue = VecDeque::new();
        queue.push_back(bsp_id.to_string());
        while let Some(cur) = queue.pop_front() {
            for nxt in next(&cur) {
                if seen.insert(nxt.clone()) {
                    queue.push_back(nxt);
                }
            }
        }
        seen
    }

    /// DocFacts for `uri` in target `bsp_id`; workspace-source default when the
    /// target is unknown.
    pub fn facts_for(&self, bsp_id: &str, uri: &str) -> DocFacts {
        self.spec_of(bsp_id)
            .map(|t| t.facts(uri))
            .unwrap_or_else(DocFacts::workspace_source)
    }
}
