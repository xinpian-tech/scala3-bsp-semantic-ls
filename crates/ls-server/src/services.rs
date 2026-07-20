//! The production ready-services bundle and the ready-path request handlers
//! wired over the Rust engine.
//!
//! [`CoreServices`] is the `S` the ready state owns (the Scala `CoreServices`
//! equivalent): the query orchestrator, the workspace URI mapping, the workspace
//! root, and the build model's URI ownership (`uri_to_target`, backing the
//! `requireSemanticdb` gate). [`CoreHandlers`] is the production [`Handlers`]
//! impl. It wires the ready methods that the engine + retained build compiler
//! answer without the PC island — `references`, `documentHighlight`,
//! `workspace/symbol`, `prepareRename`, `rename` (the FreshRequired compile
//! ladder), and the `scala3SemanticLs.reindex`, `scala3SemanticLs.compile`,
//! and `scala3SemanticLs.pcPluginStatus` executeCommand actions — over the
//! engine's [`ReferencesEngine`] /
//! [`DocumentHighlightService`] / workspace-symbol resolver / [`RenameEngine`]
//! and the retained build compiler, each gated (where the source applies it) by
//! `requireSemanticdb`, converting SemanticDB coordinates and URIs to the LSP
//! result shapes. It also wires the PC-island methods over `CoreServices.pc`:
//! `definition`/`typeDefinition` (the location family) and the
//! `completion`/`hover`/`signatureHelp` queries (decoding the island's flat
//! `#[repr(C)]` result carriers to the LSP result shapes). `completionItem/
//! resolve` enriches the item through the presentation compiler when it carries a
//! SemanticDB symbol and the last completion's target is still registered,
//! echoing the item back unchanged otherwise (the Scala `case _ => item`
//! fallback). The payload-backed `inlayHint`/`selectionRange`/`foldingRange`
//! methods follow the same dispatch discipline (`require_semanticdb` where it
//! applies, the `withPcBuffer` gate, empty/null fallbacks) with their LSP
//! shapes bridged through `lsp_types` models (`crate::pc_lsp`), not hand-rolled
//! serde.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use ls_bsp::model::BspProjectModel;
use ls_engine::{
    CompileOutcome, CompileService, DocHighlight, DocumentHighlightService, IngestReport,
    QueryOrchestrator, ReferencesEngine, ReferencesResult, RenameEngine, WorkspaceSymbolEntry,
};
use ls_index_model::uri::{normalize_uri, uri_to_path};
use ls_index_model::{LsError, Span};

use crate::build_scheduler::BuildScheduler;
use crate::capabilities::{commands, watch_globs};
use crate::convert::{self, DocumentHighlight, Location, SymbolKind, WorkspaceSymbol};
use crate::doctor::{
    DoctorReport, DoctorTargets, NixSection, PcPluginsSection, PcSection, RuntimeSection,
    SectionState, StoreSection,
};
use crate::documents::DocumentStore;
use crate::jsonrpc::{error_codes, RequestId, Response, ResponseError};
use crate::pc::{PcLocation, PcPluginStatusReport, PcQueryService};
use crate::pc_overlay::{PcOnlySymbol, PcOverlayInner};
use crate::protocol::Position;
use crate::server::{Handlers, RequestContext, WatchedFileEvent};
use crate::workspace_uris::WorkspaceUris;

/// The ready-services bundle owned by [`WorkspaceState::Ready`](crate::WorkspaceState::Ready):
/// the query orchestrator, the workspace URI mapping, the workspace root, and the
/// live build model's URI ownership (`uri_to_target`).
pub struct CoreServices {
    /// The query engine. Shared (`Arc`) because the PC island's cross-file
    /// `symbol_definition` resolver answers from this same index.
    pub orchestrator: Arc<QueryOrchestrator>,
    pub uris: WorkspaceUris,
    pub workspace_root: Option<PathBuf>,
    /// Normalized `file://` URI -> owning bspId, from the build project model
    /// (`WorkspaceState`'s `uriToTarget`). Backs the `requireSemanticdb` gate and
    /// names the PC target a buffer's PC request runs against.
    pub uri_to_target: HashMap<String, String>,
    /// The live build compile capability (the Scala `CoreServices.compiler`):
    /// the `compile` executeCommand and the rename compile ladder run through it,
    /// and the build-target-change reload refetches the project model over its
    /// retained session. In production it retains the BSP session; index-only
    /// injections get the disconnected stub. `Arc` so the build-job scheduler's
    /// background thread holds a clone and can run the didSave compile-first job.
    pub compiler: Arc<dyn BuildCompiler>,
    /// The presentation-compiler query capability (the Scala `CoreServices.pc`):
    /// the definition-family PC methods run through it. In production it lazily
    /// boots the embedded JVM island on the first PC request. `Arc` so the
    /// production dirty-buffer [`PcOverlay`](crate::pc_overlay::PcOverlay) holds a
    /// clone for its symbol-at-cursor resolution over the same island.
    pub pc: Arc<dyn PcQueryService>,
    /// Whether a live BSP session backs this workspace (the Scala `s.session`
    /// being non-empty). `false` only in the no-BSP recovered-index warm-restart
    /// mode; it gates the `require_semanticdb` persisted-index fallback so that
    /// fallback applies exclusively when no live model is authoritative.
    pub bsp_connected: bool,
    /// The target that owned the most recent completion request's buffer (the
    /// Scala `lastCompletionTarget`). `completion` records it; `completionItem/
    /// resolve` reads it to name the PC target the enrichment runs against.
    last_completion_target: Mutex<Option<String>>,
    /// The debounced, single-flight background build-job scheduler (the Scala
    /// `scheduleBuildJob` index thread). `didSave` enqueues a compile-first job
    /// over the saved file's reverse-dependency closure; a RawSemanticDBPath
    /// reference that could not heal inline enqueues a reindex-only job. Dropping
    /// the services stops and joins its worker.
    scheduler: BuildScheduler,
    /// The `Arc` handle to the PC-backed dirty-buffer overlay that lives inside
    /// `orchestrator` (the boxed [`DirtyBufferOverlay`]). Held here to
    /// [`install`](PcOverlayInner::install) its late-bound environment once the
    /// ready bundle exists and to answer `workspace/symbol`'s PC-only unsaved
    /// top-level symbols. The Scala `PcOverlay` reference retained by
    /// `WorkspaceState`.
    pub(crate) pc_overlay: Arc<PcOverlayInner>,
    /// The doctor's full-target inventory (all Scala 3 targets, the unavailable
    /// subset, and the indexable targetroots), captured from the build project
    /// model at bootstrap/reload. The doctor's `BSP`/`SemanticDB` sections read
    /// off this, NOT the indexable-only ingest `WorkspaceTargets`, so a target
    /// without SemanticDB output is counted and surfaced (the Scala doctor's
    /// `model.targets`/`model.unavailableTargets`).
    doctor_targets: DoctorTargets,
}

/// The retained build capability: a [`CompileService`] that can also refetch the
/// project model over its live session. The build-target-change reload uses
/// `refetch_model` to reload the model without relaunching or re-initializing the
/// server (the Scala `Bootstrap.loadModel(session, …, initialize = false)`), while
/// `compile`/rename keep using the [`CompileService`] surface. The disconnected
/// stub reports no session for both.
pub trait BuildCompiler: CompileService {
    /// Refetch the build project model over the retained session, or a
    /// human-readable detail on failure (a transient refetch error keeps the
    /// previous ready snapshot rather than dropping the workspace).
    fn refetch_model(&self) -> Result<BspProjectModel, String>;
}

impl CoreServices {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        orchestrator: Arc<QueryOrchestrator>,
        uris: WorkspaceUris,
        workspace_root: Option<PathBuf>,
        uri_to_target: HashMap<String, String>,
        compiler: Arc<dyn BuildCompiler>,
        pc: Arc<dyn PcQueryService>,
        bsp_connected: bool,
        pc_overlay: Arc<PcOverlayInner>,
        doctor_targets: DoctorTargets,
    ) -> CoreServices {
        let scheduler = BuildScheduler::new(Arc::clone(&orchestrator), Arc::clone(&compiler));
        CoreServices {
            orchestrator,
            uris,
            workspace_root,
            uri_to_target,
            compiler,
            pc,
            bsp_connected,
            last_completion_target: Mutex::new(None),
            scheduler,
            pc_overlay,
            doctor_targets,
        }
    }

    /// Install the PC-backed dirty-buffer overlay's late-bound environment (the
    /// Scala `PcOverlay.install(pc, toFileUri, isIndexedName)`), binding the
    /// shared document store, the PC query seam, and two closures reaching back
    /// into this ready bundle for URI mapping and index name-membership. Called
    /// at Ready adoption and after a reload (once `docs` is available). The
    /// closures hold a `Weak<QueryOrchestrator>` so the overlay — which lives
    /// inside that orchestrator — introduces no reference cycle.
    pub fn install_pc_overlay(&self, docs: Arc<DocumentStore>) {
        let uri_orch = Arc::downgrade(&self.orchestrator);
        let uris = self.uris.clone();
        let to_file_uri = Box::new(move |sdb: &str| {
            uri_orch
                .upgrade()
                .and_then(|orch| uris.to_file_uri(sdb, &orch))
        });
        let name_orch = Arc::downgrade(&self.orchestrator);
        // Fail-safe: a dropped orchestrator (never during a live query) reads as
        // indexed, so a membership check never spuriously refuses references/rename.
        let is_indexed_name = Box::new(move |name: &str| {
            name_orch
                .upgrade()
                .is_none_or(|orch| orch.workspace_symbol_name_exists(name))
        });
        self.pc_overlay
            .install(docs, Arc::clone(&self.pc), to_file_uri, is_indexed_name);
    }

    /// PC-only unsaved top-level symbols matching `query` (the overlay's
    /// `pcOnlySymbols`), for the `workspace/symbol` augmentation.
    pub fn pc_only_symbols(&self, query: &str) -> Vec<PcOnlySymbol> {
        self.pc_overlay.pc_only_symbols(query)
    }

    /// The retained build server `(display name, version)` from `build/initialize`
    /// (the doctor `server:` line). Read by `reload_build_model` to carry the
    /// identity across a model refetch, which does not re-initialize the session.
    pub(crate) fn bsp_server(&self) -> (Option<String>, Option<String>) {
        (
            self.doctor_targets.server_name.clone(),
            self.doctor_targets.server_version.clone(),
        )
    }

    /// The live doctor report for a ready workspace (the Scala
    /// `DoctorCommand.input`): `Runtime`/`Nix`/`Store` from the host, workspace
    /// files, and read-only store, plus the live `BSP`/`SemanticDB`/`PC`/`PC
    /// Plugins` sections gathered from the ingested workspace targets, the PC
    /// config, and the booted island's plugin report. Fully non-invasive — the
    /// `PC` worker status reads `/proc/self/maps` and the plugin report is
    /// fetched only from an ALREADY-BOOTED island (`plugin_status()` answers
    /// `None` for a cold one; the pre-boot invariant lives in
    /// `IslandPcService::plugin_status`), so the report never boots the
    /// embedded JVM.
    pub fn doctor_report(&self) -> DoctorReport {
        let root = self.workspace_root.as_deref();
        // A ready workspace loaded a build model, so BSP + SemanticDB are always
        // available here (an empty model is a valid zero-target ready state); the
        // `unavailable` rendering is the offline path (`DoctorReport::offline`).
        let bsp = SectionState::Available(bsp_section(&self.doctor_targets));
        let semanticdb = SectionState::Available(semanticdb_section(&self.doctor_targets));
        let booted = ls_jvm::libjvm_mapped();
        let registered = self.pc.registered_targets();
        let pc = SectionState::Available(PcSection {
            worker_status: if booted {
                "booted".to_string()
            } else {
                "not booted (cold)".to_string()
            },
            // Non-invasive: a booted island holds its registered targets active;
            // a cold island has none (enumerating active would require a query).
            active_targets: if booted {
                registered.clone()
            } else {
                Vec::new()
            },
            registered_targets: registered,
        });
        // The plugin report exists only over a booted island's control lane:
        // `plugin_status()` never boots (mirroring how the `PC` section reads
        // cold from `ls_jvm::libjvm_mapped()`), so a `None` renders the typed
        // cold reason and the report stays boot-free.
        let pc_plugins = match self.pc.plugin_status() {
            Some(report) => SectionState::Available(PcPluginsSection::of(report)),
            None => SectionState::Unavailable(PcPluginsSection::COLD.to_string()),
        };
        DoctorReport {
            runtime: RuntimeSection::gather(root.unwrap_or_else(|| Path::new("."))),
            nix: NixSection::gather(root.unwrap_or_else(|| Path::new("."))),
            bsp,
            semanticdb,
            store: StoreSection::gather(root),
            pc,
            pc_plugins,
        }
    }

    /// Enqueue a background reingest on the build-job scheduler (Scala
    /// `scheduleBuildJob(Vector.empty, compileFirst = false)`). Called when a
    /// RawSemanticDBPath query reports `needs_reindex`; never blocks the caller.
    pub fn schedule_reindex(&self) {
        self.scheduler.schedule_reindex();
    }

    /// The `didSave` build job (the tail of Scala `ScalaLs.didSave`): schedule a
    /// compile-first job over the reverse-dependency closure of the saved file's
    /// target, then reingest. The closure is the exact upper bound of targets that
    /// can reference a symbol defined in the saved file. With no owning target (or
    /// an id unknown to the indexed workspace) it degrades to the owning target
    /// alone, or to a reindex-only job when the file has no target at all. `uri`
    /// must already be normalized (it is matched against `uri_to_target`).
    pub fn schedule_save_build(&self, uri: &str) {
        let targets = match self.uri_to_target.get(uri) {
            Some(bsp_id) => {
                let closure = self
                    .orchestrator
                    .workspace()
                    .map(|ws| ws.reverse_dependency_closure(bsp_id))
                    .unwrap_or_default();
                if closure.is_empty() {
                    vec![bsp_id.clone()]
                } else {
                    let mut targets: Vec<String> = closure.into_iter().collect();
                    targets.sort();
                    targets
                }
            }
            None => Vec::new(),
        };
        let compile_first = !targets.is_empty();
        self.scheduler.schedule(targets, compile_first);
    }

    /// Test-only: replace the scheduler with a short-debounce one so background
    /// healing is observable without waiting out the production debounce.
    #[cfg(test)]
    fn with_scheduler_debounce(mut self, debounce: std::time::Duration) -> Self {
        self.scheduler = BuildScheduler::with_debounce(
            Arc::clone(&self.orchestrator),
            Arc::clone(&self.compiler),
            debounce,
        );
        self
    }

    /// Test-only: block until the scheduler has drained at least `n` reingest runs.
    #[cfg(test)]
    fn wait_for_reindex(&self, n: u64, timeout: std::time::Duration) -> u64 {
        self.scheduler.wait_for_runs(n, timeout)
    }

    /// `file://` URI -> SemanticDB URI against this workspace's index.
    pub fn to_sdb_uri(&self, file_uri: &str) -> Option<String> {
        self.uris.to_sdb_uri(file_uri, &self.orchestrator)
    }

    /// SemanticDB URI -> `file://` URI against this workspace's index.
    pub fn to_file_uri(&self, sdb_uri: &str) -> Option<String> {
        self.uris.to_file_uri(sdb_uri, &self.orchestrator)
    }

    /// SemanticDB is mandatory. A URI is serviceable when EITHER the live model
    /// owns it via an *indexable* target (one that produces SemanticDB, hence
    /// present in the ingested workspace), OR — only with no live BSP session —
    /// the recovered persisted index still holds an active document for it (the
    /// no-BSP warm-restart fallback). Otherwise the gated methods answer
    /// `NoSemanticdb` rather than a stale/empty result. Ports the three-branch
    /// `ScalaLs.requireSemanticdb`.
    pub fn require_semanticdb(&self, raw_uri: &str) -> Result<(), LsError> {
        let uri = normalize_uri(raw_uri);
        let owned_by_live_target = self.uri_to_target.get(&uri);
        let indexable_in_model = owned_by_live_target.is_some_and(|bsp_id| {
            self.orchestrator
                .workspace()
                .is_some_and(|workspace| workspace.targets.iter().any(|t| &t.bsp_id == bsp_id))
        });
        // The persisted-index fallback serves an already-indexed source in the
        // no-BSP recovered-index warm-restart mode. It applies ONLY when there
        // is no live BSP session AND the live model does not own the URI: while
        // a session exists the live model is authoritative about coverage, so a
        // URI it no longer owns — e.g. a source/target dropped via
        // `buildTarget/didChange`, leaving only a stale persisted row — is a
        // hard `NoSemanticdb`, never answered from that stale-but-active row.
        // Lazily evaluated, matching Scala's `def indexedOnDisk`.
        let indexed_on_disk = || {
            !self.bsp_connected
                && owned_by_live_target.is_none()
                && self
                    .to_sdb_uri(&uri)
                    .is_some_and(|sdb| self.orchestrator.has_active_document(&sdb))
        };
        if indexable_in_model || indexed_on_disk() {
            Ok(())
        } else {
            Err(LsError::NoSemanticdb { uri })
        }
    }
}

