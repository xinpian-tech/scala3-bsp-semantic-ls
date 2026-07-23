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
use ls_bsp::protocol::PublishDiagnosticsParams as BspPublishDiagnosticsParams;
use ls_bsp::{
    BspClientHandlers, BspCompileOutcome, BspDiscovery, BspSession, BspSessionConfig,
    ProjectModelLoader,
};
use ls_engine::{
    CompileOutcome, CompileService, DocFacts, MethodHit, QueryOrchestrator, TargetSpec,
    WorkspaceTargets,
};
use ls_index_model::uri::normalize_uri;
use ls_index_model::Loc;
use ls_pc_abi::payloads::{
    origin, LocationsResult, MethodHitsResult, Rng, TargetConfig, ToplevelsResult,
};
use ls_store::Store;

use crate::doctor::DoctorTargets;
use crate::documents::DocumentStore;
use crate::lifecycle::WorkspaceState;
use crate::pc::{
    pc_options, IslandPcService, PcQueryService, SearchMethodsResolver, SymbolResolver,
    ToplevelsResolver,
};
use crate::pc_diagnostics::PcDiagnosticsLayer;
use crate::pc_overlay::PcOverlay;
use crate::server::Bootstrap;
use crate::services::{BuildCompiler, CoreServices, UnavailableCompiler};
use crate::workspace_uris::WorkspaceUris;

/// The directory under the workspace root that holds the index store — the
/// manifest, immutable segments, and generational state files that replaced
/// SQLite. Matches the Scala `settings` `workspaceRoot.resolve(".scala3-bsp-semantic-ls")`.
/// Public so the doctor `Store` section derives the same store path from a
/// workspace root.
pub const STORE_DIR: &str = ".scala3-bsp-semantic-ls";

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

