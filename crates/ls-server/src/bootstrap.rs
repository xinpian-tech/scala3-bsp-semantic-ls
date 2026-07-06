//! The production workspace bootstrap: from the build model to the ready,
//! index-backed services.
//!
//! Ports the index-relevant subset of `ls.core.WorkspaceState.loadModel` plus
//! `ls.rename.ingest.WorkspaceTargets.fromBsp`. [`from_bsp`] maps the build
//! server's project model to the ingest pipeline's [`WorkspaceTargets`];
//! [`IndexBootstrap`] opens the store under the workspace root, runs the initial
//! ingest, and assembles the ready [`CoreServices`]. It answers the [`Bootstrap`]
//! seam the message loop drives on `initialized`.
//!
//! The build model is supplied by an injected [`ModelSource`] — the live BSP
//! session in production, a fixture model in tests — so the model-to-ready
//! assembly is exercised without a live build server or an embedded JVM. The PC
//! island and the live session attach through the model source and, as the
//! compiler/PC-backed methods land, an expanded services bundle.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ls_bsp::model::BspProjectModel;
use ls_bsp::{BspClientHandlers, BspDiscovery, BspSession, BspSessionConfig, ProjectModelLoader};
use ls_engine::{DocFacts, QueryOrchestrator, TargetSpec, WorkspaceTargets};
use ls_store::Store;

use crate::lifecycle::WorkspaceState;
use crate::server::{Bootstrap, BootstrapContext};
use crate::services::CoreServices;
use crate::workspace_uris::WorkspaceUris;

/// The directory under the workspace root that holds the index store — the
/// manifest, immutable segments, and generational state files that replaced
/// SQLite. Matches the Scala `settings` `workspaceRoot.resolve(".scala3-bsp-semantic-ls")`.
const STORE_DIR: &str = ".scala3-bsp-semantic-ls";

/// Per-document generated/readonly/dependency-source knowledge keyed by
/// `(bspId, sdbUri)` — the ingest's `contributesOccurrences` profile input.
/// Ports the Scala `WorkspaceTargets.fromBsp` `docFacts` parameter.
pub type BspDocFacts = Arc<dyn Fn(&str, &str) -> DocFacts + Send + Sync>;

/// The default per-document facts: every source is a plain workspace source.
/// Matches the Scala `loadModel`, which builds `WorkspaceTargets.fromBsp(m)`
/// with the default `(_, _) => DocFacts.workspaceSource`; the generated/readonly/
/// dependency-source refinement from `dependencySources`/`outputPaths` is
/// best-effort project info that attaches with the live session.
pub fn workspace_source_facts() -> BspDocFacts {
    Arc::new(|_, _| DocFacts::workspace_source())
}

/// Maps the BSP project model to the ingest pipeline's [`WorkspaceTargets`]: one
/// [`TargetSpec`] per indexable target that carries both a SemanticDB targetroot
/// and a sourceroot, in model order. A target missing either root is skipped —
/// without a targetroot its `.semanticdb` cannot be located, and without a
/// sourceroot its occurrences cannot be made sourceroot-relative. Ports
/// `WorkspaceTargets.fromBsp`.
pub fn from_bsp(model: &BspProjectModel, doc_facts: BspDocFacts) -> WorkspaceTargets {
    let specs = model
        .indexable_targets()
        .into_iter()
        .filter_map(|t| {
            let sdb_root = t.semanticdb_root.clone()?;
            let src_root = t.sourceroot.clone()?;
            let bsp_id = t.bsp_id.clone();
            let facts = doc_facts.clone();
            let mut spec = TargetSpec::new(bsp_id.clone(), sdb_root, src_root)
                .with_deps(t.direct_deps.clone())
                .with_doc_facts(Arc::new(move |uri| facts(&bsp_id, uri)));
            spec.scala_version = t.scala_version.clone();
            Some(spec)
        })
        .collect();
    WorkspaceTargets::new(specs)
}

/// A source of the build project model for a workspace root. In production it
/// connects to the build server and loads the model (`BspDiscovery` +
/// `BspSession` + `ProjectModelLoader`); tests inject a fixture. A load error is
/// carried as a human-readable detail for the failed-bootstrap state.
pub trait ModelSource {
    fn load(&self, workspace_root: &Path) -> Result<BspProjectModel, String>;
}

impl<F> ModelSource for F
where
    F: Fn(&Path) -> Result<BspProjectModel, String>,
{
    fn load(&self, workspace_root: &Path) -> Result<BspProjectModel, String> {
        self(workspace_root)
    }
}