/// The production request handlers over [`CoreServices`].
pub struct CoreHandlers;

impl Handlers<CoreServices> for CoreHandlers {
    fn handle(&self, cx: RequestContext<'_, CoreServices>) -> Response {
        let id = cx.request.id.clone();
        match cx.request.method.as_str() {
            "textDocument/references" => references(id, cx.services, &cx.request.params),
            "textDocument/documentHighlight" => {
                document_highlight(id, cx.services, &cx.request.params)
            }
            "workspace/symbol" => workspace_symbol(id, cx.services, &cx.request.params),
            "textDocument/prepareRename" => prepare_rename(id, cx.services, &cx.request.params),
            "textDocument/rename" => rename(id, cx.services, &cx.request.params),
            "textDocument/definition" => definition(id, cx.services, &cx.request.params),
            "textDocument/typeDefinition" => type_definition(id, cx.services, &cx.request.params),
            "textDocument/completion" => completion(id, cx.services, &cx.request.params),
            "textDocument/hover" => hover(id, cx.services, &cx.request.params),
            "textDocument/signatureHelp" => signature_help(id, cx.services, &cx.request.params),
            "textDocument/inlayHint" => inlay_hint(id, cx.services, &cx.request.params),
            "textDocument/selectionRange" => selection_range(id, cx.services, &cx.request.params),
            "textDocument/foldingRange" => folding_range(id, cx.services, &cx.request.params),
            "completionItem/resolve" => {
                resolve_completion_item(id, cx.services, &cx.request.params)
            }
            "workspace/executeCommand" => execute_command(id, cx.services, &cx.request.params),
            other => not_implemented(id, other),
        }
    }

    /// A buffer opened: mirror it into the presentation compiler if the live
    /// model owns it via a target. Ports `TextDocs.didOpen`'s PC forward
    /// (`s.uriToTarget.get(uri).foreach(bspId => s.pc.didOpen(bspId, uri, text))`).
    fn on_did_open(&self, services: &CoreServices, uri: &str, text: &str) {
        if let Some(target_id) = services.uri_to_target.get(uri) {
            services.pc.did_open(target_id, uri, text);
        }
    }

    /// An open buffer changed: update the mirror if the PC already holds the
    /// buffer, otherwise open it (a change that arrives before the PC has the
    /// buffer). Ports `TextDocs.didChange`'s `if bufferText(uri).isDefined then
    /// didChange else uriToTarget.get(uri).foreach(didOpen)`.
    fn on_did_change(&self, services: &CoreServices, uri: &str, text: &str) {
        if services.pc.is_open(uri) {
            services.pc.did_change(uri, text);
        } else if let Some(target_id) = services.uri_to_target.get(uri) {
            services.pc.did_open(target_id, uri, text);
        }
    }

    /// A buffer closed: drop it from the mirror. Ports `TextDocs.didClose`'s
    /// unconditional `s.pc.didClose(uri)`.
    fn on_did_close(&self, services: &CoreServices, uri: &str) {
        services.pc.did_close(uri);
    }

    fn on_did_save(&self, services: &CoreServices, uri: &str) {
        services.schedule_save_build(uri);
    }

    /// `workspace/didChangeConfiguration`: the notification's `settings` payload
    /// is deliberately ignored — the workspace `.scala3-bsp-semantic-ls/
    /// config.json` stays the single configuration source — the PC island is
    /// only nudged to re-read that file (a still-cold island un-latches a
    /// recorded boot failure so the next PC query re-attempts the boot).
    fn on_did_change_configuration(&self, services: &CoreServices) {
        services.pc.on_config_changed();
    }

    /// Client-watched file events, filtered against the SAME globs the server
    /// registered ([`watch_globs`], compiled once into a [`globset::GlobSet`]).
    /// The batch coalesces per class — one reaction per notification, and the
    /// scheduler's debounce coalesces bursts of notifications further:
    /// `.semanticdb` (any change type) schedules the debounced reindex-only
    /// background job; the workspace `config.json` nudges the PC island to
    /// re-read its config (the didChangeConfiguration path); a `.bsp/*.json`
    /// change only logs — reconnecting the live BSP session in place is out of
    /// scope by decision, restart the server to reconnect. Unmatched URIs (a
    /// client may batch unrelated events) do nothing.
    fn on_watched_files(&self, services: &CoreServices, changes: &[WatchedFileEvent]) {
        let mut reindex = false;
        let mut config_changed = false;
        let mut bsp_changed = false;
        for change in changes {
            let Ok(path) = uri_to_path(&change.uri) else {
                continue;
            };
            for glob in watch_glob_set().matches(&path) {
                match glob {
                    WATCH_GLOB_SEMANTICDB => reindex = true,
                    WATCH_GLOB_CONFIG => config_changed = true,
                    _ => bsp_changed = true,
                }
            }
        }
        if reindex {
            services.schedule_reindex();
        }
        if config_changed {
            services.pc.on_config_changed();
        }
        if bsp_changed {
            eprintln!(
                "scala3-bsp-semantic-ls: build connection files (.bsp/*.json) changed; \
                 restart the server to reconnect"
            );
        }
    }

    fn doctor(
        &self,
        services: &CoreServices,
        _workspace_root: Option<&Path>,
    ) -> Option<DoctorReport> {
        Some(services.doctor_report())
    }
}

/// The [`watch_glob_set`] indices, in [`watch_globs::all`] order.
const WATCH_GLOB_SEMANTICDB: usize = 0;
const WATCH_GLOB_CONFIG: usize = 1;

/// The registered watcher globs compiled into one [`globset::GlobSet`] (built
/// lazily, once per process — the upstream ripgrep-family matcher, not a
/// hand-rolled glob). `literal_separator` keeps `*` within one path segment
/// (LSP glob semantics: `*` never crosses `/`, `**` matches any depth), so
/// `**/.bsp/*.json` matches only direct children of a `.bsp` directory.
fn watch_glob_set() -> &'static globset::GlobSet {
    static SET: std::sync::OnceLock<globset::GlobSet> = std::sync::OnceLock::new();
    SET.get_or_init(|| {
        let mut builder = globset::GlobSetBuilder::new();
        for pattern in watch_globs::all() {
            builder.add(
                globset::GlobBuilder::new(pattern)
                    .literal_separator(true)
                    .build()
                    .expect("the registered watch glob parses"),
            );
        }
        builder.build().expect("the watch glob set compiles")
    })
}

/// `textDocument/references`: resolve the cursor symbol over the index and return
/// its occurrences as LSP locations. An unmappable URI is `NotIndexed`; an engine
/// error maps to `RequestFailed` — the message is byte-compatible with the Scala
/// `mapErrors(LsException)` path. Ports `ScalaLs.references`.
fn references(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    let Some(raw) = text_document_uri(params) else {
        return invalid_params(id, "references: missing textDocument.uri");
    };
    // `requireSemanticdb` runs first: a URI the live model does not own via an
    // indexable target is `NoSemanticdb`, not answered.
    if let Err(error) = services.require_semanticdb(&raw) {
        return request_failed(id, &error);
    }
    let uri = normalize_uri(&raw);
    // `sdbUriOf`: a URI that maps to no sourceroot-relative form is NotIndexed.
    let Some(sdb_uri) = services.to_sdb_uri(&uri) else {
        return request_failed(id, &LsError::NotIndexed { uri });
    };
    let Some(pos) = position(params) else {
        return invalid_params(id, "references: missing position");
    };
    let include_declaration = params
        .get("context")
        .and_then(|context| context.get("includeDeclaration"))
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let engine = ReferencesEngine::new(&services.orchestrator);
    match engine.references(&sdb_uri, pos.line, pos.character, include_declaration) {
        Ok(result) => references_ok(id, services, &result),
        Err(error) => request_failed(id, &error),
    }
}

