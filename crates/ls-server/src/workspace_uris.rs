//! Workspace-aware conversion between `file://` URIs (what LSP speaks) and
//! SemanticDB URIs (sourceroot-relative, forward slashes — what the engine
//! speaks). A behavior-preserving port of `ls.core.WorkspaceUris` (and the
//! `ls.core.Uris.sdbUri`/`fromSdbUri` helpers it builds on).
//!
//! `to_sdb_uri` prefers the deepest sourceroot containing the path and, among
//! ambiguous roots, one whose relative URI is actually known to the metadata
//! store; `to_file_uri` asks the orchestrator (metadata truth) first and falls
//! back to probing the sourceroots for an existing file. The two
//! metadata-truth steps are injected as closures so the mapping logic is
//! exercised without a live index.

use std::path::{Path, PathBuf};

use ls_engine::QueryOrchestrator;
use ls_index_model::uri::{normalize, path_to_uri, uri_to_path};

/// The sourceroots of a ready workspace, sorted so lookups prefer the deepest
/// containing root.
#[derive(Clone)]
pub struct WorkspaceUris {
    /// Absolutized + normalized + distinct sourceroots, sorted deepest-first.
    roots: Vec<PathBuf>,
}

impl WorkspaceUris {
    /// Build from the workspace sourceroots. Roots are absolutized and lexically
    /// normalized, de-duplicated preserving first-occurrence order, then sorted
    /// by descending path depth (deepest first), matching
    /// `WorkspaceUris`'s `sortBy(-_.getNameCount)` over `distinct` roots.
    pub fn new(sourceroots: &[PathBuf]) -> WorkspaceUris {
        let mut roots: Vec<PathBuf> = Vec::new();
        for root in sourceroots {
            let root = abs_normalize(root);
            if !roots.contains(&root) {
                roots.push(root);
            }
        }
        // Stable sort keeps the first-occurrence order among equal-depth roots.
        roots.sort_by_key(|p| std::cmp::Reverse(p.components().count()));
        WorkspaceUris { roots }
    }

    /// `file://` URI -> SemanticDB URI, preferring a candidate the metadata store
    /// knows, else the deepest-root candidate. `None` when the path parses to no
    /// sourceroot-relative URI at all.
    pub fn to_sdb_uri(&self, file_uri: &str, orchestrator: &QueryOrchestrator) -> Option<String> {
        self.pick_sdb_uri(file_uri, |sdb| orchestrator.primary_spec_of(sdb).is_some())
    }

    /// SemanticDB URI -> `file://` URI: the orchestrator's absolute source path
    /// (metadata truth), else the first sourceroot under which the URI names an
    /// existing regular file.
    pub fn to_file_uri(&self, sdb_uri: &str, orchestrator: &QueryOrchestrator) -> Option<String> {
        self.resolve_file_uri(sdb_uri, |sdb| orchestrator.absolute_source_path(sdb))
    }

    /// The sourceroot-relative candidates for `file_uri`, deepest root first.
    /// Empty when the URI does not parse to a path (mirrors the Scala `try
    /// toPath … catch => return None`).
    fn candidates(&self, file_uri: &str) -> Vec<String> {
        let Ok(path) = uri_to_path(file_uri) else {
            return Vec::new();
        };
        self.roots
            .iter()
            .filter_map(|root| sdb_uri(root, &path))
            .collect()
    }

    /// `to_sdb_uri` with the metadata-membership check injected: prefer the first
    /// candidate `is_known` accepts, else the deepest-root candidate.
    fn pick_sdb_uri(&self, file_uri: &str, is_known: impl Fn(&str) -> bool) -> Option<String> {
        let candidates = self.candidates(file_uri);
        candidates
            .iter()
            .find(|sdb| is_known(sdb))
            .cloned()
            .or_else(|| candidates.into_iter().next())
    }

    /// `to_file_uri` with the metadata absolute-source resolver injected: use it
    /// first, else probe the sourceroots for an existing file.
    fn resolve_file_uri(
        &self,
        sdb_uri: &str,
        absolute_source: impl Fn(&str) -> Option<PathBuf>,
    ) -> Option<String> {
        absolute_source(sdb_uri)
            .map(|p| path_to_uri(&abs_normalize(&p)))
            .or_else(|| {
                self.roots
                    .iter()
                    .map(|root| from_sdb_uri(root, sdb_uri))
                    .find(|p| p.is_file())
                    .map(|p| path_to_uri(&p))
            })
    }
}

/// SemanticDB URI of `absolute` under `sourceroot`, or `None` when the path is
/// not strictly inside the sourceroot. Forward slashes always (Linux-only, so
/// the platform separator already is `/`). Mirrors `Uris.sdbUri`.
fn sdb_uri(sourceroot: &Path, absolute: &Path) -> Option<String> {
    let root = abs_normalize(sourceroot);
    let abs = abs_normalize(absolute);
    if abs == root {
        return None;
    }
    let rel = abs.strip_prefix(&root).ok()?;
    Some(
        rel.to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/"),
    )
}