/// The production index-backed bootstrap: loads the build model, maps it to
/// [`WorkspaceTargets`], opens the store under the workspace root, runs the
/// initial ingest, and assembles the ready [`CoreServices`]. Ports the
/// index-relevant path of `WorkspaceState.loadModel` reaching `Ready`.
///
/// PC-target registration, the compiler, and diagnostics publishing attach as
/// their methods land; this stands up the query surface the engine already
/// answers over the snapshot (references, documentHighlight, workspace/symbol).
pub struct IndexBootstrap<M> {
    model_source: M,
}

impl<M: ModelSource> IndexBootstrap<M> {
    pub fn new(model_source: M) -> Self {
        IndexBootstrap { model_source }
    }

    /// Loads the model, ingests it into a fresh store under the root, and returns
    /// the assembled services, or a human-readable detail on the first failure.
    fn build(&self, workspace_root: &Path) -> Result<CoreServices, String> {
        let model = self.model_source.load(workspace_root)?;
        let workspace = from_bsp(&model, workspace_source_facts());
        // Sourceroots for the URI mapping are collected before the targets move
        // into the ingest; `WorkspaceUris` de-duplicates and orders them.
        let sourceroots: Vec<PathBuf> = workspace
            .targets
            .iter()
            .map(|t| t.sourceroot.clone())
            .collect();
        let store = Store::open(&workspace_root.join(STORE_DIR)).map_err(|e| e.to_string())?;
        let orchestrator = QueryOrchestrator::with_defaults(store);
        orchestrator
            .ingest(Arc::new(workspace))
            .map_err(|e| e.to_string())?;
        let uris = WorkspaceUris::new(&sourceroots);
        Ok(CoreServices::new(
            orchestrator,
            uris,
            Some(workspace_root.to_path_buf()),
        ))
    }
}

impl<M: ModelSource> Bootstrap<CoreServices> for IndexBootstrap<M> {
    fn run(&self, cx: BootstrapContext<'_>) -> WorkspaceState<CoreServices> {
        let Some(root) = cx.workspace_root else {
            return WorkspaceState::Failed {
                detail: "no workspace root in the initialize params".to_string(),
            };
        };
        match self.build(root) {
            Ok(services) => WorkspaceState::Ready(services),
            Err(detail) => WorkspaceState::Failed { detail },
        }
    }
}

/// The production [`ModelSource`]: discover the workspace's build server, launch
/// and initialize a session, and load the project model. Ports the model-load
/// prefix of `WorkspaceState.loadModel` (`build/initialize` then
/// `ProjectModelLoader.load`). Every `BspError` becomes the failed-bootstrap
/// detail string.
///
/// The session is shut down once the model is loaded: the index-backed query
/// path answers from the ingested store and does not retain it. When compile /
/// reload land, the session moves into the services bundle instead of closing.
#[derive(Clone, Copy, Default)]
pub struct LiveBspModelSource;

impl LiveBspModelSource {
    pub fn new() -> Self {
        LiveBspModelSource
    }
}

impl ModelSource for LiveBspModelSource {
    fn load(&self, workspace_root: &Path) -> Result<BspProjectModel, String> {
        let connection = BspDiscovery::required(workspace_root).map_err(|e| e.to_string())?;
        let session = BspSession::launch(
            workspace_root.to_path_buf(),
            &connection.details,
            BspClientHandlers::new(),
            BspSessionConfig::default(),
        )
        .map_err(|e| e.to_string())?;
        // Load, then close the session cleanly whether or not the load
        // succeeded — a launched build server must not be left running.
        let model = load_project_model(&session);
        session.shutdown();
        session.close();
        model
    }
}