/// Shapes a successful `references` result. When the resolution fell through to
/// the raw `.semanticdb` path (`needs_reindex`), enqueue a background reingest to
/// heal the index (Scala `if result.needsReindex then scheduleBuildJob(
/// Vector.empty, compileFirst = false)`) — then return the full, already-complete
/// location list UNCHANGED. The healing is asynchronous; the response never waits
/// on it and never varies with `needs_reindex`.
fn references_ok(id: RequestId, services: &CoreServices, result: &ReferencesResult) -> Response {
    if result.needs_reindex {
        services.schedule_reindex();
    }
    ok_json(
        id,
        &references_locations(result, |sdb| services.to_file_uri(sdb)),
    )
}

/// `textDocument/documentHighlight`: read/write occurrences of the cursor symbol
/// within the one document, from the index doc-postings. `requireSemanticdb`
/// runs first and *outside* the query's catch: a URI the model does not own via
/// an indexable target is a hard `NoSemanticdb`/`RequestFailed` error, not an
/// empty result. Only the inner cursor-follow failures (an unmappable URI, a
/// missing position, an engine error) collapse to an empty list, matching
/// `ScalaLs.documentHighlight`'s inner `catch _: LsException => List.of()`.
fn document_highlight(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    let Some(raw) = text_document_uri(params) else {
        return Response::success(id, json!([]));
    };
    if let Err(error) = services.require_semanticdb(&raw) {
        return request_failed(id, &error);
    }
    let uri = normalize_uri(&raw);
    let Some(sdb_uri) = services.to_sdb_uri(&uri) else {
        return Response::success(id, json!([]));
    };
    let Some(pos) = position(params) else {
        return Response::success(id, json!([]));
    };
    let service = DocumentHighlightService::new(&services.orchestrator);
    match service.highlights(&sdb_uri, pos.line, pos.character) {
        Ok(highlights) => ok_json(id, &highlights_to_lsp(&highlights)),
        Err(_) => Response::success(id, json!([])),
    }
}

/// The default workspace-symbol candidate cap (Scala `workspaceSymbol`'s
/// `limit = 200`).
const WORKSPACE_SYMBOL_LIMIT: usize = 200;

/// `workspace/symbol`: resolve the query over the index and return matching
/// symbols with their defining locations. Ports `ScalaLs.symbol` for the
/// index-backed hits; the PC-only unsaved-buffer augmentation
/// (`overlay.pcOnlySymbols`) attaches with the PcOverlay. Ready-only — the
/// pre-ready fallback (an empty list) is served before the workspace is ready.
fn workspace_symbol(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    let query = params.get("query").and_then(Value::as_str).unwrap_or("");
    let mut symbols: Vec<WorkspaceSymbol> = services
        .orchestrator
        .workspace_symbols(query, WORKSPACE_SYMBOL_LIMIT)
        .iter()
        .map(workspace_symbol_of)
        .collect();
    // Merge top-level symbols from open unsaved buffers the persisted index has
    // never seen, flagged PC-only, AFTER the index hits (Scala `ScalaLs.symbol`).
    symbols.extend(
        services
            .pc_only_symbols(query)
            .iter()
            .map(pc_only_workspace_symbol),
    );
    ok_json(id, &symbols)
}

/// The `containerName` marking a `workspace/symbol` entry that exists only in an
/// open, unsaved buffer (the Scala `ScalaLs.PcOnlyContainer`).
const PC_ONLY_CONTAINER: &str = "unsaved buffer (PC-only)";

/// Render a PC-only unsaved top-level symbol as an LSP `WorkspaceSymbol`,
/// mapping its declaration keyword to a `SymbolKind` (Scala
/// `pcOnlyWorkspaceSymbol`).
fn pc_only_workspace_symbol(symbol: &PcOnlySymbol) -> WorkspaceSymbol {
    let kind = match symbol.keyword.as_str() {
        "object" => SymbolKind::Object,
        "class" => SymbolKind::Class,
        "trait" => SymbolKind::Interface,
        "enum" => SymbolKind::Enum,
        "def" => SymbolKind::Method,
        "val" | "var" => SymbolKind::Variable,
        "type" => SymbolKind::TypeParameter,
        _ => SymbolKind::Object,
    };
    WorkspaceSymbol {
        name: symbol.name.clone(),
        kind,
        location: convert::location(&symbol.file_uri, symbol.span),
        container_name: Some(PC_ONLY_CONTAINER.to_string()),
    }
}

/// `textDocument/prepareRename`: validate that a rename can start under the
/// cursor and return the symbol occurrence's span as an LSP range.
/// `requireSemanticdb` runs first and *outside* the query's catch: a URI the
/// model does not own via an indexable target is a hard `NoSemanticdb`/
/// `RequestFailed` error. Only the inner failures (an unmappable URI, a
/// non-renameable/unresolved cursor) answer `null`, matching
/// `ScalaLs.prepareRename`'s inner `catch => null`; a missing position answers
/// `null` as well (`_ => null`).
fn prepare_rename(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    let Some(raw) = text_document_uri(params) else {
        return Response::success(id, Value::Null);
    };
    if let Err(error) = services.require_semanticdb(&raw) {
        return request_failed(id, &error);
    }
    let Some(pos) = position(params) else {
        return Response::success(id, Value::Null);
    };
    let Some(sdb_uri) = services.to_sdb_uri(&normalize_uri(&raw)) else {
        return Response::success(id, Value::Null);
    };
    // `prepare_rename` resolves the span only; it never compiles, so the compile
    // hook is a placeholder the FreshRequired `rename` ladder will supply for real.
    let engine = RenameEngine::new(&services.orchestrator, &UnavailableCompiler);
    match engine.prepare_rename(&sdb_uri, pos.line, pos.character) {
        Ok(span) => ok_json(id, &convert::range(span)),
        Err(_) => Response::success(id, Value::Null),
    }
}

/// `textDocument/rename`: rename the symbol under the cursor across the workspace
/// via the FreshRequired compile ladder — compile the reverse-dependency closure
/// through the retained build compiler, reingest, re-resolve against the fresh
/// snapshot, apply the safety gates — and return the edits as an LSP
/// `WorkspaceEdit`. Ports `ScalaLs.rename`: `requireSemanticdb` gates first, and
/// with NO inner catch (unlike prepareRename) EVERY failure — a dirty/PC-only
/// buffer, an invalid new name, an unrenameable cursor, a failed compile, an
/// unmappable result URI — is a hard `RequestFailed`, never an empty/null result.
fn rename(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    let Some(raw) = text_document_uri(params) else {
        return invalid_params(id, "rename: missing textDocument.uri");
    };
    if let Err(error) = services.require_semanticdb(&raw) {
        return request_failed(id, &error);
    }
    let uri = normalize_uri(&raw);
    let Some(sdb_uri) = services.to_sdb_uri(&uri) else {
        return request_failed(id, &LsError::NotIndexed { uri });
    };
    let Some(pos) = position(params) else {
        return invalid_params(id, "rename: missing position");
    };
    let new_name = params.get("newName").and_then(Value::as_str).unwrap_or("");
    let engine = RenameEngine::new(&services.orchestrator, services.compiler.as_ref());
    match engine.rename(&sdb_uri, pos.line, pos.character, new_name) {
        Ok(plan) => match convert::workspace_edit(&plan, |sdb| services.to_file_uri(sdb)) {
            Ok(edit) => ok_json(id, &edit),
            Err(error) => request_failed(id, &error),
        },
        Err(error) => request_failed(id, &error),
    }
}

/// `textDocument/definition`: go-to-definition of the cursor symbol through the
/// presentation compiler over the open buffer. Ports `ScalaLs.definition`.
fn definition(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    pc_locations(id, services, params, |pc, uri, l, c| {
        pc.definition(uri, l, c)
    })
}

/// `textDocument/typeDefinition`: the type-definition variant, otherwise
/// identical to [`definition`]. Ports `ScalaLs.typeDefinition`.
fn type_definition(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    pc_locations(id, services, params, |pc, uri, l, c| {
        pc.type_definition(uri, l, c)
    })
}

/// The shared body of `definition`/`typeDefinition`. `requireSemanticdb` runs
/// FIRST and *outside* the buffer fallback (ports the Scala guard preceding
/// `withPcBuffer`): a URI the model does not own via an indexable target is a
/// hard `NoSemanticdb`/`RequestFailed`, not an empty result. Only a missing
/// position, a not-open buffer (the `withPcBuffer` fallback — the presentation
/// compiler's mirror does not hold the buffer), or the PC yielding nothing
/// answers the empty location list. The buffer text reaches the presentation
/// compiler through the document notifications, so the query runs against the
/// already-mirrored buffer by URI.
fn pc_locations(
    id: RequestId,
    services: &CoreServices,
    params: &Value,
    run: impl Fn(&dyn PcQueryService, &str, u32, u32) -> Vec<PcLocation>,
) -> Response {
    let Some(raw) = text_document_uri(params) else {
        return Response::success(id, json!([]));
    };
    if let Err(error) = services.require_semanticdb(&raw) {
        return request_failed(id, &error);
    }
    let Some(pos) = position(params) else {
        return Response::success(id, json!([]));
    };
    let uri = normalize_uri(&raw);
    // `withPcBuffer`: the presentation compiler serves only an open (dirty)
    // buffer; a URI the PC mirror does not hold answers the empty list.
    if !services.pc.is_open(&uri) {
        return Response::success(id, json!([]));
    }
    let locations = run(services.pc.as_ref(), &uri, pos.line, pos.character);
    ok_json(id, &pc_locations_to_lsp(&locations))
}

/// `textDocument/completion`: presentation-compiler completion over the open
/// (dirty) buffer, as an LSP `CompletionList`. Ports `ScalaLs.completion` — the
/// `withPcBuffer` fallback is an empty, complete completion list. After the gates
/// pass it records the buffer's owning target as `lastCompletionTarget`, so a
/// following `completionItem/resolve` can name the PC target to enrich against.
fn completion(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    pc_value(
        id,
        services,
        params,
        crate::pc_convert::empty_completions(),
        |services, uri, line, character| {
            if let Some(target) = services.uri_to_target.get(uri) {
                *services
                    .last_completion_target
                    .lock()
                    .expect("last completion target mutex") = Some(target.clone());
            }
            services.pc.completion(uri, line, character)
        },
    )
}

/// `textDocument/hover`: presentation-compiler hover over the open buffer, as an
/// LSP `Hover` or `null`. Ports `ScalaLs.hover` — the `withPcBuffer` fallback and
/// a null hover are both JSON `null`.
fn hover(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    pc_value(
        id,
        services,
        params,
        Value::Null,
        |services, uri, line, character| services.pc.hover(uri, line, character),
    )
}

/// `textDocument/signatureHelp`: presentation-compiler signature help over the
/// open buffer, as an LSP `SignatureHelp` or `null`. Ports `ScalaLs.signatureHelp`
/// — the `withPcBuffer` fallback is JSON `null`.
fn signature_help(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    pc_value(
        id,
        services,
        params,
        Value::Null,
        |services, uri, line, character| services.pc.signature_help(uri, line, character),
    )
}

/// The shared body of the JSON-valued PC query methods (completion/hover/
/// signatureHelp). Mirrors [`pc_locations`]: `requireSemanticdb` runs FIRST and
/// *outside* the buffer fallback (a URI the model does not own via an indexable
/// target is a hard `NoSemanticdb`/`RequestFailed`, never the fallback), then the
/// `withPcBuffer` gate (`is_open`) answers `fallback` for a buffer the PC mirror
/// does not hold. The PC method itself degrades a boundary/decode failure to the
/// same fallback, so the compiler yielding nothing is never an error. `run`
/// receives the whole `services` (not just the PC) so `completion` can record its
/// `lastCompletionTarget` inside the gated body, matching the Scala `withPcBuffer`.
fn pc_value(
    id: RequestId,
    services: &CoreServices,
    params: &Value,
    fallback: Value,
    run: impl Fn(&CoreServices, &str, u32, u32) -> Value,
) -> Response {
    let Some(raw) = text_document_uri(params) else {
        return Response::success(id, fallback);
    };
    if let Err(error) = services.require_semanticdb(&raw) {
        return request_failed(id, &error);
    }
    let Some(pos) = position(params) else {
        return Response::success(id, fallback);
    };
    let uri = normalize_uri(&raw);
    if !services.pc.is_open(&uri) {
        return Response::success(id, fallback);
    }
    Response::success(id, run(services, &uri, pos.line, pos.character))
}