/// Absolute path of a SemanticDB URI under a sourceroot. Mirrors
/// `Uris.fromSdbUri`.
fn from_sdb_uri(sourceroot: &Path, sdb_uri: &str) -> PathBuf {
    abs_normalize(&sourceroot.join(sdb_uri))
}

/// `Path.toAbsolutePath.normalize`: make absolute against the process working
/// directory if relative, then collapse `.`/`..` lexically. Workspace
/// sourceroots and `file://` paths are already absolute, so the working
/// directory is only consulted for a relative input.
fn abs_normalize(path: &Path) -> PathBuf {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    };
    normalize(&absolute)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sorts_roots_deepest_first_and_dedups() {
        let uris = WorkspaceUris::new(&[
            PathBuf::from("/ws/a"),
            PathBuf::from("/ws/a/b/c"),
            PathBuf::from("/ws/a"), // duplicate
            PathBuf::from("/ws/a/b"),
        ]);
        assert_eq!(
            uris.roots,
            vec![
                PathBuf::from("/ws/a/b/c"),
                PathBuf::from("/ws/a/b"),
                PathBuf::from("/ws/a"),
            ]
        );
    }

    #[test]
    fn sdb_uri_is_relative_inside_the_root_and_none_otherwise() {
        let root = Path::new("/ws/a");
        assert_eq!(
            sdb_uri(root, Path::new("/ws/a/src/Foo.scala")).as_deref(),
            Some("src/Foo.scala")
        );
        // The root itself is not a document under the root.
        assert_eq!(sdb_uri(root, Path::new("/ws/a")), None);
        // Outside the root.
        assert_eq!(sdb_uri(root, Path::new("/ws/other/Foo.scala")), None);
        // A `..` spelling normalizes before the containment test.
        assert_eq!(
            sdb_uri(root, Path::new("/ws/a/src/../src/Foo.scala")).as_deref(),
            Some("src/Foo.scala")
        );
    }

    #[test]
    fn from_sdb_uri_joins_and_normalizes_under_the_root() {
        assert_eq!(
            from_sdb_uri(Path::new("/ws/a"), "src/Foo.scala"),
            PathBuf::from("/ws/a/src/Foo.scala")
        );
    }

    #[test]
    fn candidates_are_deepest_root_first_and_empty_for_a_non_file_uri() {
        let uris = WorkspaceUris::new(&[PathBuf::from("/ws"), PathBuf::from("/ws/mod")]);
        // A file under both roots yields both candidates, deepest root first.
        assert_eq!(
            uris.candidates("file:///ws/mod/src/Foo.scala"),
            vec!["src/Foo.scala".to_string(), "mod/src/Foo.scala".to_string()]
        );
        assert!(uris.candidates("untitled:Untitled-1").is_empty());
    }

    // A non-empty-authority `file://host/...` URI is unmappable: `uri_to_path`
    // rejects the authority (Java `Path.of(URI)` parity), so it yields no
    // sourceroot-relative candidate rather than mapping to the local `/...` path.
    #[test]
    fn candidates_reject_a_non_empty_authority_uri() {
        let uris = WorkspaceUris::new(&[PathBuf::from("/ws")]);
        assert!(uris.candidates("file://host/ws/A.scala").is_empty());
    }

    #[test]
    fn pick_sdb_uri_prefers_a_known_candidate_else_the_deepest() {
        let uris = WorkspaceUris::new(&[PathBuf::from("/ws"), PathBuf::from("/ws/mod")]);
        // No candidate is known: fall back to the deepest root's candidate.
        assert_eq!(
            uris.pick_sdb_uri("file:///ws/mod/src/Foo.scala", |_| false)
                .as_deref(),
            Some("src/Foo.scala")
        );
        // The shallower candidate is the one the store knows: prefer it over the
        // deeper-but-unknown one.
        assert_eq!(
            uris.pick_sdb_uri("file:///ws/mod/src/Foo.scala", |sdb| sdb
                == "mod/src/Foo.scala")
                .as_deref(),
            Some("mod/src/Foo.scala")
        );
        // Not under any root.
        assert_eq!(
            uris.pick_sdb_uri("file:///elsewhere/Foo.scala", |_| true),
            None
        );
    }

    #[test]
    fn resolve_file_uri_uses_the_resolver_then_probes_roots() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/Foo.scala"), "x").unwrap();
        let roots = [root.clone()];
        let uris = WorkspaceUris::new(&roots);

        // The injected resolver (metadata truth) wins when it answers.
        let via_resolver =
            uris.resolve_file_uri("src/Foo.scala", |_| Some(PathBuf::from("/other/Bar.scala")));
        assert_eq!(via_resolver.as_deref(), Some("file:///other/Bar.scala"));

        // With no resolver answer, probe the roots for an existing file.
        let via_probe = uris.resolve_file_uri("src/Foo.scala", |_| None);
        assert_eq!(via_probe, Some(path_to_uri(&root.join("src/Foo.scala"))));

        // A URI that names no existing file under any root is unresolvable.
        assert_eq!(uris.resolve_file_uri("src/Missing.scala", |_| None), None);
    }
}