/// Initializes the session and loads the project model, mapping any `BspError`
/// to a detail string. Separated so the caller closes the session on either
/// outcome.
fn load_project_model(session: &BspSession) -> Result<BspProjectModel, String> {
    session.initialize().map_err(|e| e.to_string())?;
    ProjectModelLoader::load(session).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use ls_bsp::model::BspTarget;

    use crate::documents::DocumentStore;
    use crate::protocol::PublishDiagnosticsParams;

    fn target(bsp_id: &str, semanticdb_root: Option<&str>, sourceroot: Option<&str>) -> BspTarget {
        BspTarget {
            bsp_id: bsp_id.to_string(),
            display_name: bsp_id.to_string(),
            scala_version: "3.3.1".to_string(),
            scalac_options: Vec::new(),
            class_directory: PathBuf::from("/out").join(bsp_id),
            semanticdb_root: semanticdb_root.map(PathBuf::from),
            sourceroot: sourceroot.map(PathBuf::from),
            sources: Vec::new(),
            direct_deps: Vec::new(),
        }
    }

    fn bootstrap_context<'a>(
        root: Option<&'a Path>,
        documents: &'a DocumentStore,
        publish: &'a dyn Fn(PublishDiagnosticsParams),
        changed: &'a dyn Fn(),
    ) -> BootstrapContext<'a> {
        BootstrapContext {
            workspace_root: root,
            documents,
            publish_diagnostics: publish,
            on_build_targets_changed: changed,
        }
    }

    // Ports WorkspaceTargets.fromBsp: an indexable, doubly-rooted target maps to a
    // TargetSpec; a non-indexable target and an indexable-but-unrooted target are
    // both dropped; model order and deps/scalaVersion are preserved.
    #[test]
    fn from_bsp_maps_indexable_rooted_targets_only() {
        let model = BspProjectModel::new(
            vec![
                {
                    let mut t = target("a", Some("/out/a"), Some("/src"));
                    t.direct_deps = vec!["b".to_string()];
                    t
                },
                // No SemanticDB output -> not indexable -> dropped.
                target("b", None, Some("/src")),
                // Indexable but no sourceroot -> dropped (cannot relativize uris).
                target("c", Some("/out/c"), None),
                target("d", Some("/out/d"), Some("/src2")),
            ],
            HashMap::new(),
        );
        let ws = from_bsp(&model, workspace_source_facts());
        let ids: Vec<&str> = ws.targets.iter().map(|t| t.bsp_id.as_str()).collect();
        assert_eq!(ids, vec!["a", "d"]);
        assert_eq!(ws.targets[0].direct_deps, vec!["b".to_string()]);
        assert_eq!(ws.targets[0].scala_version, "3.3.1");
        assert_eq!(ws.targets[0].sourceroot, PathBuf::from("/src"));
        assert_eq!(ws.targets[0].semanticdb_root, PathBuf::from("/out/a"));
    }

    #[test]
    fn from_bsp_routes_doc_facts_by_target_and_uri() {
        let model = BspProjectModel::new(
            vec![target("a", Some("/out/a"), Some("/src"))],
            HashMap::new(),
        );
        // A fact function that marks one uri generated only under target "a".
        let facts: BspDocFacts = Arc::new(|bsp_id, uri| {
            if bsp_id == "a" && uri == "gen.scala" {
                DocFacts {
                    generated: true,
                    readonly: false,
                    is_dependency_source: false,
                }
            } else {
                DocFacts::workspace_source()
            }
        });
        let ws = from_bsp(&model, facts);
        assert!(ws.targets[0].facts("gen.scala").generated);
        assert!(!ws.targets[0].facts("other.scala").generated);
    }

    #[test]
    fn bootstrap_without_a_workspace_root_fails() {
        let bootstrap = IndexBootstrap::new(|_root: &Path| {
            Ok(BspProjectModel::new(Vec::new(), HashMap::new()))
        });
        let documents = DocumentStore::new();
        let publish = |_p: PublishDiagnosticsParams| {};
        let changed = || {};
        let state = bootstrap.run(bootstrap_context(None, &documents, &publish, &changed));
        assert!(matches!(state, WorkspaceState::Failed { .. }));
    }

    #[test]
    fn bootstrap_over_an_empty_model_is_ready_on_an_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = IndexBootstrap::new(|_root: &Path| {
            Ok(BspProjectModel::new(Vec::new(), HashMap::new()))
        });
        let documents = DocumentStore::new();
        let publish = |_p: PublishDiagnosticsParams| {};
        let changed = || {};
        let state = bootstrap.run(bootstrap_context(
            Some(dir.path()),
            &documents,
            &publish,
            &changed,
        ));
        let services = match state {
            WorkspaceState::Ready(s) => s,
            other => panic!("expected Ready, got {:?}", other.status_line()),
        };
        // The store landed under the workspace root, and an empty index knows no
        // symbols.
        assert!(dir.path().join(STORE_DIR).is_dir());
        assert!(services
            .orchestrator
            .workspace_symbols("Anything", 10)
            .is_empty());
    }

    // The live model source fails cleanly (not a panic) when the workspace has
    // no BSP connection file, surfacing the discovery error as the detail.
    #[test]
    fn live_model_source_without_a_connection_file_errors() {
        let dir = tempfile::tempdir().unwrap();
        let err = LiveBspModelSource::new()
            .load(dir.path())
            .expect_err("no .bsp connection file should fail");
        assert!(err.contains(".bsp"), "{err}");
    }

    #[test]
    fn bootstrap_propagates_a_model_load_failure() {
        let dir = tempfile::tempdir().unwrap();
        let bootstrap =
            IndexBootstrap::new(|_root: &Path| Err("no build server found".to_string()));
        let documents = DocumentStore::new();
        let publish = |_p: PublishDiagnosticsParams| {};
        let changed = || {};
        let state = bootstrap.run(bootstrap_context(
            Some(dir.path()),
            &documents,
            &publish,
            &changed,
        ));
        match state {
            WorkspaceState::Failed { detail } => assert!(detail.contains("no build server")),
            other => panic!("expected Failed, got {:?}", other.status_line()),
        }
    }
}