/// The server's default inlay-hint category bitset, passed to every
/// `textDocument/inlayHint` request (the LSP request carries no category
/// choice; the flag set is server policy).
///
/// ON: `inferredTypes` (the `val x/*: Int*/` annotations — the headline
/// feature), plus the call-site adornments that surface INVISIBLE code —
/// `implicitParameters`, `byNameParameters`, `implicitConversions` — and
/// `namedParameters` (Metals' `hints-in-arguments` style default set: high
/// signal, low churn).
///
/// OFF: `typeParameters` and `hintsXRayMode` (every polymorphic apply/chain
/// gains bracket runs — noisy on idiomatic Scala and off by default in Metals
/// too), `hintsInPatternMatch` (pattern binders churn while a match is being
/// typed), and `closingLabels` (a Metals-custom rendering extension placed
/// AFTER closing braces that standard LSP clients cannot render as inlay
/// hints).
const INLAY_HINT_FLAGS: u32 = ls_pc_abi::payloads::inlay_hint_flags::INFERRED_TYPES
    | ls_pc_abi::payloads::inlay_hint_flags::IMPLICIT_PARAMETERS
    | ls_pc_abi::payloads::inlay_hint_flags::BY_NAME_PARAMETERS
    | ls_pc_abi::payloads::inlay_hint_flags::IMPLICIT_CONVERSIONS
    | ls_pc_abi::payloads::inlay_hint_flags::NAMED_PARAMETERS;

/// `textDocument/inlayHint`: presentation-compiler inlay hints for the request
/// range of the open buffer, with the server's default category set
/// ([`INLAY_HINT_FLAGS`]). Follows the hover/completion dispatch discipline:
/// `requireSemanticdb` runs FIRST and outside the buffer fallback (an unowned
/// URI is a hard `NoSemanticdb` error), then a missing/unparseable range or a
/// buffer the PC mirror does not hold (`withPcBuffer`) answers the empty hint
/// list — as does the island yielding nothing. The result is mapped through
/// `lsp_types::InlayHint` (label parts with location/tooltip, kind, padding,
/// text edits, `data` passed through verbatim). `inlayHint/resolve` is not
/// advertised (`resolveProvider: false`): every hint ships complete.
fn inlay_hint(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    let Some(raw) = text_document_uri(params) else {
        return Response::success(id, json!([]));
    };
    if let Err(error) = services.require_semanticdb(&raw) {
        return request_failed(id, &error);
    }
    let Some(range) = params
        .get("range")
        .and_then(|range| serde_json::from_value::<lsp_types::Range>(range.clone()).ok())
    else {
        return Response::success(id, json!([]));
    };
    let uri = normalize_uri(&raw);
    if !services.pc.is_open(&uri) {
        return Response::success(id, json!([]));
    }
    let hints: Vec<lsp_types::InlayHint> = services
        .pc
        .inlay_hints(&uri, crate::pc_lsp::abi_rng(&range), INLAY_HINT_FLAGS)
        .iter()
        .map(crate::pc_lsp::inlay_hint)
        .collect();
    ok_json(id, &hints)
}

/// `textDocument/selectionRange`: the chain of enclosing selection ranges per
/// query position, as the linked `lsp_types::SelectionRange` structure.
///
/// Deliberately NO `require_semanticdb` gate: selection ranges are pure syntax
/// — the island parses the mirrored buffer text and never consults SemanticDB
/// — so a source outside the indexable model (e.g. a target compiled without
/// `-Xsemanticdb`) still gets structural selections. The `withPcBuffer`
/// `is_open` gate stays: the island can only parse a buffer its mirror holds.
/// The gate fallback is `null` (spec: `SelectionRange[] | null`) rather than
/// an empty array, because the spec ties `result[i]` to `positions[i]` — an
/// empty array against a non-empty position list would break that
/// correspondence.
fn selection_range(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    let Some(raw) = text_document_uri(params) else {
        return Response::success(id, Value::Null);
    };
    let Some(positions) = params
        .get("positions")
        .and_then(|p| serde_json::from_value::<Vec<lsp_types::Position>>(p.clone()).ok())
    else {
        return Response::success(id, Value::Null);
    };
    let uri = normalize_uri(&raw);
    if !services.pc.is_open(&uri) {
        return Response::success(id, Value::Null);
    }
    let query: Vec<ls_pc_abi::payloads::Pos> =
        positions.iter().map(crate::pc_lsp::abi_pos).collect();
    let chains = services.pc.selection_range(&uri, &query);
    // The island degraded to nothing (a boot failure / boundary error): null,
    // like the gate fallback — never a position-count-mismatched array.
    if chains.is_empty() && !positions.is_empty() {
        return Response::success(id, Value::Null);
    }
    let result: Vec<lsp_types::SelectionRange> = positions
        .iter()
        .enumerate()
        .map(|(i, position)| {
            let chain = chains.get(i).map(Vec::as_slice).unwrap_or(&[]);
            crate::pc_lsp::selection_chain(chain, position)
        })
        .collect();
    ok_json(id, &result)
}

/// `textDocument/foldingRange`: the open buffer's folding ranges as
/// `lsp_types::FoldingRange` (kind ordinals mapped to `comment`/`imports`/
/// `region`; the island's `0` "none" omits the kind field). Pure syntax like
/// [`selection_range`] — the island's provider is a parser-only walk over the
/// mirrored buffer text, so there is NO `require_semanticdb` gate; only the
/// `withPcBuffer` `is_open` gate applies, and its fallback is the empty list
/// (no positions to stay in correspondence with).
fn folding_range(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    let Some(raw) = text_document_uri(params) else {
        return Response::success(id, json!([]));
    };
    let uri = normalize_uri(&raw);
    if !services.pc.is_open(&uri) {
        return Response::success(id, json!([]));
    }
    let ranges: Vec<lsp_types::FoldingRange> = services
        .pc
        .folding_range(&uri)
        .iter()
        .map(crate::pc_lsp::folding_range)
        .collect();
    ok_json(id, &ranges)
}

/// `completionItem/resolve`: enrich the item through the presentation compiler.
/// Ports `ScalaLs.resolveCompletionItem`: when the item carries a SemanticDB
/// `data.symbol`, the last completion's target is known, and that target is still
/// a registered PC config, resolve against the PC; otherwise (and on any PC
/// failure) echo the item back unchanged (the Scala `resolved.getOrElse(item)`).
fn resolve_completion_item(id: RequestId, services: &CoreServices, item: &Value) -> Response {
    let enriched = resolve_enriched(services, item).unwrap_or_else(|| item.clone());
    Response::success(id, enriched)
}

/// The enriched item when the symbol / last-completion-target / registration gates
/// all hold, else `None` (the caller echoes the item). The PC resolve itself
/// degrades to the original item on a boundary failure.
fn resolve_enriched(services: &CoreServices, item: &Value) -> Option<Value> {
    let symbol = data_symbol(item)?;
    let target = services
        .last_completion_target
        .lock()
        .expect("last completion target mutex")
        .clone()?;
    if !services.pc.is_registered(&target) {
        return None;
    }
    Some(services.pc.resolve_completion_item(&target, &symbol, item))
}

/// The SemanticDB symbol carried in a completion item's `data.symbol`, but only
/// when `data` is a JSON object and `symbol` a JSON string. Ports
/// `ScalaLs.dataSymbol`.
fn data_symbol(item: &Value) -> Option<String> {
    item.get("data")?
        .as_object()?
        .get("symbol")?
        .as_str()
        .map(str::to_string)
}

/// PC definition locations (already `file://` URIs) -> LSP locations.
pub fn pc_locations_to_lsp(locations: &[PcLocation]) -> Vec<Location> {
    locations
        .iter()
        .map(|loc| {
            convert::location(
                &loc.uri,
                Span::new(
                    loc.start_line,
                    loc.start_character,
                    loc.end_line,
                    loc.end_character,
                ),
            )
        })
        .collect()
}

/// `workspace/executeCommand` for the ready-path commands the message loop
/// routes here. `reindex` re-ingests over the retained workspace; `compile`
/// compiles the indexable targets through the retained build compiler;
/// `pcPluginStatus` reports the island's plugin state (typed cold answer while
/// the island has not booted). (`doctor` and unknown / pre-ready commands are
/// handled before this point.)
fn execute_command(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    match params.get("command").and_then(Value::as_str) {
        Some(commands::REINDEX) => Response::success(id, Value::String(reindex(services))),
        Some(commands::COMPILE) => Response::success(id, Value::String(compile(services))),
        Some(commands::PC_PLUGIN_STATUS) => {
            Response::success(id, plugin_status_result(services, params))
        }
        Some(other) => not_implemented(id, other),
        None => not_implemented(id, "workspace/executeCommand"),
    }
}

/// `scala3SemanticLs.pcPluginStatus`: the island's plugin-status report as a
/// standalone command (the Scala `PcStatusRender.render(s.pc.pluginStatus)`).
/// A still-cold island answers the typed cold status WITHOUT booting — the
/// pre-boot invariant lives in `IslandPcService::plugin_status`, this arm only
/// words its `None`. A booted island renders the text summary by default, or
/// the structured `{compilerPlugins, servicePlugins, disabled}` object with the
/// doctor's `arguments: [{"json": true}]` convention.
fn plugin_status_result(services: &CoreServices, params: &Value) -> Value {
    let Some(report) = services.pc.plugin_status() else {
        return Value::String(format!(
            "pc plugin status unavailable: {}",
            PcPluginsSection::COLD
        ));
    };
    if json_requested(params) {
        PcPluginsSection::of(report).render_json()
    } else {
        Value::String(plugin_status_text(&report))
    }
}