/// Distills the doctor `BSP`/`SemanticDB` target inventory from the full project
/// model — ALL Scala 3 targets, which of them lack SemanticDB output, and the
/// indexable targetroots — so the doctor reports off the same inventory the Scala
/// `DoctorCommand` reads (`model.targets` + `model.unavailableTargets`), not the
/// indexable-only ingest view. Boots nothing; pure data.
fn doctor_targets_of(model: &BspProjectModel) -> DoctorTargets {
    let mut all_ids: Vec<String> = model
        .indexable_targets()
        .iter()
        .chain(model.unavailable_targets().iter())
        .map(|t| t.bsp_id.clone())
        .collect();
    all_ids.sort();
    let mut unavailable_ids: Vec<String> = model
        .unavailable_targets()
        .iter()
        .map(|t| t.bsp_id.clone())
        .collect();
    unavailable_ids.sort();
    let mut indexable_roots: Vec<(String, PathBuf)> = model
        .indexable_targets()
        .iter()
        .filter_map(|t| {
            t.semanticdb_root
                .clone()
                .map(|root| (t.bsp_id.clone(), root))
        })
        .collect();
    indexable_roots.sort_by(|a, b| a.0.cmp(&b.0));
    DoctorTargets {
        all_ids,
        unavailable_ids,
        indexable_roots,
        ..Default::default()
    }
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

/// The presentation-compiler target registrations, one per doubly-rooted
/// (indexable + sourceroot) target — the same set `from_bsp` ingests. A
/// non-doubly-rooted target's buffer is rejected by `requireSemanticdb` before
/// any PC request, so it needs no PC registration. Ports the `pcConfigs`
/// construction: the PC classpath is the target's dependency classpath PLUS its
/// own compiled output directory, deduped preserving order
/// (`(classpathOf… :+ t.classDirectory).distinct`) — the class directory lets
/// the PC resolve same-target symbols from sibling sources; the scalac options
/// are SemanticDB-stripped (so the PC does not re-emit SemanticDB); the source
/// path is empty (`sourceDirs = Vector.empty`), since the SemanticDB sourceroot
/// is the workspace root, not a source directory.
fn pc_target_configs(model: &BspProjectModel) -> Vec<TargetConfig> {
    model
        .indexable_targets()
        .into_iter()
        .filter(|t| t.sourceroot.is_some())
        .map(|t| {
            let mut classpath: Vec<String> = Vec::new();
            for path in t
                .classpath
                .iter()
                .chain(std::iter::once(&t.class_directory))
            {
                let entry = path_string(path);
                if !classpath.contains(&entry) {
                    classpath.push(entry);
                }
            }
            TargetConfig {
                bsp_id: t.bsp_id.clone(),
                scala_version: t.scala_version.clone(),
                classpath,
                scalac_options: pc_options(&t.scalac_options),
                source_dirs: Vec::new(),
            }
        })
        .collect()
}

/// Index definition locations -> the ABI `LocationsResult` the island's
/// `symbol_definition` resolver returns. The engine already emits `file://`
/// URIs; they are tagged workspace-origin.
fn locations_result(locations: Vec<Loc>) -> LocationsResult {
    LocationsResult {
        locations: locations
            .into_iter()
            .map(|loc| ls_pc_abi::payloads::Location {
                uri: loc.uri,
                range: Rng {
                    start_line: loc.span.start_line,
                    start_character: loc.span.start_char,
                    end_line: loc.span.end_line,
                    end_character: loc.span.end_char,
                },
                origin: origin::WORKSPACE,
            })
            .collect(),
    }
}

/// Index method-search hits -> the ABI `MethodHitsResult` the island's
/// `search_methods` resolver returns. The engine already emits absolute
/// `file://` URIs and raw SemanticDB symbols.
fn method_hits_result(hits: Vec<MethodHit>) -> MethodHitsResult {
    MethodHitsResult {
        hits: hits
            .into_iter()
            .map(|hit| ls_pc_abi::payloads::MethodHit {
                uri: hit.uri,
                symbol: hit.symbol,
                kind: hit.kind,
                range: Rng {
                    start_line: hit.span.start_line,
                    start_character: hit.span.start_char,
                    end_line: hit.span.end_line,
                    end_character: hit.span.end_char,
                },
            })
            .collect(),
    }
}

fn path_string(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// The build model plus the live compile capability the ready bundle retains.
/// In production the `compiler` owns the launched BSP session; index-only
/// injections carry the disconnected stub.
pub struct ReadyModel {
    pub model: BspProjectModel,
    pub compiler: Arc<dyn BuildCompiler>,
    /// The build server's `build/initialize` display name, threaded into the
    /// doctor `BSP` section. `None` for index-only injections with no session.
    pub server_name: Option<String>,
    /// The build server's `build/initialize` version.
    pub server_version: Option<String>,
}

/// The outcome of asking a [`ModelSource`] for a workspace's build model.
// A one-shot bootstrap return whose `Model` payload is destructured immediately;
// the `Model`/`NoBsp` size gap is irrelevant here and boxing would only add a
// heap allocation on the hot ready path (and obscure the `.map(LoadOutcome::Model)`
// call sites).
#[allow(clippy::large_enum_variant)]
pub enum LoadOutcome {
    /// A live BSP session produced a project model (with its retained compiler).
    Model(ReadyModel),
    /// No BSP connection is available (the `session = None` case of
    /// `WorkspaceState.run`). Serving the recovered index without a build
    /// connection is deferred, so the bootstrap declines rather than reaching
    /// `Ready`; this variant lets the source report that case distinctly from a
    /// connected-but-failed load.
    NoBsp,
}

/// A source of the build project model for a workspace root. In production it
/// discovers the build server and, when one is present, loads the model (via
/// `BspDiscovery`, `BspSession`, and `ProjectModelLoader`), handing the retained
/// session's compile capability to the ready bundle; with no connection it
/// yields [`LoadOutcome::NoBsp`] for the recovered-index mode. Tests inject a
/// fixture. A load error (a connected server that fails to load) is carried as a
/// human-readable detail for the failed-bootstrap state.
/// `Send + Sync` so an [`IndexBootstrap`] over the source can run on the bootstrap
/// worker thread.
pub trait ModelSource: Send + Sync {
    fn load(&self, workspace_root: &Path) -> Result<LoadOutcome, String>;
}

/// A closure that only produces the model gets the disconnected compiler, so an
/// index-only test/injection needs no build server. A closure never selects the
/// no-BSP recovered-index mode: it either produces a model or fails.
impl<F> ModelSource for F
where
    F: Fn(&Path) -> Result<BspProjectModel, String> + Send + Sync,
{
    fn load(&self, workspace_root: &Path) -> Result<LoadOutcome, String> {
        Ok(LoadOutcome::Model(ReadyModel {
            model: self(workspace_root)?,
            compiler: Arc::new(UnavailableCompiler),
            server_name: None,
            server_version: None,
        }))
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
    pc_factory: PcServiceFactory,
    /// The PC/BSP diagnostics merge layer the ready bundle's live-typing pull
    /// publishes through. `main` (and the wire harness) hands in the layer it
    /// also routes BSP publishes through; the default is disconnected —
    /// injected bundles that wire no sink publish nowhere.
    pc_diagnostics: Arc<PcDiagnosticsLayer>,
}

/// Builds the ready bundle's PC capability from the workspace root, the model's
/// PC target registrations, and the index-backed resolvers (the cross-file
/// `symbol_definition` resolver plus the `search_methods` workspace
/// method-search resolver). The default factory ([`IndexBootstrap::new`])
/// stands up the production embedded-island service; tests inject a JVM-free
/// fake through [`IndexBootstrap::with_pc`] so the PC-backed wire surface
/// (completion, hover, signature help, the definition family) runs through the
/// real serve loop without booting a JVM.
pub type PcServiceFactory = Arc<
    dyn Fn(
            PathBuf,
            Vec<TargetConfig>,
            Box<SymbolResolver>,
            Box<SearchMethodsResolver>,
            Box<ToplevelsResolver>,
        ) -> Arc<dyn PcQueryService>
        + Send
        + Sync,
>;

impl<M: ModelSource> IndexBootstrap<M> {
    pub fn new(model_source: M) -> Self {
        Self::with_pc(
            model_source,
            Arc::new(
                |root, targets, resolver, search_resolver, toplevels_resolver| {
                    Arc::new(IslandPcService::new(
                        root,
                        targets,
                        resolver,
                        search_resolver,
                        toplevels_resolver,
                    )) as Arc<dyn PcQueryService>
                },
            ),
        )
    }

    /// An `IndexBootstrap` whose ready bundle carries the PC service built by
    /// `pc_factory` instead of the production embedded island.
    pub fn with_pc(model_source: M, pc_factory: PcServiceFactory) -> Self {
        IndexBootstrap {
            model_source,
            pc_factory,
            pc_diagnostics: PcDiagnosticsLayer::disconnected(),
        }
    }

    /// Wire the shared PC/BSP diagnostics merge layer (the one the caller also
    /// routes `DiagnosticRouter` publishes through), so the ready bundle's
    /// live-typing pull publishes into the same merged stream.
    pub fn with_pc_diagnostics(mut self, pc_diagnostics: Arc<PcDiagnosticsLayer>) -> Self {
        self.pc_diagnostics = pc_diagnostics;
        self
    }

    /// Loads the model, ingests it into a fresh store under the root, and returns
    /// the assembled services, or a human-readable detail on the first failure.
    /// This is the heavy work the bootstrap worker runs off the message loop.
    fn build_services(&self, workspace_root: &Path) -> Result<CoreServices, String> {
        let ReadyModel {
            model,
            compiler,
            server_name,
            server_version,
        } =
            match self.model_source.load(workspace_root)? {
                LoadOutcome::Model(ready) => {
                    log_model_summary(&ready.model);
                    ready
                }
                // The no-BSP warm-restart mode over the recovered index is deferred
                // (see the deferral note in the cutover docs): the server requires a
                // build connection. Faithful recovered-index serving needs the target
                // dependency graph to scope references, but the persisted segment does
                // not carry it — a permissive fallback would answer references across
                // independent, identically-named symbols in unrelated targets. So this
                // fails cleanly rather than serving a divergent index.
                LoadOutcome::NoBsp => return Err(
                    "no build server connection found (the no-BSP warm-restart mode is deferred)"
                        .to_string(),
                ),
            };
        let workspace = from_bsp(&model, workspace_source_facts());
        // The model's URI ownership, keyed by normalized `file://` URI (as
        // `WorkspaceState` does with `Uris.normalize`), backs `requireSemanticdb`.
        let uri_to_target = model
            .uri_to_target
            .iter()
            .map(|(uri, bsp_id)| (normalize_uri(uri), bsp_id.clone()))
            .collect();
        // Sourceroots for the URI mapping are collected before the targets move
        // into the ingest; `WorkspaceUris` de-duplicates and orders them.
        let sourceroots: Vec<PathBuf> = workspace
            .targets
            .iter()
            .map(|t| t.sourceroot.clone())
            .collect();
        // The PC target registrations are built before the model is dropped.
        let pc_targets = pc_target_configs(&model);
        // The doctor's full-target inventory (all Scala 3 targets + unavailable +
        // indexable roots) is captured from the model before it is dropped, plus
        // the build server's initialize identity for the doctor `server:` line.
        let mut doctor_targets = doctor_targets_of(&model);
        doctor_targets.server_name = server_name;
        doctor_targets.server_version = server_version;
        let store_dir = workspace_root.join(STORE_DIR);
        let store = Store::open(&store_dir).map_err(|e| e.to_string())?;
        match store.current() {
            Some(snapshot) => log::info!(
                target: "boot",
                "store opened at {}: recovered generation {} (segment {})",
                store_dir.display(),
                snapshot.generation(),
                snapshot.segment_id(),
            ),
            None => log::info!(
                target: "boot",
                "store opened at {}: fresh (no prior index)",
                store_dir.display(),
            ),
        }
        // `Arc` because the PC island's cross-file `symbol_definition` resolver
        // answers from this same query engine. `with_defaults` is the production
        // orchestrator: `sync_write_through = true`, so a RawSemanticDBPath
        // resolution runs the full-generation ingest INLINE and clears
        // `needs_reindex` before returning (write-through parity; matches the
        // Scala `WorkspaceState` default `QueryOrchestrator(..., overlay)` with
        // `syncWriteThrough = true`). The ready services' build scheduler is only
        // the FALLBACK — `references_ok` enqueues a background reingest solely for
        // results that STILL carry `needs_reindex` (write-through unavailable/failed).
        // The production dirty-buffer overlay lives inside the orchestrator; its
        // late-bound environment is installed at Ready adoption (once `docs` and
        // the ready bundle exist). The retained `handle` lets the ready services
        // install it and answer `workspace/symbol`'s PC-only unsaved symbols.
        let overlay = PcOverlay::new();
        let pc_overlay = overlay.handle();
        let orchestrator = Arc::new(QueryOrchestrator::new(store, Box::new(overlay), true));
        let report = orchestrator
            .ingest(Arc::new(workspace))
            .map_err(|e| e.to_string())?;
        log::info!(
            target: "boot",
            "initial ingest complete — {}",
            crate::services::ingest_summary(&report)
        );
        let uris = WorkspaceUris::new(&sourceroots);
        // The cross-file `symbol_definition` resolver the PC island calls when it
        // has no in-buffer source position for a symbol: it answers from the
        // global index (with forward-closure pruning by the requesting buffer's
        // target), ports `WorkspaceState`'s `SymbolSearch.definition` wiring.
        let resolver_orchestrator = orchestrator.clone();
        let resolver: Box<SymbolResolver> = Box::new(move |symbol: &str, from_uri: &str| {
            locations_result(resolver_orchestrator.symbol_definition(symbol, from_uri))
        });
        // The workspace method-search resolver the PC island calls for
        // member-mode extension-method discovery (`SymbolSearch.searchMethods`):
        // it answers from the global index, pruned to the forward closure of the
        // requesting PC target — the second resolver closure next to
        // `symbol_definition`.
        let search_orchestrator = orchestrator.clone();
        let search_resolver: Box<SearchMethodsResolver> =
            Box::new(move |query: &str, bsp_target_id: &str| {
                method_hits_result(search_orchestrator.search_methods(query, bsp_target_id))
            });
        // The `definition_source_toplevels` resolver the PC island calls for the
        // toplevel symbols of a definition source (exhaustive-match case
        // ordering): it answers from the global index — the defining doc's
        // toplevels in source order, pruned to the forward closure of the
        // requesting buffer's target — the third resolver closure next to
        // `symbol_definition` and `search_methods`.
        let toplevels_orchestrator = orchestrator.clone();
        let toplevels_resolver: Box<ToplevelsResolver> =
            Box::new(move |symbol: &str, source_uri: &str| ToplevelsResult {
                symbols: toplevels_orchestrator.definition_source_toplevels(symbol, source_uri),
            });
        let pc: Arc<dyn PcQueryService> = (self.pc_factory)(
            workspace_root.to_path_buf(),
            pc_targets,
            resolver,
            search_resolver,
            toplevels_resolver,
        );
        Ok(CoreServices::new(
            orchestrator,
            uris,
            Some(workspace_root.to_path_buf()),
            uri_to_target,
            compiler,
            pc,
            // A live build model was loaded, so a BSP session backs this
            // workspace; the persisted-index fallback stays inert here.
            true,
            pc_overlay,
            doctor_targets,
            Arc::clone(&self.pc_diagnostics),
        ))
    }
}

/// The one-line Scala 3 target summary the boot narrative prints once the
/// model is loaded: how many targets, how many are indexable, and WHICH ones
/// lack SemanticDB (the list a user needs to fix their scalacOptions).
fn log_model_summary(model: &BspProjectModel) {
    let indexable = model.indexable_targets().len();
    let unavailable: Vec<String> = model
        .unavailable_targets()
        .iter()
        .map(|t| t.bsp_id.clone())
        .collect();
    log::info!(
        target: "boot",
        "build model loaded: {} Scala 3 target(s), {} indexable, {} without SemanticDB{}",
        indexable + unavailable.len(),
        indexable,
        unavailable.len(),
        if unavailable.is_empty() {
            String::new()
        } else {
            format!(" (no -Xsemanticdb: {})", unavailable.join(", "))
        },
    );
}

impl<M: ModelSource> Bootstrap<CoreServices> for IndexBootstrap<M> {
    fn build(&self, workspace_root: Option<PathBuf>) -> WorkspaceState<CoreServices> {
        let started = std::time::Instant::now();
        let Some(root) = workspace_root else {
            log::error!(
                target: "boot",
                "bootstrap failed: no workspace root in the initialize params — \
                 run scala3SemanticLs.doctor"
            );
            return WorkspaceState::Failed {
                detail: "no workspace root in the initialize params".to_string(),
            };
        };
        log::info!(target: "boot", "bootstrap started for workspace {}", root.display());
        match self.build_services(&root) {
            Ok(services) => {
                log::info!(
                    target: "boot",
                    "READY in {:.1}s total",
                    started.elapsed().as_secs_f64()
                );
                WorkspaceState::Ready(services)
            }
            Err(detail) => {
                log::error!(
                    target: "boot",
                    "bootstrap failed after {:.1}s: {detail} — run scala3SemanticLs.doctor",
                    started.elapsed().as_secs_f64()
                );
                WorkspaceState::Failed { detail }
            }
        }
    }

    fn replay(&self, services: &CoreServices, documents: &Arc<DocumentStore>) {
        // Install the dirty-buffer overlay's environment (binding this same
        // shared document store) before replaying, so a PC query over a buffer
        // opened pre-ready sees the installed overlay.
        services.install_pc_overlay(Arc::clone(documents));
        replay_open_buffers(services, documents);
    }

    fn reload(
        &self,
        old: CoreServices,
        documents: &Arc<DocumentStore>,
    ) -> WorkspaceState<CoreServices> {
        reload_build_model(old, documents)
    }
}

/// Reload the build project model after a `buildTarget/didChange`, reusing the
/// ready bundle's durable handles. Refetches the model over the retained session
/// (`BuildCompiler::refetch_model` — no rediscovery, no relaunch), re-ingests into
/// the reused orchestrator (same store), rebuilds the URI ownership and sourceroot
/// mapping from the new model, re-registers the new PC target set into the reused
/// island, and replays the open buffers. A refetch or reingest failure keeps
/// serving the previous ready snapshot (never a spurious `Failed`). A
/// behavior-preserving port of `ScalaLs.reloadBuildModel`.
pub fn reload_build_model(
    old: CoreServices,
    documents: &Arc<DocumentStore>,
) -> WorkspaceState<CoreServices> {
    let old_target_count = old
        .orchestrator
        .workspace()
        .map(|ws| ws.targets.len())
        .unwrap_or(0);
    log::info!(
        target: "bsp",
        "build model reload started (buildTarget/didChange): refetching over the retained session"
    );
    let model = match old.compiler.refetch_model() {
        Ok(model) => model,
        Err(detail) => {
            log::warn!(
                target: "bsp",
                "build target model reload failed: {detail} — keeping the previous ready snapshot"
            );
            return WorkspaceState::Ready(old);
        }
    };
    let workspace = from_bsp(&model, workspace_source_facts());
    // The sourceroots, PC target registrations, and URI ownership are read from
    // the new model before the workspace moves into the reingest (mirrors `build`).
    let sourceroots: Vec<PathBuf> = workspace
        .targets
        .iter()
        .map(|t| t.sourceroot.clone())
        .collect();
    let pc_targets = pc_target_configs(&model);
    // The refetch reuses the same session WITHOUT re-initializing, so the build
    // server identity is unchanged — carry it from the old bundle onto the
    // rebuilt inventory (the refetched model has no initialize result of its own).
    let mut doctor_targets = doctor_targets_of(&model);
    let (old_name, old_version) = old.bsp_server();
    doctor_targets.server_name = old_name;
    doctor_targets.server_version = old_version;
    let uri_to_target = model
        .uri_to_target
        .iter()
        .map(|(uri, bsp_id)| (normalize_uri(uri), bsp_id.clone()))
        .collect();
    // Reingest only when the refetched model still has indexable targets — the
    // Scala `reloadBuildModel` gates its build job on `workspaceTargets.targets
    // .nonEmpty` (ScalaLs.scala). An all-targets-removed change must NOT commit an
    // empty segment that supersedes the prior index; the old segment is kept and
    // still answers the un-gated `workspace/symbol` (only the model-authoritative
    // `uri_to_target` ownership drops to the new empty set).
    let new_target_count = workspace.targets.len();
    if !workspace.targets.is_empty() {
        if let Err(error) = old.orchestrator.ingest(Arc::new(workspace)) {
            log::warn!(
                target: "bsp",
                "build target model reingest failed: {error} — keeping the previous ready snapshot"
            );
            return WorkspaceState::Ready(old);
        }
    }
    // Reuse the same island (buffers + JVM intact), updating its registered target
    // set to the refetched model — the Scala `reloadBuildModel` reuse of `s.pc`.
    old.pc.reconfigure_targets(pc_targets);
    let uris = WorkspaceUris::new(&sourceroots);
    let updated = CoreServices::new(
        old.orchestrator, // reused: same store, just reingested
        uris,
        old.workspace_root,
        uri_to_target,
        old.compiler, // reused: same retained session
        old.pc,       // reused: same island
        true,
        old.pc_overlay, // reused: same overlay inside the reused orchestrator
        doctor_targets, // refreshed from the reloaded model
        // Reused: the same merge layer the BSP session's on_diagnostics route
        // feeds, so live-typing publishes keep merging with compile truth.
        old.pc_diagnostics,
    );
    // Re-install the overlay environment with the refreshed URI mapping (the
    // sourceroots may have changed) before replaying the open buffers.
    updated.install_pc_overlay(Arc::clone(documents));
    replay_open_buffers(&updated, documents);
    log::info!(
        target: "bsp",
        "build model reload complete: {old_target_count} -> {new_target_count} indexable target(s)"
    );
    WorkspaceState::Ready(updated)
}

/// Seeds the presentation compiler's open-buffer mirror from the buffers already
/// open when the workspace reaches ready (opened during the pre-ready window, so
/// their `didOpen` notifications were dropped before the ready services existed).
/// A buffer opened before bootstrap finished is thereby visible to a later PC
/// query. Ports `ScalaLs.replayOpenBuffers`.
fn replay_open_buffers(services: &CoreServices, documents: &DocumentStore) {
    for uri in documents.open_uris() {
        if let (Some(text), Some(target_id)) =
            (documents.text(&uri), services.uri_to_target.get(&uri))
        {
            services.pc.did_open(target_id, &uri, &text);
        }
    }
}

/// The production [`ModelSource`]: discover the workspace's build server, launch
/// and initialize a session, and load the project model. Ports the model-load
/// prefix of `WorkspaceState.loadModel` (`build/initialize` then
/// `ProjectModelLoader.load`). Every `BspError` becomes the failed-bootstrap
/// detail string.
///
/// On success the launched session is RETAINED inside a [`BspCompileService`]
/// carried in the ready bundle, so `compile` and rename reach the live build
/// server; it is torn down from the ready-state teardown ([`BspCompileService`]'s
/// `Drop`), not here. On a load failure the launched server is shut down before
/// returning, so it is never left running.
#[derive(Clone)]
pub struct LiveBspModelSource {
    /// Called (on the BSP reader thread) when the build server reports a
    /// `buildTarget/didChange`, so the server schedules a model reload. The
    /// production hook sets the message loop's reload flag.
    on_build_targets_changed: Arc<dyn Fn() + Send + Sync>,
    /// Called (on the BSP reader thread) for each `build/publishDiagnostics`, so
    /// the server routes it through the diagnostics router and queues the LSP
    /// publish for the loop to forward to the editor.
    on_diagnostics: Arc<dyn Fn(BspPublishDiagnosticsParams) + Send + Sync>,
}

impl LiveBspModelSource {
    pub fn new(
        on_build_targets_changed: Arc<dyn Fn() + Send + Sync>,
        on_diagnostics: Arc<dyn Fn(BspPublishDiagnosticsParams) + Send + Sync>,
    ) -> Self {
        LiveBspModelSource {
            on_build_targets_changed,
            on_diagnostics,
        }
    }
}

impl ModelSource for LiveBspModelSource {
    fn load(&self, workspace_root: &Path) -> Result<LoadOutcome, String> {
        // No `.bsp` connection file: report the no-BSP case distinctly (ports
        // `defaultConnect` returning `None`). The bootstrap then declines it to a
        // failed state — the recovered-index warm restart is deferred (see
        // `IndexBootstrap::build`), so no build connection means no ready server.
        let discovery = BspDiscovery::discover(workspace_root);
        for invalid in &discovery.invalid {
            log::warn!(target: "boot", ".bsp discovery: invalid connection file: {invalid}");
        }
        let Some(connection) = discovery.candidates.first() else {
            log::warn!(
                target: "boot",
                ".bsp discovery: no usable connection file under {}/.bsp \
                 ({} invalid) — install one (for mill: `mill mill.bsp.BSP/install`) and restart",
                workspace_root.display(),
                discovery.invalid.len(),
            );
            return Ok(LoadOutcome::NoBsp);
        };
        let names: Vec<&str> = discovery
            .candidates
            .iter()
            .map(|c| c.details.name.as_str())
            .collect();
        log::info!(
            target: "boot",
            ".bsp discovery: {} candidate(s) {:?}, {} invalid; picked '{}' argv={:?}",
            discovery.candidates.len(),
            names,
            discovery.invalid.len(),
            connection.details.name,
            connection.details.argv,
        );
        // The build server drives reloads: a `buildTarget/didChange` notification
        // (delivered on the session's reader thread) fires the reload hook. The
        // server's async `build/logMessage`/`build/showMessage` traffic — mill's
        // compile progress — is re-emitted on the log stream so a user watching
        // a long compile sees it move.
        let on_changed = self.on_build_targets_changed.clone();
        let on_diagnostics = self.on_diagnostics.clone();
        let handlers = BspClientHandlers::new()
            .on_did_change_build_target(move |_| {
                log::info!(
                    target: "bsp",
                    "buildTarget/didChange received — scheduling a build model reload"
                );
                on_changed()
            })
            .on_log_message(|params| {
                log_build_message("build/logMessage", params.message_type, &params.message)
            })
            .on_show_message(|params| {
                log_build_message("build/showMessage", params.message_type, &params.message)
            })
            .on_diagnostics(move |params| on_diagnostics(params));
        let session = BspSession::launch(
            workspace_root.to_path_buf(),
            &connection.details,
            handlers,
            BspSessionConfig::default(),
        )
        .map_err(|e| e.to_string())?;
        ready_model_from_session(session).map(LoadOutcome::Model)
    }
}

/// Re-emits one BSP `build/logMessage`/`build/showMessage` on the log stream.
/// BSP `MessageType` 1 (error) and 2 (warning) map to WARN — they are the
/// BUILD's problems, not this server's — and 3 (info) / 4 (log) to INFO.
pub fn log_build_message(kind: &str, message_type: i32, message: &str) {
    if message_type == 1 || message_type == 2 {
        log::warn!(target: "bsp", "{kind}: {message}");
    } else {
        log::info!(target: "bsp", "{kind}: {message}");
    }
}

/// Assemble the ready build model from a connected (not-yet-initialized) BSP
/// session: initialize, load the project model, and — on success — retain the
/// session inside the compiler so `compile`/`refetch_model` run over it; on
/// failure tear the session down so a build server is never left running past a
/// failed load. Shared by the live model source (over a launched subprocess) and
/// the fake-BSP end-to-end harness (over an in-process `connect`ed server), so
/// both exercise the same real session-backed compiler.
pub fn ready_model_from_session(session: BspSession) -> Result<ReadyModel, String> {
    match load_project_model(&session) {
        Ok((model, server_name, server_version)) => Ok(ReadyModel {
            model,
            compiler: Arc::new(BspCompileService::new(session)),
            server_name,
            server_version,
        }),
        Err(detail) => {
            session.shutdown();
            session.close();
            Err(detail)
        }
    }
}

/// Initializes the session and loads the project model, mapping any `BspError`
/// to a detail string. Returns the build server's `build/initialize` display
/// name + version alongside the model (the Scala doctor `server:` line), so the
/// ready bundle can surface them. Separated so the caller retains or tears down
/// the session by the outcome.
fn load_project_model(
    session: &BspSession,
) -> Result<(BspProjectModel, Option<String>, Option<String>), String> {
    let init = session.initialize().map_err(|e| e.to_string())?;
    let model = ProjectModelLoader::load(session).map_err(|e| e.to_string())?;
    Ok((model, Some(init.display_name), Some(init.version)))
}

/// The live build compile capability: it owns the retained BSP session and
/// compiles through `buildTarget/compile`. Dropped when the ready bundle is torn
/// down, at which point it shuts the session down (matching the Scala ready-state
/// shutdown, which closes the session rather than leaving it after model load).
struct BspCompileService {
    session: BspSession,
}

impl BspCompileService {
    fn new(session: BspSession) -> Self {
        BspCompileService { session }
    }
}

impl CompileService for BspCompileService {
    fn compile(&self, targets: &[String]) -> CompileOutcome {
        match self.session.compile(targets, None) {
            Ok(BspCompileOutcome::Ok { .. }) => CompileOutcome::Ok,
            Ok(BspCompileOutcome::Failed { status_code, .. }) => CompileOutcome::Failed {
                reason: bsp_status_name(status_code),
            },
            Err(error) => CompileOutcome::Failed {
                reason: error.to_string(),
            },
        }
    }
}

impl BuildCompiler for BspCompileService {
    /// Refetch the model over the already-launched, already-initialized session
    /// (`ProjectModelLoader::load` without a second `initialize` — the Scala
    /// `Bootstrap.loadModel(session, …, initialize = false)`); no rediscovery, no
    /// relaunch.
    fn refetch_model(&self) -> Result<BspProjectModel, String> {
        ProjectModelLoader::load(&self.session).map_err(|e| e.to_string())
    }
}

impl Drop for BspCompileService {
    fn drop(&mut self) {
        // Best-effort teardown of the retained build server on ready-state drop.
        self.session.shutdown();
        self.session.close();
    }
}

/// The BSP `StatusCode` name a failed compile reports, matching the Scala
/// `s"compile failed: $code"` rendering (`StatusCode` interpolates its name).
fn bsp_status_name(status_code: i32) -> String {
    match status_code {
        2 => "ERROR".to_string(),
        3 => "CANCELLED".to_string(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use ls_bsp::model::BspTarget;

    fn target(bsp_id: &str, semanticdb_root: Option<&str>, sourceroot: Option<&str>) -> BspTarget {
        BspTarget {
            bsp_id: bsp_id.to_string(),
            display_name: bsp_id.to_string(),
            scala_version: "3.3.1".to_string(),
            scalac_options: Vec::new(),
            class_directory: PathBuf::from("/out").join(bsp_id),
            classpath: Vec::new(),
            semanticdb_root: semanticdb_root.map(PathBuf::from),
            sourceroot: sourceroot.map(PathBuf::from),
            sources: Vec::new(),
            direct_deps: Vec::new(),
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

    // Ports the Scala `pcConfigs` classpath/sourceDirs construction: the PC
    // classpath is the dependency classpath PLUS the target's own class directory,
    // deduped preserving order; the scalac options are SemanticDB-stripped; the
    // source path is empty.
    #[test]
    fn pc_target_configs_append_the_class_directory_deduped_with_empty_source_dirs() {
        let mut t = target("a", Some("/out/a"), Some("/src"));
        // The class directory is also already listed in the dependency classpath,
        // so the `.distinct` dedup must collapse it to one entry.
        t.classpath = vec![PathBuf::from("/dep/lib.jar"), PathBuf::from("/out/a")];
        t.class_directory = PathBuf::from("/out/a");
        t.scalac_options = vec!["-Xsemanticdb".to_string(), "-deprecation".to_string()];
        let model = BspProjectModel::new(vec![t], HashMap::new());
        let configs = pc_target_configs(&model);
        assert_eq!(configs.len(), 1);
        assert_eq!(
            configs[0].classpath,
            vec!["/dep/lib.jar".to_string(), "/out/a".to_string()]
        );
        assert_eq!(configs[0].scalac_options, vec!["-deprecation".to_string()]);
        assert!(configs[0].source_dirs.is_empty());
    }

    #[test]
    fn bootstrap_without_a_workspace_root_fails() {
        let bootstrap = IndexBootstrap::new(|_root: &Path| {
            Ok(BspProjectModel::new(Vec::new(), HashMap::new()))
        });
        let state = bootstrap.build(None);
        assert!(matches!(state, WorkspaceState::Failed { .. }));
    }

    #[test]
    fn bootstrap_over_an_empty_model_is_ready_on_an_empty_index() {
        let dir = tempfile::tempdir().unwrap();
        let bootstrap = IndexBootstrap::new(|_root: &Path| {
            Ok(BspProjectModel::new(Vec::new(), HashMap::new()))
        });
        let state = bootstrap.build(Some(dir.path().to_path_buf()));
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

    // With no `.bsp` connection file the live source reports the no-BSP case
    // (`LoadOutcome::NoBsp`) rather than erroring; the bootstrap declines it to a
    // failed state separately.
    #[test]
    fn live_model_source_without_a_connection_file_selects_no_bsp() {
        let dir = tempfile::tempdir().unwrap();
        // No `.bsp` connection file: the live source reports the no-BSP case
        // (ports `defaultConnect` -> None), not an error.
        let outcome = LiveBspModelSource::new(Arc::new(|| {}), Arc::new(|_| {}))
            .load(dir.path())
            .expect("no .bsp connection reports the no-BSP case, not an error");
        assert!(matches!(outcome, LoadOutcome::NoBsp));
    }

    #[test]
    fn bootstrap_propagates_a_model_load_failure() {
        let dir = tempfile::tempdir().unwrap();
        let bootstrap =
            IndexBootstrap::new(|_root: &Path| Err("no build server found".to_string()));
        let state = bootstrap.build(Some(dir.path().to_path_buf()));
        match state {
            WorkspaceState::Failed { detail } => assert!(detail.contains("no build server")),
            other => panic!("expected Failed, got {:?}", other.status_line()),
        }
    }
}
