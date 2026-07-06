//! The assembled BSP project model: Scala 3 targets, the source-file-uri ->
//! bspId map, and exact dependency-graph queries.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::path::PathBuf;

use ls_index_model::LsError;

/// One Scala 3 build target as this LS sees it. `semanticdb_root` is the
/// SemanticDB targetroot; None marks the target IndexUnavailable (no global
/// workspace-symbol / references / rename for it).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BspTarget {
    pub bsp_id: String,
    pub display_name: String,
    pub scala_version: String,
    pub scalac_options: Vec<String>,
    pub class_directory: PathBuf,
    /// The target's compile classpath (from `buildTarget/scalacOptions`),
    /// resolved to filesystem paths. Feeds the presentation compiler's per-target
    /// config; the global index does not use it.
    pub classpath: Vec<PathBuf>,
    pub semanticdb_root: Option<PathBuf>,
    pub sourceroot: Option<PathBuf>,
    pub sources: Vec<PathBuf>,
    pub direct_deps: Vec<String>,
}

impl BspTarget {
    pub fn indexable(&self) -> bool {
        self.semanticdb_root.is_some()
    }
}

/// `directDeps` may mention targets that were filtered out (non-Scala-3); graph
/// queries only traverse targets present in the model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BspProjectModel {
    pub targets: Vec<BspTarget>,
    pub uri_to_target: HashMap<String, String>,
    by_id: HashMap<String, usize>,
    reverse_edges: HashMap<String, Vec<String>>,
}

impl BspProjectModel {
    pub fn new(targets: Vec<BspTarget>, uri_to_target: HashMap<String, String>) -> Self {
        let by_id: HashMap<String, usize> = targets
            .iter()
            .enumerate()
            .map(|(i, t)| (t.bsp_id.clone(), i))
            .collect();

        // Reverse edges over deps that resolve to targets in the model, sorted.
        let mut acc: HashMap<String, Vec<String>> = HashMap::new();
        for t in &targets {
            let mut seen: BTreeSet<&str> = BTreeSet::new();
            for dep in &t.direct_deps {
                if seen.insert(dep.as_str()) && by_id.contains_key(dep) {
                    acc.entry(dep.clone()).or_default().push(t.bsp_id.clone());
                }
            }
        }
        for v in acc.values_mut() {
            v.sort();
        }

        BspProjectModel {
            targets,
            uri_to_target,
            by_id,
            reverse_edges: acc,
        }
    }

    pub fn target_for(&self, bsp_id: &str) -> Option<&BspTarget> {
        self.by_id.get(bsp_id).map(|&i| &self.targets[i])
    }

    pub fn target_of_uri(&self, uri: &str) -> Option<&BspTarget> {
        self.uri_to_target
            .get(uri)
            .and_then(|id| self.target_for(id))
    }

    /// Direct dependencies restricted to targets known to the model, sorted.
    pub fn dependencies_of(&self, bsp_id: &str) -> Vec<String> {
        match self.target_for(bsp_id) {
            Some(t) => {
                let mut deps: Vec<String> = t
                    .direct_deps
                    .iter()
                    .filter(|d| self.by_id.contains_key(*d))
                    .cloned()
                    .collect();
                deps.sort();
                deps.dedup();
                deps
            }
            None => Vec::new(),
        }
    }

    /// Direct dependents (targets that list `bsp_id` as a dependency), sorted.
    pub fn dependents_of(&self, bsp_id: &str) -> Vec<String> {
        self.reverse_edges.get(bsp_id).cloned().unwrap_or_default()
    }

    /// `bsp_id` plus every target that transitively depends on it: the exact
    /// upper bound of targets that can reference a symbol defined in `bsp_id`.
    /// Exact BFS over the reverse edges; empty for unknown ids.
    pub fn reverse_dependency_closure(&self, bsp_id: &str) -> BTreeSet<String> {
        if !self.by_id.contains_key(bsp_id) {
            return BTreeSet::new();
        }
        let mut seen: BTreeSet<String> = BTreeSet::new();
        seen.insert(bsp_id.to_string());
        let mut queue: VecDeque<String> = VecDeque::new();
        queue.push_back(bsp_id.to_string());
        while let Some(current) = queue.pop_front() {
            for dependent in self.dependents_of(&current) {
                if seen.insert(dependent.clone()) {
                    queue.push_back(dependent);
                }
            }
        }
        seen
    }

    /// Targets that produce SemanticDB and participate in the global index.
    pub fn indexable_targets(&self) -> Vec<&BspTarget> {
        self.targets.iter().filter(|t| t.indexable()).collect()
    }

    /// Targets without SemanticDB output; global features are disabled there.
    pub fn unavailable_targets(&self) -> Vec<&BspTarget> {
        self.targets.iter().filter(|t| !t.indexable()).collect()
    }

    /// IndexUnavailable errors for every non-indexable target.
    pub fn unavailable_errors(&self) -> Vec<LsError> {
        self.unavailable_targets()
            .into_iter()
            .map(|t| LsError::IndexUnavailable {
                target: t.bsp_id.clone(),
            })
            .collect()
    }
}