/// Whether an executeCommand asked for JSON output — `arguments: [{"json":
/// true}]`, the doctor's argument convention (`server::doctor_json_requested`
/// over the same params shape).
fn json_requested(params: &Value) -> bool {
    params
        .get("arguments")
        .and_then(Value::as_array)
        .and_then(|args| args.first())
        .and_then(|arg| arg.get("json"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

/// Ports the Scala `PcStatusRender.render` byte-for-byte: the compiler plugins
/// (jars -> loaded/detail), the service plugins (id/source/status/self-test),
/// and the disabled plugins with reasons, as one newline-joined summary.
fn plugin_status_text(report: &PcPluginStatusReport) -> String {
    let mut lines: Vec<String> = Vec::new();
    if report.compiler_plugins.is_empty() {
        lines.push("compiler plugins: none".to_string());
    } else {
        lines.push(format!(
            "compiler plugins: {}",
            report.compiler_plugins.len()
        ));
        for c in &report.compiler_plugins {
            let jars = if c.jars.is_empty() {
                "(no jars)".to_string()
            } else {
                c.jars.join(", ")
            };
            let outcome = if c.loaded {
                "loaded"
            } else {
                c.detail.as_str()
            };
            lines.push(format!("  {jars}: {outcome}"));
        }
    }
    if report.service_plugins.is_empty() {
        lines.push("service plugins: none".to_string());
    } else {
        lines.push(format!("service plugins: {}", report.service_plugins.len()));
        for p in &report.service_plugins {
            let status = if p.enabled { "enabled" } else { "disabled" };
            let self_test = if p.self_test_ok {
                "self-test ok"
            } else {
                p.self_test_detail.as_str()
            };
            lines.push(format!("  {} ({}): {status}, {self_test}", p.id, p.source));
        }
    }
    if report.disabled.is_empty() {
        lines.push("disabled plugins: none".to_string());
    } else {
        lines.push(format!("disabled plugins: {}", report.disabled.len()));
        for d in &report.disabled {
            lines.push(format!("  {}: {}", d.id, d.reason));
        }
    }
    lines.join("\n")
}

/// `scala3SemanticLs.compile`: compile the ingested workspace's indexable targets
/// through the retained build compiler and report the outcome (no reingest — the
/// didSave build flow handles that). With no indexable target it is a no-op with
/// the Scala skip message. Ports the `Compile` executeCommand branch.
fn compile(services: &CoreServices) -> String {
    let indexable_ids: Vec<String> = match services.orchestrator.workspace() {
        Some(workspace) => workspace.targets.iter().map(|t| t.bsp_id.clone()).collect(),
        None => Vec::new(),
    };
    if indexable_ids.is_empty() {
        return "compile skipped: no indexable targets".to_string();
    }
    match services.compiler.compile(&indexable_ids) {
        CompileOutcome::Ok => format!("compile ok ({} targets)", indexable_ids.len()),
        CompileOutcome::Failed { reason } => format!("compile failed: {reason}"),
    }
}

/// `scala3SemanticLs.reindex`: re-run a full ingest over the workspace the
/// orchestrator already holds, returning the ingest summary. With no indexable
/// target it is a no-op with the Scala skip message. Ports the `Reindex`
/// executeCommand branch.
fn reindex(services: &CoreServices) -> String {
    let orchestrator = &services.orchestrator;
    match orchestrator.workspace() {
        Some(workspace) if !workspace.targets.is_empty() => match orchestrator.ingest(workspace) {
            Ok(report) => ingest_summary(&report),
            Err(error) => format!("reindex failed: {error}"),
        },
        _ => "reindex skipped: no target produces SemanticDB".to_string(),
    }
}

/// One-line ingest summary. Ports `Bootstrap.ingestSummary` byte-for-byte.
fn ingest_summary(report: &IngestReport) -> String {
    format!(
        "ingest: segment {}, {} docs ({} shared, {} stale, {} skipped), \
         {} symbols, {} ref groups, {} rename groups in {}ms",
        report.segment_id,
        report.docs_indexed,
        report.docs_shared,
        report.docs_stale,
        report.docs_skipped,
        report.symbol_count,
        report.ref_group_count,
        report.rename_group_count,
        report.duration_ms,
    )
}

/// The disconnected compile capability: the default for index-only bundles that
/// carry no live build server (test injections, and the `prepareRename` engine,
/// which never compiles). A compile request reports no build server is connected,
/// and a model refetch reports there is no session to refetch over.
pub(crate) struct UnavailableCompiler;

impl CompileService for UnavailableCompiler {
    fn compile(&self, _targets: &[String]) -> CompileOutcome {
        CompileOutcome::Failed {
            reason: "no build server connected".to_string(),
        }
    }
}

impl BuildCompiler for UnavailableCompiler {
    fn refetch_model(&self) -> Result<BspProjectModel, String> {
        Err("no build server connected".to_string())
    }
}

/// Reference hits -> LSP locations, dropping any hit whose SemanticDB URI does
/// not resolve to a `file://` URI (mirroring the Scala `foreach`/`flatMap` that
/// only emits resolvable locations).
pub fn references_locations(
    result: &ReferencesResult,
    to_file_uri: impl Fn(&str) -> Option<String>,
) -> Vec<Location> {
    result
        .hits
        .iter()
        .filter_map(|hit| {
            to_file_uri(&hit.loc.uri).map(|file_uri| convert::location(&file_uri, hit.loc.span))
        })
        .collect()
}

/// Engine highlights -> LSP document highlights.
pub fn highlights_to_lsp(highlights: &[DocHighlight]) -> Vec<DocumentHighlight> {
    highlights
        .iter()
        .map(|highlight| DocumentHighlight {
            range: convert::range(highlight.span),
            kind: convert::highlight_kind(highlight.kind),
        })
        .collect()
}

/// A resolved workspace-symbol entry -> LSP `WorkspaceSymbol`.
/// The live doctor `BSP` section from the full-target inventory. Server
/// name/version come from the retained `build/initialize` identity (the Scala
/// `server: <name>` line); they render `unknown` only for an index-only injection
/// that captured no initialize result. Counts and lists come from ALL Scala 3
/// targets — including those without SemanticDB output — so the
/// `-Xsemanticdb`-missing targets are surfaced, not hidden (the Scala
/// `BspSection.gather`: `model.targets` + `model.unavailableTargets`).
fn bsp_section(targets: &DoctorTargets) -> crate::doctor::BspSection {
    crate::doctor::BspSection {
        server_name: targets.server_name.clone(),
        server_version: targets.server_version.clone(),
        target_count: targets.all_ids.len(),
        scala3_targets: targets.all_ids.clone(),
        index_unavailable_targets: targets.unavailable_ids.clone(),
    }
}

/// The live doctor `SemanticDB` section: one root fact per indexable target, its
/// existence + `.semanticdb` file count read from the REAL SemanticDB directory
/// (`<targetroot>/META-INF/semanticdb`, the `SemanticdbLocator` resolution), not
/// the targetroot itself — so a target whose `META-INF/semanticdb` was cleaned
/// reads `missing` even though its class-output targetroot still exists. Doc
/// freshness is `None` (`unavailable: not computed yet`), matching the Scala
/// `stats = None` gather. Roots are pre-sorted by bspId in [`DoctorTargets`].
fn semanticdb_section(targets: &DoctorTargets) -> crate::doctor::SemanticdbSection {
    let roots = targets
        .indexable_roots
        .iter()
        .map(|(bsp_id, targetroot)| {
            let semanticdb_dir = targetroot.join("META-INF").join("semanticdb");
            let exists = semanticdb_dir.is_dir();
            let semanticdb_file_count = if exists {
                count_semanticdb_files(&semanticdb_dir)
            } else {
                0
            };
            crate::doctor::SemanticdbRoot {
                bsp_id: bsp_id.clone(),
                semanticdb_root: semanticdb_dir.display().to_string(),
                exists,
                semanticdb_file_count,
            }
        })
        .collect();
    crate::doctor::SemanticdbSection {
        roots,
        freshness: None,
        generated_source_count: 0,
        stale_targets: Vec::new(),
    }
}

/// Count `.semanticdb` files under `root` (recursive), read-only. Symlinked
/// entries are NOT followed — `entry.file_type()` reports the link itself, so a
/// symlink cycle under a targetroot cannot make the doctor loop forever (a status
/// check must always terminate).
fn count_semanticdb_files(root: &Path) -> usize {
    let mut count = 0;
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            // `file_type()` does not traverse symlinks, so a symlinked directory
            // is neither recursed into nor miscounted.
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                stack.push(entry.path());
            } else if file_type.is_file()
                && entry.path().extension().and_then(|e| e.to_str()) == Some("semanticdb")
            {
                count += 1;
            }
        }
    }
    count
}

pub fn workspace_symbol_of(entry: &WorkspaceSymbolEntry) -> WorkspaceSymbol {
    WorkspaceSymbol {
        name: entry.display.clone(),
        kind: convert::symbol_kind(entry.kind),
        location: convert::location(&entry.location.uri, entry.location.span),
        container_name: entry.container.clone(),
    }
}

fn text_document_uri(params: &Value) -> Option<String> {
    params
        .get("textDocument")?
        .get("uri")?
        .as_str()
        .map(str::to_string)
}

fn position(params: &Value) -> Option<Position> {
    let position = params.get("position")?;
    Some(Position {
        line: u32::try_from(position.get("line")?.as_u64()?).ok()?,
        character: u32::try_from(position.get("character")?.as_u64()?).ok()?,
    })
}

fn ok_json<T: serde::Serialize>(id: RequestId, value: &T) -> Response {
    Response::success(id, serde_json::to_value(value).unwrap_or(Value::Null))
}

fn invalid_params(id: RequestId, message: &str) -> Response {
    Response::failure(
        id,
        ResponseError::new(error_codes::INVALID_PARAMS, message.to_string()),
    )
}

fn request_failed(id: RequestId, error: &LsError) -> Response {
    Response::failure(
        id,
        ResponseError::new(error_codes::REQUEST_FAILED, error.to_string()),
    )
}

/// A ready method whose subsystem is not yet wired. Answering a typed error
/// keeps the seam honest (never a silent empty lie) until the handler lands.
fn not_implemented(id: RequestId, method: &str) -> Response {
    Response::failure(
        id,
        ResponseError::new(
            error_codes::REQUEST_FAILED,
            format!("{method} is not yet available in this build"),
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    use ls_engine::{
        DocHighlight, HighlightKind, QueryOrchestrator, ReferenceHit, WorkspaceSymbolEntry,
    };
    use ls_index_model::uri::path_to_uri;
    use ls_index_model::{Loc, Role, Span, SymKind};
    use ls_store::Store;

    use crate::documents::DocumentStore;
    use crate::jsonrpc::Request;

    /// A fake PC that returns canned locations, so the definition/typeDefinition
    /// handlers are exercised without an embedded JVM.
    #[derive(Default)]
    struct FakePc {
        definition: Vec<PcLocation>,
        type_definition: Vec<PcLocation>,
        completion: Value,
        hover: Value,
        signature_help: Value,
        registered: bool,
        resolved: Option<Value>,
        /// The canned plugin-status report; `None` reads as a cold island.
        plugin_status: Option<PcPluginStatusReport>,
        /// Counts `on_config_changed` calls (shared so the test observes them
        /// after the fake moves into the services bundle).
        config_changes: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl PcQueryService for FakePc {
        fn did_open(&self, _t: &str, _u: &str, _x: &str) {}
        fn did_change(&self, _u: &str, _x: &str) {}
        fn did_close(&self, _u: &str) {}
        fn is_open(&self, _u: &str) -> bool {
            true
        }
        fn definition(&self, _u: &str, _l: u32, _c: u32) -> Vec<PcLocation> {
            self.definition.clone()
        }
        fn type_definition(&self, _u: &str, _l: u32, _c: u32) -> Vec<PcLocation> {
            self.type_definition.clone()
        }
        fn completion(&self, _u: &str, _l: u32, _c: u32) -> Value {
            self.completion.clone()
        }
        fn hover(&self, _u: &str, _l: u32, _c: u32) -> Value {
            self.hover.clone()
        }
        fn signature_help(&self, _u: &str, _l: u32, _c: u32) -> Value {
            self.signature_help.clone()
        }
        fn is_registered(&self, _t: &str) -> bool {
            self.registered
        }
        fn resolve_completion_item(&self, _t: &str, _s: &str, item: &Value) -> Value {
            self.resolved.clone().unwrap_or_else(|| item.clone())
        }
        fn plugin_status(&self) -> Option<PcPluginStatusReport> {
            self.plugin_status.clone()
        }
        fn on_config_changed(&self) {
            self.config_changes
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
    }

    // `workspace/didChangeConfiguration` reaches the PC service through the
    // production handlers: the settings payload is dropped upstream and the hook
    // only forwards to `PcQueryService::on_config_changed`.
    #[test]
    fn did_change_configuration_forwards_to_the_pc_service() {
        let dir = tempfile::tempdir().unwrap();
        let config_changes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let services = services_with_pc(
            dir.path(),
            FakePc {
                config_changes: config_changes.clone(),
                ..FakePc::default()
            },
        );
        CoreHandlers.on_did_change_configuration(&services);
        assert_eq!(
            config_changes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the handlers must forward the config change to the PC service"
        );
    }

    // The compiled watch-glob set classifies each registered event class by
    // index and rejects near-misses: LSP glob semantics (`*` within one
    // segment, `**` any depth) via globset's literal_separator.
    #[test]
    fn the_watch_glob_set_classifies_each_event_class() {
        let matches = |path: &str| watch_glob_set().matches(std::path::Path::new(path));
        assert_eq!(
            matches("/ws/out/META-INF/semanticdb/a/Core.scala.semanticdb"),
            vec![WATCH_GLOB_SEMANTICDB]
        );
        assert_eq!(
            matches("/ws/.scala3-bsp-semantic-ls/config.json"),
            vec![WATCH_GLOB_CONFIG]
        );
        assert_eq!(matches("/ws/.bsp/mill-bsp.json"), vec![2]);
        assert!(matches("/ws/src/Main.scala").is_empty());
        // Not the workspace config: config.json outside .scala3-bsp-semantic-ls.
        assert!(matches("/ws/config.json").is_empty());
        // `*` stays within one segment: nested .bsp json is not a connection file.
        assert!(matches("/ws/.bsp/nested/x.json").is_empty());
    }

    // A watched `.semanticdb` event schedules the debounced reindex-only
    // background job (the BuildScheduler coalesces bursts) and leaves the PC
    // config untouched.
    #[test]
    fn a_watched_semanticdb_event_schedules_the_background_reindex() {
        let dir = tempfile::tempdir().unwrap();
        let config_changes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let services = services_with_pc(
            dir.path(),
            FakePc {
                config_changes: config_changes.clone(),
                ..FakePc::default()
            },
        )
        .with_scheduler_debounce(std::time::Duration::from_millis(2));

        let uri = path_to_uri(
            &dir.path()
                .join("out/META-INF/semanticdb/A.scala.semanticdb"),
        );
        CoreHandlers.on_watched_files(&services, &[WatchedFileEvent { uri }]);
        assert_eq!(
            services.wait_for_reindex(1, std::time::Duration::from_secs(5)),
            1,
            "the semanticdb event must drain one background reingest"
        );
        assert_eq!(
            config_changes.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "a semanticdb event must not touch the PC config"
        );
    }

    // A watched workspace-config event nudges the PC island to re-read
    // config.json (the didChangeConfiguration path) and schedules no reingest.
    #[test]
    fn a_watched_config_event_nudges_the_pc_and_schedules_no_reindex() {
        let dir = tempfile::tempdir().unwrap();
        let config_changes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let services = services_with_pc(
            dir.path(),
            FakePc {
                config_changes: config_changes.clone(),
                ..FakePc::default()
            },
        )
        .with_scheduler_debounce(std::time::Duration::from_millis(2));

        let uri = path_to_uri(&dir.path().join(".scala3-bsp-semantic-ls/config.json"));
        CoreHandlers.on_watched_files(&services, &[WatchedFileEvent { uri }]);
        assert_eq!(
            config_changes.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "the config event must reach PcQueryService::on_config_changed"
        );
        assert_eq!(
            services.wait_for_reindex(1, std::time::Duration::from_millis(60)),
            0,
            "a config event must not schedule a reingest"
        );
    }

    // `.bsp/*.json` events only log (restart to reconnect — the warm re-bootstrap
    // is out of scope), and unrelated URIs do nothing at all.
    #[test]
    fn watched_bsp_and_unrelated_events_neither_reindex_nor_touch_the_pc() {
        let dir = tempfile::tempdir().unwrap();
        let config_changes = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let services = services_with_pc(
            dir.path(),
            FakePc {
                config_changes: config_changes.clone(),
                ..FakePc::default()
            },
        )
        .with_scheduler_debounce(std::time::Duration::from_millis(2));

        CoreHandlers.on_watched_files(
            &services,
            &[
                WatchedFileEvent {
                    uri: path_to_uri(&dir.path().join(".bsp/mill-bsp.json")),
                },
                WatchedFileEvent {
                    uri: path_to_uri(&dir.path().join("src/Main.scala")),
                },
                // An unparseable URI is skipped, never a panic.
                WatchedFileEvent {
                    uri: "untitled:Untitled-1".to_string(),
                },
            ],
        );
        assert_eq!(
            services.wait_for_reindex(1, std::time::Duration::from_millis(60)),
            0,
            ".bsp/unrelated events must not schedule a reingest"
        );
        assert_eq!(
            config_changes.load(std::sync::atomic::Ordering::SeqCst),
            0,
            ".bsp/unrelated events must not touch the PC config"
        );
    }

    // Reference hits become locations; a hit whose URI cannot be resolved to a
    // file is dropped rather than emitted with a bad URI.
    #[test]
    fn references_locations_maps_resolvable_hits_and_drops_the_rest() {
        let result = ReferencesResult {
            hits: vec![
                ReferenceHit {
                    loc: Loc::new("a/A.scala", Span::new(0, 0, 0, 3)),
                    role: Role::Definition,
                    from_overlay: false,
                },
                ReferenceHit {
                    loc: Loc::new("gone/G.scala", Span::new(1, 1, 1, 4)),
                    role: Role::Reference,
                    from_overlay: false,
                },
            ],
            needs_reindex: false,
        };
        let locations = references_locations(&result, |sdb| {
            (sdb == "a/A.scala").then(|| "file:///ws/a/A.scala".to_string())
        });
        assert_eq!(
            locations,
            vec![convert::location(
                "file:///ws/a/A.scala",
                Span::new(0, 0, 0, 3)
            )]
        );
    }

    #[test]
    fn highlights_to_lsp_maps_span_and_read_write_kind() {
        let highlights = vec![
            DocHighlight {
                span: Span::new(0, 0, 0, 2),
                kind: HighlightKind::Read,
            },
            DocHighlight {
                span: Span::new(2, 4, 2, 9),
                kind: HighlightKind::Write,
            },
        ];
        assert_eq!(
            serde_json::to_value(highlights_to_lsp(&highlights)).unwrap(),
            json!([
                { "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 2 } }, "kind": 2 },
                { "range": { "start": { "line": 2, "character": 4 }, "end": { "line": 2, "character": 9 } }, "kind": 3 },
            ])
        );
    }

    /// A `CoreServices` over a fresh (unindexed) store: URI mapping works from the
    /// sourceroot, but the engine has no occurrences, so it exercises the real
    /// glue's error/empty paths end-to-end (params -> URI map -> engine -> shape).
    fn unindexed_services(root: &Path) -> CoreServices {
        services_with_pc(root, FakePc::default())
    }

    /// Like [`unindexed_services`] but with a caller-supplied fake PC, for the
    /// completion-resolve gate tests.
    fn services_with_pc(root: &Path, pc: FakePc) -> CoreServices {
        let store = Store::open(root).unwrap();
        let overlay = crate::pc_overlay::PcOverlay::new();
        let pc_overlay = overlay.handle();
        let orchestrator = Arc::new(QueryOrchestrator::new(store, Box::new(overlay), true));
        let uris = WorkspaceUris::new(&[root.to_path_buf()]);
        CoreServices::new(
            orchestrator,
            uris,
            Some(root.to_path_buf()),
            HashMap::new(),
            Arc::new(UnavailableCompiler),
            Arc::new(pc),
            true,
            pc_overlay,
            DoctorTargets::default(),
        )
    }

    fn position_params(root: &Path) -> Value {
        let file_uri = path_to_uri(&root.join("A.scala"));
        json!({ "textDocument": { "uri": file_uri }, "position": { "line": 0, "character": 7 } })
    }

    #[test]
    fn references_over_an_unindexed_workspace_is_a_request_failed_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("A.scala"), "object A").unwrap();
        let services = unindexed_services(dir.path());
        let response = references(
            RequestId::Number(1),
            &services,
            &position_params(dir.path()),
        );
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["error"]["code"], error_codes::REQUEST_FAILED);
        assert!(value.get("result").is_none());
    }

    // A raw-`.semanticdb`-path references result (`needs_reindex`) enqueues a
    // background reingest and returns the SAME location list it would without the
    // flag — the response never varies with, or blocks on, the healing.
    #[test]
    fn references_needs_reindex_heals_in_background_without_changing_the_response() {
        use ls_engine::{ReferenceHit, ReferencesResult};
        use ls_index_model::{Loc, Role, Span};

        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path())
            .with_scheduler_debounce(std::time::Duration::from_millis(2));

        let hit = ReferenceHit {
            loc: Loc::new("A.scala".to_string(), Span::new(0, 0, 0, 1)),
            role: Role::Reference,
            from_overlay: false,
        };
        let cold = ReferencesResult {
            hits: vec![hit.clone()],
            needs_reindex: false,
        };
        let hot = ReferencesResult {
            hits: vec![hit],
            needs_reindex: true,
        };

        let cold_resp = references_ok(RequestId::Number(1), &services, &cold);
        // needs_reindex=false schedules nothing: no run within a short window.
        assert_eq!(
            services.wait_for_reindex(1, std::time::Duration::from_millis(60)),
            0
        );

        let hot_resp = references_ok(RequestId::Number(2), &services, &hot);
        // Same hits -> byte-identical result body regardless of needs_reindex.
        let cold_body = serde_json::to_value(&cold_resp).unwrap()["result"].clone();
        let hot_body = serde_json::to_value(&hot_resp).unwrap()["result"].clone();
        assert_eq!(cold_body, hot_body);
        // The heal enqueued exactly one background reingest.
        assert_eq!(
            services.wait_for_reindex(1, std::time::Duration::from_secs(5)),
            1
        );
    }

    /// Records every compile call; `refetch_model` is unused by the didSave path.
    struct RecordingCompiler {
        calls: Arc<Mutex<Vec<Vec<String>>>>,
    }

    impl ls_engine::CompileService for RecordingCompiler {
        fn compile(&self, targets: &[String]) -> ls_engine::CompileOutcome {
            self.calls.lock().unwrap().push(targets.to_vec());
            ls_engine::CompileOutcome::Ok
        }
    }

    impl BuildCompiler for RecordingCompiler {
        fn refetch_model(&self) -> Result<ls_bsp::model::BspProjectModel, String> {
            Err("no reload in the didSave test".to_string())
        }
    }

    /// `CoreServices` over a workspace where `b` depends on `a`, a recording
    /// compiler, and a short-debounce scheduler. Saving a file owned by `a` must
    /// compile the reverse-dependency closure `{a, b}` (sorted) then reingest.
    fn services_with_recording_compiler(
        root: &Path,
        uri_to_target: HashMap<String, String>,
    ) -> (CoreServices, Arc<Mutex<Vec<Vec<String>>>>) {
        use ls_engine::{TargetSpec, WorkspaceTargets};
        let store = Store::open(root).unwrap();
        let overlay = crate::pc_overlay::PcOverlay::new();
        let pc_overlay = overlay.handle();
        let orchestrator = Arc::new(QueryOrchestrator::new(store, Box::new(overlay), true));
        // Empty SemanticDB roots (0 docs) — enough to record the workspace so
        // `reverse_dependency_closure` resolves; the compile targets are what matter.
        std::fs::create_dir_all(root.join("out-a")).unwrap();
        std::fs::create_dir_all(root.join("out-b")).unwrap();
        let ws = WorkspaceTargets::new(vec![
            TargetSpec::new("a", root.join("out-a"), root.join("src")),
            TargetSpec::new("b", root.join("out-b"), root.join("src"))
                .with_deps(vec!["a".to_string()]),
        ]);
        orchestrator.ingest(Arc::new(ws)).unwrap();
        let calls = Arc::new(Mutex::new(Vec::new()));
        let compiler: Arc<dyn BuildCompiler> = Arc::new(RecordingCompiler {
            calls: Arc::clone(&calls),
        });
        let services = CoreServices::new(
            orchestrator,
            WorkspaceUris::new(&[root.to_path_buf()]),
            Some(root.to_path_buf()),
            uri_to_target,
            compiler,
            Arc::new(FakePc::default()),
            true,
            pc_overlay,
            DoctorTargets::default(),
        )
        .with_scheduler_debounce(std::time::Duration::from_millis(2));
        (services, calls)
    }

    // didSave over a file owned by `a` schedules a compile-first job over the
    // reverse-dependency closure `{a, b}` (Scala `didSave` -> `scheduleBuildJob`).
    #[test]
    fn did_save_schedules_compile_first_over_the_reverse_dependency_closure() {
        let dir = tempfile::tempdir().unwrap();
        let uri = path_to_uri(&dir.path().join("src/A.scala"));
        let uri_to_target = HashMap::from([(uri.clone(), "a".to_string())]);
        let (services, calls) = services_with_recording_compiler(dir.path(), uri_to_target);

        services.schedule_save_build(&uri);
        assert_eq!(
            services.wait_for_reindex(1, std::time::Duration::from_secs(5)),
            1
        );
        assert_eq!(
            *calls.lock().unwrap(),
            vec![vec!["a".to_string(), "b".to_string()]],
            "compile-first must cover the reverse-dependency closure, sorted"
        );
    }

    // A save for a file with no owning target degrades to a reindex-only job: the
    // run drains (reingest) but no compile is attempted.
    #[test]
    fn did_save_with_no_owning_target_schedules_reindex_only() {
        let dir = tempfile::tempdir().unwrap();
        let (services, calls) = services_with_recording_compiler(dir.path(), HashMap::new());

        services.schedule_save_build(&path_to_uri(&dir.path().join("src/Unowned.scala")));
        assert_eq!(
            services.wait_for_reindex(1, std::time::Duration::from_secs(5)),
            1
        );
        assert!(
            calls.lock().unwrap().is_empty(),
            "a save with no target must not compile"
        );
    }

    // `requireSemanticdb` runs outside the query's catch, so a URI the model does
    // not own is a hard NoSemanticdb error, not an empty list.
    #[test]
    fn document_highlight_over_an_unowned_uri_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("A.scala"), "object A").unwrap();
        let services = unindexed_services(dir.path());
        let response = document_highlight(
            RequestId::Number(1),
            &services,
            &position_params(dir.path()),
        );
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["error"]["code"], error_codes::REQUEST_FAILED);
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("has no SemanticDB output"));
    }

    // A ready method with no handler branch answers a typed error through the
    // production handler dispatch — never a silent empty result.
    // (`textDocument/documentColor` is the still-unimplemented example;
    // foldingRange graduated to a real handler.)
    #[test]
    fn an_unwired_ready_method_answers_a_typed_placeholder_error() {
        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path());
        let documents = DocumentStore::new();
        let request = Request {
            id: RequestId::Number(1),
            method: "textDocument/documentColor".to_string(),
            params: json!({}),
        };
        let response = CoreHandlers.handle(RequestContext {
            request: &request,
            services: &services,
            workspace_root: None,
            documents: &documents,
            shutting_down: false,
        });
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["error"]["code"], error_codes::REQUEST_FAILED);
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("not yet available"));
    }

    // A ready `completionItem/resolve` with no recorded `lastCompletionTarget`
    // echoes the item back unchanged (the Scala `resolved.getOrElse(item)` /
    // `case _ => item` fallback), never a typed error — driven through the real
    // handler dispatch.
    #[test]
    fn completion_item_resolve_echoes_without_a_last_completion_target() {
        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path());
        let documents = DocumentStore::new();
        let item = json!({ "label": "foo", "kind": 2, "data": { "symbol": "x" } });
        let request = Request {
            id: RequestId::Number(1),
            method: "completionItem/resolve".to_string(),
            params: item.clone(),
        };
        let response = CoreHandlers.handle(RequestContext {
            request: &request,
            services: &services,
            workspace_root: None,
            documents: &documents,
            shutting_down: false,
        });
        assert_eq!(serde_json::to_value(&response).unwrap()["result"], item);
    }

    // When the item carries a `data.symbol`, a `lastCompletionTarget` is recorded,
    // and that target is a registered PC config, resolve runs against the PC and
    // returns the enriched item (ports `ScalaLs.resolveCompletionItem`'s success).
    #[test]
    fn completion_item_resolve_enriches_when_symbol_target_and_registration_hold() {
        let dir = tempfile::tempdir().unwrap();
        let enriched = json!({ "label": "foo", "detail": "def foo: Int" });
        let services = services_with_pc(
            dir.path(),
            FakePc {
                registered: true,
                resolved: Some(enriched.clone()),
                ..Default::default()
            },
        );
        *services.last_completion_target.lock().unwrap() = Some("t".to_string());
        let item = json!({ "label": "foo", "data": { "symbol": "scala/foo." } });
        let value = serde_json::to_value(resolve_completion_item(
            RequestId::Number(1),
            &services,
            &item,
        ))
        .unwrap();
        assert_eq!(value["result"], enriched);
    }

    // Each broken gate echoes the item unchanged rather than resolving: no
    // `data.symbol`, and a target that is not a registered PC config.
    #[test]
    fn completion_item_resolve_echoes_when_a_gate_is_unmet() {
        let dir = tempfile::tempdir().unwrap();
        let would_enrich = json!({ "label": "X" });

        // No `data.symbol` (even with a target + registration): echo the item.
        let services = services_with_pc(
            dir.path(),
            FakePc {
                registered: true,
                resolved: Some(would_enrich.clone()),
                ..Default::default()
            },
        );
        *services.last_completion_target.lock().unwrap() = Some("t".to_string());
        let no_symbol = json!({ "label": "foo" });
        assert_eq!(
            serde_json::to_value(resolve_completion_item(
                RequestId::Number(1),
                &services,
                &no_symbol
            ))
            .unwrap()["result"],
            no_symbol
        );

        // A target that is not a registered PC config: echo the item.
        let services = services_with_pc(
            dir.path(),
            FakePc {
                registered: false,
                resolved: Some(would_enrich),
                ..Default::default()
            },
        );
        *services.last_completion_target.lock().unwrap() = Some("t".to_string());
        let item = json!({ "label": "foo", "data": { "symbol": "s" } });
        assert_eq!(
            serde_json::to_value(resolve_completion_item(
                RequestId::Number(1),
                &services,
                &item
            ))
            .unwrap()["result"],
            item
        );
    }

    // `data_symbol` reads `data.symbol` only when `data` is an object and `symbol`
    // a JSON string (ports `ScalaLs.dataSymbol`).
    #[test]
    fn data_symbol_reads_a_string_symbol_only() {
        assert_eq!(
            data_symbol(&json!({ "data": { "symbol": "s" } })),
            Some("s".to_string())
        );
        assert_eq!(data_symbol(&json!({ "data": { "symbol": 3 } })), None);
        assert_eq!(data_symbol(&json!({ "data": "not-an-object" })), None);
        assert_eq!(data_symbol(&json!({ "label": "x" })), None);
    }

    // `requireSemanticdb` runs first and *outside* the buffer fallback for the PC
    // query methods too, so completion/hover/signatureHelp — and inlayHint,
    // which follows the same discipline — over a URI the model does not own are
    // hard NoSemanticdb errors, not the empty/null fallback.
    #[test]
    fn pc_query_methods_over_an_unowned_uri_are_hard_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("A.scala"), "object A").unwrap();
        let services = unindexed_services(dir.path());
        for handler in [completion, hover, signature_help, inlay_hint] {
            let value = serde_json::to_value(handler(
                RequestId::Number(1),
                &services,
                &position_params(dir.path()),
            ))
            .unwrap();
            assert_eq!(value["error"]["code"], error_codes::REQUEST_FAILED);
            assert!(value["error"]["message"]
                .as_str()
                .unwrap()
                .contains("has no SemanticDB output"));
        }
    }

    // selectionRange and foldingRange are pure syntax: deliberately NO
    // `require_semanticdb` gate, so the same unowned URI that hard-errors the
    // semantic methods above answers the graceful fallback here (the FakePc
    // holds every buffer open but its payload ops default to empty, so the
    // island-yields-nothing path is what shapes the answer: null for
    // selectionRange — never a position-count-mismatched array — and the empty
    // list for foldingRange).
    #[test]
    fn selection_and_folding_over_an_unowned_uri_are_not_semanticdb_gated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("A.scala"), "object A").unwrap();
        let services = unindexed_services(dir.path());
        let file_uri = path_to_uri(&dir.path().join("A.scala"));

        let selection = selection_range(
            RequestId::Number(1),
            &services,
            &json!({
                "textDocument": { "uri": file_uri },
                "positions": [{ "line": 0, "character": 7 }]
            }),
        );
        let value = serde_json::to_value(&selection).unwrap();
        assert!(value.get("error").is_none(), "{value}");
        assert_eq!(value["result"], Value::Null);

        let folding = folding_range(
            RequestId::Number(2),
            &services,
            &json!({ "textDocument": { "uri": file_uri } }),
        );
        let value = serde_json::to_value(&folding).unwrap();
        assert!(value.get("error").is_none(), "{value}");
        assert_eq!(value["result"], json!([]));
    }

    // The `withPcBuffer` gate for the payload methods: a buffer the PC mirror
    // does not hold answers each method's fallback — null selection, empty
    // folds — never an error and never a PC call. (inlayHint's copy of the
    // same gate needs a semanticdb-owned URI to reach it, so it is pinned over
    // the wire in `tests/pc_wire.rs` against the fixture corpus.)
    #[test]
    fn payload_methods_on_an_unheld_buffer_take_their_fallbacks() {
        /// A fake whose mirror holds nothing (`is_open` false); any payload-op
        /// call would still answer the default empties, but the gate must
        /// answer first.
        struct ClosedPc;
        impl PcQueryService for ClosedPc {
            fn did_open(&self, _t: &str, _u: &str, _x: &str) {}
            fn did_change(&self, _u: &str, _x: &str) {}
            fn did_close(&self, _u: &str) {}
            fn is_open(&self, _u: &str) -> bool {
                false
            }
            fn definition(&self, _u: &str, _l: u32, _c: u32) -> Vec<PcLocation> {
                Vec::new()
            }
            fn type_definition(&self, _u: &str, _l: u32, _c: u32) -> Vec<PcLocation> {
                Vec::new()
            }
            fn completion(&self, _u: &str, _l: u32, _c: u32) -> Value {
                Value::Null
            }
            fn hover(&self, _u: &str, _l: u32, _c: u32) -> Value {
                Value::Null
            }
            fn signature_help(&self, _u: &str, _l: u32, _c: u32) -> Value {
                Value::Null
            }
            fn is_registered(&self, _t: &str) -> bool {
                false
            }
            fn resolve_completion_item(&self, _t: &str, _s: &str, item: &Value) -> Value {
                item.clone()
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let overlay = crate::pc_overlay::PcOverlay::new();
        let pc_overlay = overlay.handle();
        let orchestrator = Arc::new(QueryOrchestrator::new(store, Box::new(overlay), true));
        let services = CoreServices::new(
            orchestrator,
            WorkspaceUris::new(&[dir.path().to_path_buf()]),
            Some(dir.path().to_path_buf()),
            HashMap::new(),
            Arc::new(UnavailableCompiler),
            Arc::new(ClosedPc),
            true,
            pc_overlay,
            DoctorTargets::default(),
        );
        let file_uri = path_to_uri(&dir.path().join("A.scala"));

        let selection = selection_range(
            RequestId::Number(1),
            &services,
            &json!({
                "textDocument": { "uri": file_uri },
                "positions": [{ "line": 0, "character": 0 }]
            }),
        );
        assert_eq!(
            serde_json::to_value(&selection).unwrap()["result"],
            Value::Null
        );

        let folding = folding_range(
            RequestId::Number(2),
            &services,
            &json!({ "textDocument": { "uri": file_uri } }),
        );
        assert_eq!(serde_json::to_value(&folding).unwrap()["result"], json!([]));
    }

    #[test]
    fn workspace_symbol_of_maps_entry_with_its_container() {
        let entry = WorkspaceSymbolEntry {
            display: "Foo".to_string(),
            kind: SymKind::Class,
            container: Some("pkg".to_string()),
            location: Loc::new("file:///ws/a/Foo.scala", Span::new(1, 2, 1, 5)),
        };
        assert_eq!(
            serde_json::to_value(workspace_symbol_of(&entry)).unwrap(),
            json!({
                "name": "Foo",
                "kind": 5,
                "location": {
                    "uri": "file:///ws/a/Foo.scala",
                    "range": { "start": { "line": 1, "character": 2 }, "end": { "line": 1, "character": 5 } }
                },
                "containerName": "pkg"
            })
        );
    }

    // An absent container is omitted, not serialized as null/empty.
    #[test]
    fn workspace_symbol_of_omits_an_absent_container() {
        let entry = WorkspaceSymbolEntry {
            display: "bar".to_string(),
            kind: SymKind::Method,
            container: None,
            location: Loc::new("file:///ws/a/A.scala", Span::new(0, 0, 0, 3)),
        };
        let value = serde_json::to_value(workspace_symbol_of(&entry)).unwrap();
        assert_eq!(value["name"], "bar");
        assert_eq!(value["kind"], 6);
        assert!(value.get("containerName").is_none());
    }

    #[test]
    fn workspace_symbol_over_an_unindexed_workspace_is_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path());
        let response =
            workspace_symbol(RequestId::Number(1), &services, &json!({ "query": "Foo" }));
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["result"], json!([]));
    }

    // `workspace/symbol` appends PC-only top-level declarations from open unsaved
    // buffers the persisted index has never seen, flagged with the PC-only
    // container (Scala `ScalaLs.symbol` merge of `overlay.pcOnlySymbols`).
    #[test]
    fn workspace_symbol_appends_pc_only_unsaved_top_level_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path());
        // Install the overlay over a dirty buffer holding an unindexed `object`.
        let docs = Arc::new(DocumentStore::new());
        docs.open("file:///ws/Fresh.scala", "object Widget:\n  def x = 1\n");
        services.install_pc_overlay(docs);

        let response =
            workspace_symbol(RequestId::Number(1), &services, &json!({ "query": "wid" }));
        let value = serde_json::to_value(&response).unwrap();
        let entries = value["result"].as_array().expect("result array");
        let widget = entries
            .iter()
            .find(|s| s["name"] == "Widget")
            .expect("the PC-only Widget symbol");
        assert_eq!(widget["kind"], 19, "object => SymbolKind.Object");
        assert_eq!(widget["containerName"], "unsaved buffer (PC-only)");
        assert_eq!(
            widget["location"]["uri"], "file:///ws/Fresh.scala",
            "located in the unsaved buffer"
        );
        // A query matching nothing in the buffer yields no PC-only entries.
        let empty = workspace_symbol(RequestId::Number(2), &services, &json!({ "query": "zzz" }));
        assert_eq!(serde_json::to_value(&empty).unwrap()["result"], json!([]));
    }

    // `requireSemanticdb` runs first and *outside* the buffer fallback, so
    // definition over a URI the model does not own is a hard NoSemanticdb error,
    // not an empty location list (the guard-escalation contract).
    #[test]
    fn definition_over_an_unowned_uri_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("A.scala"), "object A").unwrap();
        let services = unindexed_services(dir.path());
        let response = definition(
            RequestId::Number(1),
            &services,
            &position_params(dir.path()),
        );
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["error"]["code"], error_codes::REQUEST_FAILED);
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("has no SemanticDB output"));
    }

    // A PC location (already a `file://` URI) converts to an LSP location with the
    // direct range mapping.
    #[test]
    fn pc_locations_to_lsp_maps_uri_and_range() {
        let locations = [PcLocation {
            uri: "file:///ws/a/Core.scala".to_string(),
            start_line: 2,
            start_character: 6,
            end_line: 2,
            end_character: 10,
        }];
        assert_eq!(
            serde_json::to_value(pc_locations_to_lsp(&locations)).unwrap(),
            json!([{
                "uri": "file:///ws/a/Core.scala",
                "range": { "start": { "line": 2, "character": 6 }, "end": { "line": 2, "character": 10 } }
            }])
        );
    }

    // A non-empty-authority `file://host/...` URI must be unmappable: the shared
    // `uri_to_path` rejects the authority (Java `Path.of(URI)` parity), so it can
    // never be answered as the bare local `/...` path.
    #[test]
    fn references_over_a_non_empty_authority_uri_is_not_indexed() {
        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path());
        let params = json!({
            "textDocument": { "uri": "file://host/ws/A.scala" },
            "position": { "line": 0, "character": 0 }
        });
        let value =
            serde_json::to_value(references(RequestId::Number(1), &services, &params)).unwrap();
        assert_eq!(value["error"]["code"], error_codes::REQUEST_FAILED);
        assert!(value["error"]["message"].as_str().unwrap().contains("host"));
    }

    #[test]
    fn document_highlight_over_a_non_empty_authority_uri_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path());
        let params = json!({
            "textDocument": { "uri": "file://host/ws/A.scala" },
            "position": { "line": 0, "character": 0 }
        });
        let value =
            serde_json::to_value(document_highlight(RequestId::Number(1), &services, &params))
                .unwrap();
        assert_eq!(value["error"]["code"], error_codes::REQUEST_FAILED);
        assert!(value["error"]["message"].as_str().unwrap().contains("host"));
    }

    // prepareRename's `requireSemanticdb` runs outside its `catch => null`, so a
    // URI the model does not own is a hard NoSemanticdb error, not null.
    #[test]
    fn prepare_rename_over_an_unowned_uri_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("A.scala"), "object A").unwrap();
        let services = unindexed_services(dir.path());
        let value = serde_json::to_value(prepare_rename(
            RequestId::Number(1),
            &services,
            &position_params(dir.path()),
        ))
        .unwrap();
        assert_eq!(value["error"]["code"], error_codes::REQUEST_FAILED);
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("has no SemanticDB output"));
    }

    #[test]
    fn prepare_rename_over_an_unowned_untitled_uri_is_a_hard_error() {
        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path());
        let params = json!({
            "textDocument": { "uri": "untitled:Untitled-1" },
            "position": { "line": 0, "character": 0 }
        });
        let value =
            serde_json::to_value(prepare_rename(RequestId::Number(1), &services, &params)).unwrap();
        assert_eq!(value["error"]["code"], error_codes::REQUEST_FAILED);
        assert!(value["error"]["message"]
            .as_str()
            .unwrap()
            .contains("has no SemanticDB output"));
    }

    // reindex over an orchestrator that never ingested a workspace is the no-op
    // skip message, not an error.
    #[test]
    fn reindex_without_an_ingested_workspace_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path());
        let params = json!({ "command": "scala3SemanticLs.reindex" });
        let value = serde_json::to_value(execute_command(RequestId::Number(1), &services, &params))
            .unwrap();
        assert_eq!(
            value["result"],
            "reindex skipped: no target produces SemanticDB"
        );
    }

    // compile over an orchestrator that never ingested a workspace has no
    // indexable target, so it is the skip message, not a compile attempt.
    #[test]
    fn compile_without_an_ingested_workspace_is_skipped() {
        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path());
        let params = json!({ "command": "scala3SemanticLs.compile" });
        let value = serde_json::to_value(execute_command(RequestId::Number(1), &services, &params))
            .unwrap();
        assert_eq!(value["result"], "compile skipped: no indexable targets");
    }

    fn sample_plugin_report() -> PcPluginStatusReport {
        use crate::pc::{PcCompilerPluginStatus, PcDisabledPlugin, PcServicePluginStatus};
        PcPluginStatusReport {
            compiler_plugins: vec![PcCompilerPluginStatus {
                jars: vec!["/plugins/zaozi.jar".to_string()],
                options: vec!["-P:zaozi:on".to_string()],
                loaded: true,
                detail: "ok".to_string(),
            }],
            service_plugins: vec![PcServicePluginStatus {
                id: "zaozi.nav".to_string(),
                source: "workspace pc-plugins.json".to_string(),
                enabled: true,
                self_test_ok: true,
                self_test_detail: "ok".to_string(),
            }],
            disabled: vec![PcDisabledPlugin {
                id: "old.plugin".to_string(),
                reason: "disabled by config".to_string(),
            }],
        }
    }

    // pcPluginStatus over a ready-but-cold island answers the typed cold status
    // (a success string), never a boot and never an error.
    #[test]
    fn pc_plugin_status_over_a_cold_island_is_the_typed_cold_answer() {
        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path());
        let params = json!({ "command": "scala3SemanticLs.pcPluginStatus" });
        let value = serde_json::to_value(execute_command(RequestId::Number(1), &services, &params))
            .unwrap();
        assert_eq!(
            value["result"],
            format!("pc plugin status unavailable: {}", PcPluginsSection::COLD)
        );
    }

    // pcPluginStatus over a booted island renders the PcStatusRender text
    // summary by default and the structured object with the doctor's
    // `arguments: [{"json": true}]` convention.
    #[test]
    fn pc_plugin_status_renders_the_text_summary_and_the_json_object() {
        let dir = tempfile::tempdir().unwrap();
        let services = services_with_pc(
            dir.path(),
            FakePc {
                plugin_status: Some(sample_plugin_report()),
                ..FakePc::default()
            },
        );

        let params = json!({ "command": "scala3SemanticLs.pcPluginStatus" });
        let value = serde_json::to_value(execute_command(RequestId::Number(1), &services, &params))
            .unwrap();
        let text = value["result"].as_str().expect("text summary");
        assert!(text.contains("compiler plugins: 1"), "{text}");
        assert!(text.contains("  /plugins/zaozi.jar: loaded"), "{text}");
        assert!(
            text.contains("  zaozi.nav (workspace pc-plugins.json): enabled, self-test ok"),
            "{text}"
        );
        assert!(text.contains("disabled plugins: 1"), "{text}");
        assert!(text.contains("  old.plugin: disabled by config"), "{text}");

        let params = json!({
            "command": "scala3SemanticLs.pcPluginStatus",
            "arguments": [{ "json": true }]
        });
        let value = serde_json::to_value(execute_command(RequestId::Number(2), &services, &params))
            .unwrap();
        let report = &value["result"];
        assert_eq!(report["compilerPlugins"][0]["loaded"], true);
        assert_eq!(report["servicePlugins"][0]["id"], "zaozi.nav");
        assert_eq!(report["disabled"][0]["reason"], "disabled by config");
    }

    // The doctor report's `PC Plugins` section mirrors the plugin-status seam:
    // a report renders Available, a cold `None` renders the typed cold reason —
    // and gathering never boots (the fake would panic if it could).
    #[test]
    fn doctor_report_pc_plugins_follows_the_plugin_status_seam() {
        let dir = tempfile::tempdir().unwrap();

        let cold = unindexed_services(dir.path());
        match cold.doctor_report().pc_plugins {
            SectionState::Unavailable(reason) => assert_eq!(reason, PcPluginsSection::COLD),
            SectionState::Available(_) => panic!("a cold island must render unavailable"),
        }

        let booted = services_with_pc(
            dir.path(),
            FakePc {
                plugin_status: Some(sample_plugin_report()),
                ..FakePc::default()
            },
        );
        match booted.doctor_report().pc_plugins {
            SectionState::Available(section) => {
                assert_eq!(section.service_plugins[0].id, "zaozi.nav");
            }
            SectionState::Unavailable(reason) => panic!("expected the live report, got {reason}"),
        }
    }

    // A URI the live model does not own via an indexable target is NoSemanticdb,
    // carrying the normalized URI.
    #[test]
    fn require_semanticdb_rejects_a_uri_the_model_does_not_own() {
        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path());
        let error = services
            .require_semanticdb("file:///ws/A.scala")
            .expect_err("an unowned uri has no SemanticDB");
        assert!(matches!(
            error,
            LsError::NoSemanticdb { uri } if uri == "file:///ws/A.scala"
        ));
    }

    // The doctor BSP section counts and lists ALL Scala 3 targets and surfaces the
    // ones without SemanticDB output (the misconfiguration the doctor exists to
    // report) — it must NOT read off the indexable-only ingest view.
    #[test]
    fn bsp_section_counts_all_targets_and_surfaces_the_unavailable_ones() {
        let targets = DoctorTargets {
            server_name: Some("mill-bsp".to_string()),
            server_version: Some("1.1.2".to_string()),
            all_ids: vec!["a".to_string(), "b".to_string()],
            unavailable_ids: vec!["b".to_string()],
            indexable_roots: vec![("a".to_string(), PathBuf::from("/ws/out-a"))],
        };
        let section = bsp_section(&targets);
        // The retained build/initialize identity flows into the `server:` line.
        assert_eq!(section.server_name.as_deref(), Some("mill-bsp"));
        assert_eq!(section.server_version.as_deref(), Some("1.1.2"));
        assert_eq!(section.target_count, 2);
        assert_eq!(
            section.scala3_targets,
            vec!["a".to_string(), "b".to_string()]
        );
        assert_eq!(section.index_unavailable_targets, vec!["b".to_string()]);
    }

    // The doctor SemanticDB section resolves each root to the REAL
    // `<targetroot>/META-INF/semanticdb` directory (the locator semantics), so a
    // targetroot that exists but has no `META-INF/semanticdb` reads `missing`.
    #[test]
    fn semanticdb_section_resolves_the_meta_inf_semanticdb_dir() {
        let dir = tempfile::tempdir().unwrap();
        // Target `a`: a real META-INF/semanticdb with one `.semanticdb` file.
        let root_a = dir.path().join("out-a");
        let sdb_a = root_a.join("META-INF").join("semanticdb");
        std::fs::create_dir_all(&sdb_a).unwrap();
        std::fs::write(sdb_a.join("A.scala.semanticdb"), b"x").unwrap();
        // Target `b`: the targetroot exists (class output) but no META-INF/semanticdb.
        let root_b = dir.path().join("out-b");
        std::fs::create_dir_all(&root_b).unwrap();

        let targets = DoctorTargets {
            all_ids: vec!["a".to_string(), "b".to_string()],
            unavailable_ids: Vec::new(),
            indexable_roots: vec![
                ("a".to_string(), root_a.clone()),
                ("b".to_string(), root_b.clone()),
            ],
            ..Default::default()
        };
        let section = semanticdb_section(&targets);
        assert_eq!(section.roots.len(), 2);
        // `a`: exists, one file, root reported as the META-INF/semanticdb dir.
        assert!(section.roots[0].exists, "a should exist");
        assert_eq!(section.roots[0].semanticdb_file_count, 1);
        assert!(
            section.roots[0]
                .semanticdb_root
                .ends_with("META-INF/semanticdb"),
            "{}",
            section.roots[0].semanticdb_root
        );
        // `b`: targetroot exists but the semanticdb dir does not → missing.
        assert!(!section.roots[1].exists, "b's semanticdb dir is missing");
        assert_eq!(section.roots[1].semanticdb_file_count, 0);
    }

    // ingest_summary reproduces Bootstrap.ingestSummary's format byte-for-byte.
    #[test]
    fn ingest_summary_matches_the_scala_format() {
        let report = IngestReport {
            segment_id: 3,
            docs_indexed: 25,
            docs_shared: 1,
            docs_stale: 0,
            docs_skipped: 2,
            symbol_count: 100,
            ref_group_count: 40,
            rename_group_count: 30,
            stale_uris: Vec::new(),
            skipped_uris: Vec::new(),
            parse_errors: Vec::new(),
            duration_ms: 12,
        };
        assert_eq!(
            ingest_summary(&report),
            "ingest: segment 3, 25 docs (1 shared, 0 stale, 2 skipped), \
             100 symbols, 40 ref groups, 30 rename groups in 12ms"
        );
    }
}
