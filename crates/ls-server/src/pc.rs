//! The presentation-compiler query seam and its production implementation over
//! the embedded in-process JVM island.
//!
//! [`PcQueryService`] is the narrow interface the ready path calls (the Scala
//! `CoreServices.pc` facade surface, restricted here to the document lifecycle
//! and the definition family). [`IslandPcService`] is the production
//! implementation: the document notifications keep a Rust-side mirror of the
//! open buffers in sync as they arrive, and it lazily boots the `ls-jvm` island
//! on the FIRST presentation-compiler QUERY (so an index-only session that opens
//! buffers but never issues a PC query keeps a zero-JVM process). On boot it
//! registers the workspace's PC targets, replays the mirrored open buffers into
//! the fresh island, and thereafter dispatches every notification and query over
//! the flat `#[repr(C)]` boundary. Cross-file go-to-definition falls through the
//! presentation compiler to the installed `symbol_definition` resolver, which
//! answers from the global index.
//!
//! [`pc_options`] strips the SemanticDB-generation flags from a target's scalac
//! options exactly as the Scala `Bootstrap.pcOptions` does, so the presentation
//! compiler runs without re-emitting SemanticDB.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use ls_jvm::backend::VtableBackend;
use ls_jvm::watchdog::{PayloadQueryKind, PcError, PcRequest, QueryKind, Supervisor};
use ls_jvm::{
    boot_island, install_definition_source_toplevels_resolver, install_search_methods_resolver,
    install_symbol_definition_resolver, IslandConfig,
};
use ls_pc_abi::payloads::{
    origin, AutoImport, AutoImportParams, AutoImportsResult, CodeActionParams, CodeActionResult,
    CompletionItem, CompletionList, DefinitionResult, FoldingRange, FoldingRangesResult,
    HoverResult, InlayHint, InlayHintParams, InlayHintsResult, LocationsResult, MethodHitsResult,
    PcDiagnostic, PcDiagnosticsResult, PluginStatus, Pos, PrepareRenameResult, Rng,
    SelectionRangeParams, SelectionRangesResult, SemanticNode, SemanticTokensResult, SignatureHelp,
    TargetConfig, ToplevelsResult, UriParams,
};
use serde_json::Value;

/// A resolved definition location, in the LSP coordinate system (zero-based
/// lines, UTF-16 characters, end-exclusive). The seam's own type so the trait
/// and its fakes do not depend on the ABI carrier crate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcLocation {
    pub uri: String,
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// A source span in LSP coordinates (zero-based line, UTF-16 character,
/// end-exclusive) — the seam's own type so the trait and its fakes do not
/// depend on the ABI carrier crate. The dirty-buffer overlay reads it from PC
/// `prepare_rename` as the presentation-only occurrence span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcSpan {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// Where a PC definition location resolves (mirrors the Scala `DefinitionOrigin`
/// and the ABI `origin` ordinals). The overlay treats anything that is not
/// [`PcDefOrigin::Workspace`] as a PC-plugin / synthetic origin.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PcDefOrigin {
    Workspace,
    Synthetic,
    Plugin,
}

/// One resolved definition location plus its origin.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcDefLocation {
    pub uri: String,
    pub span: PcSpan,
    pub origin: PcDefOrigin,
}

/// A PC definition result preserving the resolved SemanticDB `symbol` plus its
/// origin-tagged locations — the overlay's symbol-at-cursor truth over a dirty
/// buffer (the Scala `DefinitionResult`). An empty `symbol` means the
/// presentation compiler could not resolve a symbol at the cursor.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PcDefinition {
    pub symbol: String,
    pub locations: Vec<PcDefLocation>,
}

/// One compiler plugin's status (the island's `CompilerPluginStatus`) — the
/// seam's own type so the trait and its fakes do not depend on the ABI carrier
/// crate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PcCompilerPluginStatus {
    pub jars: Vec<String>,
    pub options: Vec<String>,
    pub loaded: bool,
    pub detail: String,
}

/// One service plugin's status (the island's `ServicePluginStatus`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PcServicePluginStatus {
    pub id: String,
    pub source: String,
    pub enabled: bool,
    pub self_test_ok: bool,
    pub self_test_detail: String,
}

/// A plugin the island disabled, with the reason (the island's `DisabledPlugin`).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PcDisabledPlugin {
    pub id: String,
    pub reason: String,
}

/// The island's plugin-status report (the Scala `PcFacade.pluginStatus` /
/// `PcPluginStatusReport`): the compiler plugins, the service plugins, and the
/// disabled plugins — the seam's mirror of the ABI `PluginStatus` carrier, so
/// the doctor and the `pcPluginStatus` command consume an ABI-free shape.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PcPluginStatusReport {
    pub compiler_plugins: Vec<PcCompilerPluginStatus>,
    pub service_plugins: Vec<PcServicePluginStatus>,
    pub disabled: Vec<PcDisabledPlugin>,
}

/// The presentation-compiler capability the ready services own. The document
/// lifecycle (`did_open`/`did_change`/`did_close`) mirrors the editor's open-
/// buffer state into the presentation compiler as the notifications arrive, and
/// the queries run against that mirrored state — the Scala `CoreServices.pc`
/// facade surface (the document notifications plus the definition family). A
/// query is served only for a buffer the mirror holds ([`PcQueryService::is_open`],
/// the `withPcBuffer` gate); the definition family answers empty when the
/// presentation compiler yields nothing.
pub trait PcQueryService: Send + Sync {
    /// Mirror a newly opened buffer (owned by `target_id`) into the presentation
    /// compiler. Never boots the island (an index-only session that opens buffers
    /// but issues no PC query keeps a zero-JVM process).
    fn did_open(&self, target_id: &str, uri: &str, text: &str);

    /// Update a mirrored buffer's text. Never boots the island.
    fn did_change(&self, uri: &str, text: &str);

    /// Drop a buffer from the mirror. Never boots the island.
    fn did_close(&self, uri: &str);

    /// Whether the mirror currently holds an open buffer for `uri` — the Scala
    /// `pc.bufferText(uri).isDefined`, the `withPcBuffer` precondition.
    fn is_open(&self, uri: &str) -> bool;

    /// Whether a PC query would run against an ALREADY-RUNNING island — i.e.
    /// this call itself never boots anything. The live-typing diagnostics
    /// scheduler gates its debounced `pc_diagnostics` pull on this, so a
    /// `didChange` alone NEVER boots the embedded JVM (preserving the
    /// index-only zero-JVM invariant and the blackbox suite's hermeticity);
    /// live diagnostics activate once some real PC query (hover, completion,
    /// semantic tokens, …) has booted the island. Default `true`: a fake or
    /// PC-less bundle has nothing to boot, so its pulls always run.
    fn booted(&self) -> bool {
        true
    }

    /// Go-to-definition of the symbol at `(line, character)` in the mirrored
    /// buffer `uri`. Empty when the presentation compiler yields nothing.
    fn definition(&self, uri: &str, line: u32, character: u32) -> Vec<PcLocation>;

    /// Go-to-type-definition, otherwise identical to [`PcQueryService::definition`].
    fn type_definition(&self, uri: &str, line: u32, character: u32) -> Vec<PcLocation>;

    /// Completion at `(line, character)` in the mirrored buffer `uri`, as an LSP
    /// `CompletionList` JSON value. An empty, complete list when the presentation
    /// compiler yields nothing (the Scala `emptyCompletions()` fallback).
    fn completion(&self, uri: &str, line: u32, character: u32) -> Value;

    /// Hover at `(line, character)`, as an LSP `Hover` JSON value, or `null` when
    /// the presentation compiler has nothing at the point.
    fn hover(&self, uri: &str, line: u32, character: u32) -> Value;

    /// Signature help at `(line, character)`, as an LSP `SignatureHelp` JSON value,
    /// or `null` when the presentation compiler has nothing at the point.
    fn signature_help(&self, uri: &str, line: u32, character: u32) -> Value;

    /// Prepare-rename span at `(line, character)` in the mirrored buffer `uri` —
    /// the presentation compiler's rename range when the symbol is file-locally
    /// renameable, else `None` (dotty offers rename ranges only for file-local
    /// symbols). The dirty-buffer overlay uses it as the presentation-only
    /// occurrence span, falling back to the identifier token under the cursor.
    /// Default `None`: a services bundle with no PC island offers no range.
    fn prepare_rename(&self, _uri: &str, _line: u32, _character: u32) -> Option<PcSpan> {
        None
    }

    /// Definition at `(line, character)` in the mirrored buffer `uri`, preserving
    /// the resolved SemanticDB symbol and per-location [`PcDefOrigin`] — the
    /// overlay's symbol-at-cursor truth over a dirty buffer (the Scala
    /// `pc.definition` feeding `PcOverlay`). Default empty: no island resolves
    /// nothing.
    fn definition_result(&self, _uri: &str, _line: u32, _character: u32) -> PcDefinition {
        PcDefinition::default()
    }

    /// Whether `target_id` is a registered PC target config — the Scala
    /// `s.pcConfigs.contains(target)` gate that a completion-resolve must pass
    /// before it runs against the presentation compiler.
    fn is_registered(&self, target_id: &str) -> bool;

    /// The registered PC target ids (sorted), for the doctor `PC` section.
    /// Non-invasive: reads the config mirror, never boots the island. Default
    /// empty (a services bundle with no PC island registers nothing).
    fn registered_targets(&self) -> Vec<String> {
        Vec::new()
    }

    /// The island's plugin-status report (the Scala `s.pc.pluginStatus`), for
    /// the doctor `PC Plugins` section and the `pcPluginStatus` command.
    /// `None` when there is no BOOTED island to ask — the inspection never
    /// boots the JVM (the pre-boot invariant lives in the production
    /// implementation), so a cold island reads as `None`, not as a boot.
    /// Default `None`: a services bundle with no PC island has no report.
    fn plugin_status(&self) -> Option<PcPluginStatusReport> {
        None
    }

    /// Resolve (enrich) an LSP completion `item` — carrying SemanticDB `symbol`,
    /// against the presentation compiler for `target_id` — returning the enriched
    /// item as LSP JSON. Degrades to the original `item` unchanged on any
    /// boundary/decode/encode failure (the Scala `resolved.getOrElse(item)`
    /// fallback: a resolve that cannot enrich returns the item the client already
    /// has, never an error).
    fn resolve_completion_item(&self, target_id: &str, symbol: &str, item: &Value) -> Value;

    /// Replace the registered PC target set with `targets` after a build-target
    /// change, reusing the same island. Updates the `is_registered` gate and the
    /// boot-replay source immediately; when the island has already booted, the
    /// new targets are (re-)registered into the running island. The Scala
    /// `reloadBuildModel` reuse of `s.pc` with the refetched `pcConfigs`. Default
    /// no-op: a services bundle with no PC island keeps its (empty) target set.
    fn reconfigure_targets(&self, _targets: Vec<TargetConfig>) {}

    /// `workspace/didChangeConfiguration` arrived. The notification's `settings`
    /// payload is deliberately ignored — the workspace
    /// `.scala3-bsp-semantic-ls/config.json` stays the single configuration
    /// source — the hook only gives the service a chance to re-read that file.
    /// Default no-op: a services bundle with no PC island has no config to
    /// re-read.
    fn on_config_changed(&self) {}

    // --- ABI v2 payload-query ops. ------------------------------------------
    //
    // Each returns the DECODED payload carrier — the LSP surface mapping is
    // the editor-facing wiring's job — and degrades to the empty/None fallback
    // on any boundary error (including `STATUS_NOT_YET`, the answer a
    // transport-first future op would give). Defaults are empty so fakes and
    // PC-less bundles compile.

    /// Inlay hints for `range` of the mirrored buffer `uri` (`flags` is the
    /// boundary hint-category bitset). Default empty.
    fn inlay_hints(&self, _uri: &str, _range: Rng, _flags: u32) -> Vec<InlayHint> {
        Vec::new()
    }

    /// Semantic tokens of the mirrored buffer `uri`, as offset-based nodes
    /// (the caller converts offsets to line/character). Default empty.
    fn semantic_tokens(&self, _uri: &str) -> Vec<SemanticNode> {
        Vec::new()
    }

    /// Per query position, the chain of enclosing selection ranges, innermost
    /// first. Default empty.
    fn selection_range(&self, _uri: &str, _positions: &[Pos]) -> Vec<Vec<Rng>> {
        Vec::new()
    }

    /// Run the PC-backed code action (`ls_pc_abi::payloads::code_action_id`)
    /// at `position`; a typed refusal comes back as data on the result.
    /// Default empty (no edits, no refusal).
    fn code_action(
        &self,
        _uri: &str,
        _action: i32,
        _position: Pos,
        _extraction_end: Option<Pos>,
        _arg_indices: Option<Vec<i32>>,
    ) -> CodeActionResult {
        CodeActionResult::default()
    }

    /// Auto-import candidates for `name` at `position`, best first. Default
    /// empty.
    fn auto_imports(
        &self,
        _uri: &str,
        _position: Pos,
        _name: &str,
        _is_extension: bool,
    ) -> Vec<AutoImport> {
        Vec::new()
    }

    /// The mirrored buffer's presentation-compiler diagnostics. Default empty.
    fn pc_diagnostics(&self, _uri: &str) -> Vec<PcDiagnostic> {
        Vec::new()
    }

    /// Folding ranges of the mirrored buffer `uri`. Default empty.
    fn folding_range(&self, _uri: &str) -> Vec<FoldingRange> {
        Vec::new()
    }
}

/// The `symbol_definition` resolver the island calls when the presentation
/// compiler has no in-buffer source position for a cross-file symbol. Answers
/// from the global index (`QueryOrchestrator::symbol_definition`).
pub type SymbolResolver = dyn Fn(&str, &str) -> LocationsResult + Send + Sync;

/// The `search_methods` resolver the island calls for member-mode workspace
/// extension-method discovery (`SymbolSearch.searchMethods`): `(query,
/// bsp_target_id) -> method hits`. Answers from the global index
/// (`QueryOrchestrator::search_methods`).
pub type SearchMethodsResolver = dyn Fn(&str, &str) -> MethodHitsResult + Send + Sync;

/// The `definition_source_toplevels` resolver the island calls for the
/// toplevel symbols of a definition source (`SymbolSearch.
/// definitionSourceToplevels`): `(semanticdb_symbol, source_uri) -> toplevel
/// symbols`. Answers from the global index — the bootstrap wires
/// `QueryOrchestrator::definition_source_toplevels` here.
pub type ToplevelsResolver = dyn Fn(&str, &str) -> ToplevelsResult + Send + Sync;

/// Strips the SemanticDB-generation flags from a target's scalac options so the
/// presentation compiler does not re-emit SemanticDB. Removes `-Xsemanticdb`,
/// `-Ysemanticdb`, and both the two-token (`-semanticdb-target <v>`) and colon
/// (`-semanticdb-target:<v>`) forms of `-semanticdb-target`/`-sourceroot`. A
/// behavior-preserving port of `Bootstrap.pcOptions`.
pub fn pc_options(scalac_options: &[String]) -> Vec<String> {
    const TWO_TOKEN: [&str; 2] = ["-semanticdb-target", "-sourceroot"];
    let mut out = Vec::new();
    let mut i = 0;
    while i < scalac_options.len() {
        let opt = &scalac_options[i];
        if opt == "-Xsemanticdb" || opt == "-Ysemanticdb" {
            // Drop the single-token generation flags.
        } else if TWO_TOKEN.contains(&opt.as_str()) && i + 1 < scalac_options.len() {
            // Drop the flag and skip its separate value token.
            i += 1;
        } else if TWO_TOKEN.iter().any(|f| opt.starts_with(&format!("{f}:"))) {
            // Drop the colon form (value fused onto the flag).
        } else {
            out.push(opt.clone());
        }
        i += 1;
    }
    out
}

/// The booted-island dispatch seam: the operations the outer mirror needs
/// from the `ls-jvm` [`Supervisor`] — the dispatch-lane `request` plus the
/// control-lane `plugin_status`. A trait so the status-aware mirror logic is
/// testable with a fake that returns `Ok`/`RequestTimeout`/`Backend` outcomes
/// (and queued status bytes) without a live JVM.
trait PcDriver: Send {
    fn request(&mut self, request: PcRequest) -> Result<Vec<u8>, PcError>;

    /// The raw encoded `PluginStatus` reply, fetched over the control lane
    /// (worker 1) so it answers even while the dispatch lane is busy. `Err`
    /// carries the vtable status.
    fn plugin_status(&mut self) -> Result<Vec<u8>, i32>;
}

impl PcDriver for Supervisor<VtableBackend> {
    fn request(&mut self, request: PcRequest) -> Result<Vec<u8>, PcError> {
        Supervisor::request(self, request)
    }

    fn plugin_status(&mut self) -> Result<Vec<u8>, i32> {
        Supervisor::plugin_status(self)
    }
}

/// Whether a lifecycle forward's outcome should be reflected in the outer mirror.
/// A success (`Ok`) or a timeout-recovery (`RequestTimeout` — the editor's
/// notification is a fact the generation replay honors) mutates the mirror; a
/// clean nonzero backend status (`PcError::Backend`, e.g. an open for an
/// unregistered target) must NOT — the island did not apply it, so the outer
/// mirror must not claim it. This mirrors `Supervisor::observe`, which updates
/// its inner recovery mirror on `Done`/`Wedged` but never on `Status`.
fn accepted(outcome: &Result<Vec<u8>, PcError>) -> bool {
    !matches!(outcome, Err(PcError::Backend(_)))
}

/// The lazily-booted embedded PC island. Constructed with the workspace's PC
/// target registrations and the index-backed resolvers (`symbol_definition` +
/// `search_methods`), but the JVM is not started until the first PC request.
pub struct IslandPcService {
    state: Mutex<IslandState>,
}

struct IslandState {
    workspace_root: PathBuf,
    /// The PC target registrations, replayed into the island on boot; also the
    /// registered-target lookup that gates `did_open` (the Scala `PcFacade`
    /// `require(targets.contains(targetId), …)`).
    targets: Vec<TargetConfig>,
    /// The `symbol_definition` resolver, installed into the island's global slot
    /// once, at boot; taken then.
    resolver: Option<Box<SymbolResolver>>,
    /// The `search_methods` resolver, installed into the island's global slot
    /// once, at boot; taken then (the second resolver closure next to
    /// `symbol_definition`).
    search_resolver: Option<Box<SearchMethodsResolver>>,
    /// The `definition_source_toplevels` resolver, installed into the island's
    /// global slot once, at boot; taken then (the third resolver closure).
    toplevels_resolver: Option<Box<ToplevelsResolver>>,
    /// The mirrored open buffers (`uri -> (owning target, text)`), replayed into
    /// the island on boot and the source of the `is_open`/`withPcBuffer` gate.
    /// Kept status-aware: a buffer is recorded only after the island has actually
    /// accepted the corresponding lifecycle request (or while still cold).
    buffers: BTreeMap<String, Buffered>,
    /// `None` until the first PC query boots the island.
    driver: Option<Box<dyn PcDriver>>,
    /// A recorded boot failure, so a broken environment is reported once and the
    /// service then degrades to empty rather than re-attempting a boot per request.
    boot_error: Option<String>,
}

impl IslandState {
    /// Whether a target id was registered — the `PcFacade` `require` precondition
    /// for `did_open`.
    fn is_registered(&self, target_id: &str) -> bool {
        self.targets.iter().any(|t| t.bsp_id == target_id)
    }
}

#[derive(Clone)]
struct Buffered {
    target_id: String,
    text: String,
}

/// A generous per-request deadline: it only bounds a *wedged* request (a healthy
/// query returns well within it), and the first query after a cold boot pays the
/// presentation compiler's class-load + init under `nix flake check` parallelism,
/// so it is sized like the live sweep rather than the 15s production budget.
const REQUEST_DEADLINE: Duration = Duration::from_secs(120);

/// The per-request deadline used ONLY when the test fault seam (`LS_PC_TEST_FAULT`)
/// is armed: below the fault hook's ~60s wedge busy-loop so a wedged dispatch
/// times out (and the dispatch-generation recovery is observable) within a test,
/// yet high enough for a cold first completion under parallel-CI load.
const FAULT_REQUEST_DEADLINE: Duration = Duration::from_secs(20);
/// The premain registration deadline, sized for a cold JVM boot under parallel
/// live checks.
const RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(60);

impl IslandPcService {
    /// Build the service from the workspace's PC target registrations and the
    /// index-backed resolvers (`symbol_definition` + `search_methods` +
    /// `definition_source_toplevels`). Does not boot the JVM.
    pub fn new(
        workspace_root: PathBuf,
        targets: Vec<TargetConfig>,
        resolver: Box<SymbolResolver>,
        search_resolver: Box<SearchMethodsResolver>,
        toplevels_resolver: Box<ToplevelsResolver>,
    ) -> IslandPcService {
        IslandPcService {
            state: Mutex::new(IslandState {
                workspace_root,
                targets,
                resolver: Some(resolver),
                search_resolver: Some(search_resolver),
                toplevels_resolver: Some(toplevels_resolver),
                buffers: BTreeMap::new(),
                driver: None,
                boot_error: None,
            }),
        }
    }

    /// Whether the embedded JVM island has booted. A document notification must
    /// never boot it — only a query does.
    #[cfg(test)]
    fn is_booted(&self) -> bool {
        self.state
            .lock()
            .expect("pc island state mutex")
            .driver
            .is_some()
    }

    /// The mirrored text of an open buffer, or `None` if the mirror does not hold
    /// it — the replay source the boot would feed the island.
    #[cfg(test)]
    fn buffer_text(&self, uri: &str) -> Option<String> {
        self.state
            .lock()
            .expect("pc island state mutex")
            .buffers
            .get(uri)
            .map(|b| b.text.clone())
    }

    /// Boots the island on the first PC request (booting + replaying the mirrored
    /// buffers), dispatches `request` over the booted driver, and returns the raw
    /// reply bytes. `None` on a boot failure or any boundary error — the shared
    /// path behind every PC query and completion resolve, so an index-only session
    /// that only opens buffers keeps a zero-JVM process until the first request.
    fn dispatch(&self, request: PcRequest) -> Option<Vec<u8>> {
        let mut guard = self.state.lock().expect("pc island state mutex");
        let state = &mut *guard;
        if state.driver.is_none() && !boot(state) {
            return None;
        }
        let driver = state.driver.as_mut()?;
        driver.request(request).ok()
    }

    /// Dispatches a query against the already-mirrored buffer and returns the raw
    /// reply bytes; the per-query carrier is decoded by the caller. `None` degrades
    /// to each query method's own empty/null fallback — matching the Scala PC
    /// methods' empty/null result when the compiler yields nothing.
    fn query_reply(
        &self,
        kind: QueryKind,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Option<Vec<u8>> {
        self.dispatch(PcRequest::Query {
            kind,
            uri: uri.to_string(),
            line,
            character,
        })
    }

    /// Dispatches a definition-family query and decodes the `DefinitionResult`
    /// carrier to seam locations, degrading to empty on any boundary/decode failure.
    fn definition_query(
        &self,
        kind: QueryKind,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Vec<PcLocation> {
        self.query_reply(kind, uri, line, character)
            .and_then(|reply| DefinitionResult::decode(&reply).ok())
            .map(|result| result.locations.into_iter().map(pc_location_of).collect())
            .unwrap_or_default()
    }

    /// Dispatches a payload-query op: encodes the typed params, routes the
    /// `PcRequest::PayloadQuery` through the (lazily booted) driver, and
    /// returns the raw reply bytes. `None` on an encode failure, a boot
    /// failure, or any boundary error — including the island's
    /// `STATUS_NOT_YET` transport-stub answer, which surfaces as
    /// `PcError::Backend(STATUS_NOT_YET)` and degrades to each method's empty
    /// fallback exactly like every other error status.
    fn payload_query_reply(
        &self,
        kind: PayloadQueryKind,
        params: Result<Vec<u8>, ls_pc_abi::AbiError>,
    ) -> Option<Vec<u8>> {
        let params = params.ok()?;
        self.dispatch(PcRequest::PayloadQuery { kind, params })
    }
}

impl PcQueryService for IslandPcService {
    fn did_open(&self, target_id: &str, uri: &str, text: &str) {
        let mut guard = self.state.lock().expect("pc island state mutex");
        let state = &mut *guard;
        // The Scala `PcFacade.didOpen` requires the target registered *before*
        // adding the buffer; an open for an unknown target is never mirrored.
        if !state.is_registered(target_id) {
            return;
        }
        // A booted island is forwarded to first; the outer mirror records the
        // buffer only if the island accepted it (or a timeout recovery replays
        // it) — never on a clean backend rejection. A not-yet-booted island stays
        // cold and picks the buffer up from the mirror on its boot replay.
        if let Some(driver) = state.driver.as_mut() {
            let outcome = driver.request(PcRequest::DidOpen {
                target_id: target_id.to_string(),
                uri: uri.to_string(),
                text: text.to_string(),
            });
            if !accepted(&outcome) {
                return;
            }
        }
        state.buffers.insert(
            uri.to_string(),
            Buffered {
                target_id: target_id.to_string(),
                text: text.to_string(),
            },
        );
    }

    fn did_change(&self, uri: &str, text: &str) {
        let mut guard = self.state.lock().expect("pc island state mutex");
        let state = &mut *guard;
        // The handler only calls `did_change` for a buffer the mirror already
        // holds; a change for an unheld buffer is a no-op.
        if !state.buffers.contains_key(uri) {
            return;
        }
        // Forward first; update the mirror text only if the island accepted the
        // change (or a timeout recovery). On a clean backend rejection the old
        // replayable text is preserved, so a later boot replay feeds the island
        // the last text it actually accepted.
        if let Some(driver) = state.driver.as_mut() {
            let outcome = driver.request(PcRequest::DidChange {
                uri: uri.to_string(),
                text: text.to_string(),
            });
            if !accepted(&outcome) {
                return;
            }
        }
        if let Some(buffered) = state.buffers.get_mut(uri) {
            buffered.text = text.to_string();
        }
    }

    fn did_close(&self, uri: &str) {
        let mut guard = self.state.lock().expect("pc island state mutex");
        let state = &mut *guard;
        // Forward first; drop the buffer from the mirror only if the island
        // accepted the close (or a timeout recovery). On a clean backend rejection
        // the island still holds the buffer, so keeping it in the mirror avoids an
        // outer/inner replay divergence. A cold island simply drops it.
        if let Some(driver) = state.driver.as_mut() {
            let outcome = driver.request(PcRequest::DidClose {
                uri: uri.to_string(),
            });
            if !accepted(&outcome) {
                return;
            }
        }
        state.buffers.remove(uri);
    }

    fn is_open(&self, uri: &str) -> bool {
        self.state
            .lock()
            .expect("pc island state mutex")
            .buffers
            .contains_key(uri)
    }

    fn booted(&self) -> bool {
        // Only inspects the driver slot — NEVER takes the boot path (the same
        // pre-boot invariant `plugin_status` keeps): a cold island reads as
        // not booted, so a debounced diagnostics pull skips it.
        self.state
            .lock()
            .expect("pc island state mutex")
            .driver
            .is_some()
    }

    fn definition(&self, uri: &str, line: u32, character: u32) -> Vec<PcLocation> {
        self.definition_query(QueryKind::Definition, uri, line, character)
    }

    fn type_definition(&self, uri: &str, line: u32, character: u32) -> Vec<PcLocation> {
        self.definition_query(QueryKind::TypeDefinition, uri, line, character)
    }

    fn completion(&self, uri: &str, line: u32, character: u32) -> Value {
        self.query_reply(QueryKind::Completion, uri, line, character)
            .and_then(|reply| CompletionList::decode(&reply).ok())
            .map(|list| crate::pc_convert::completion_list(&list))
            .unwrap_or_else(crate::pc_convert::empty_completions)
    }

    fn hover(&self, uri: &str, line: u32, character: u32) -> Value {
        self.query_reply(QueryKind::Hover, uri, line, character)
            .and_then(|reply| HoverResult::decode(&reply).ok())
            .map(|result| crate::pc_convert::hover_result(&result))
            .unwrap_or(Value::Null)
    }

    fn signature_help(&self, uri: &str, line: u32, character: u32) -> Value {
        self.query_reply(QueryKind::SignatureHelp, uri, line, character)
            .and_then(|reply| SignatureHelp::decode(&reply).ok())
            .map(|help| crate::pc_convert::signature_help(&help))
            .unwrap_or(Value::Null)
    }

    fn prepare_rename(&self, uri: &str, line: u32, character: u32) -> Option<PcSpan> {
        self.query_reply(QueryKind::PrepareRename, uri, line, character)
            .and_then(|reply| PrepareRenameResult::decode(&reply).ok())
            .and_then(|result| result.0)
            .map(pc_span_of)
    }

    fn definition_result(&self, uri: &str, line: u32, character: u32) -> PcDefinition {
        self.query_reply(QueryKind::Definition, uri, line, character)
            .and_then(|reply| DefinitionResult::decode(&reply).ok())
            .map(pc_definition_of)
            .unwrap_or_default()
    }

    fn is_registered(&self, target_id: &str) -> bool {
        self.state
            .lock()
            .expect("pc island state mutex")
            .is_registered(target_id)
    }

    fn registered_targets(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .state
            .lock()
            .expect("pc island state mutex")
            .targets
            .iter()
            .map(|t| t.bsp_id.clone())
            .collect();
        ids.sort();
        ids
    }

    fn plugin_status(&self) -> Option<PcPluginStatusReport> {
        let mut guard = self.state.lock().expect("pc island state mutex");
        let state = &mut *guard;
        // THE pre-boot invariant, kept in this one place: a plugin-status
        // inspection of a still-cold island answers `None` and NEVER takes the
        // boot path — only a PC query boots the island. The doctor and the
        // `pcPluginStatus` command derive their "cold" wording from this `None`.
        let driver = state.driver.as_mut()?;
        let reply = driver.plugin_status().ok()?;
        let status = PluginStatus::decode(&reply).ok()?;
        Some(plugin_status_report_of(status))
    }

    fn resolve_completion_item(&self, target_id: &str, symbol: &str, item: &Value) -> Value {
        // Re-encode the item the client echoed back into the flat carrier the
        // island decodes; a carrier that will not encode degrades to the item.
        let Ok(encoded) = crate::pc_convert::completion_item_to_carrier(item).encode() else {
            return item.clone();
        };
        self.dispatch(PcRequest::Resolve {
            target_id: target_id.to_string(),
            symbol: symbol.to_string(),
            item: encoded,
        })
        .and_then(|reply| CompletionItem::decode(&reply).ok())
        .map(|resolved| crate::pc_convert::completion_item(&resolved))
        .unwrap_or_else(|| item.clone())
    }

    fn reconfigure_targets(&self, targets: Vec<TargetConfig>) {
        let mut guard = self.state.lock().expect("pc island state mutex");
        let state = &mut *guard;
        // The new set is the gate + the boot-replay source immediately; a still-
        // cold island simply registers it on its eventual boot (no re-boot of the
        // one-per-process JVM). A dropped target falls out of `is_registered`, so a
        // `did_open` against it is rejected even if its island-side registration
        // lingers inertly.
        state.targets = targets;
        // If the island has already booted, (re-)register the new targets into the
        // running island so a query against a newly added target's buffer resolves.
        // A rejected registration is recorded like a boot failure would be, but the
        // mirror still reflects the intended set (the gate stays authoritative). The
        // failure is recorded after the driver borrow ends, so `state` is not
        // aliased (mirrors `install_driver`'s snapshot discipline).
        let registrations = state.targets.clone();
        let mut rejected = None;
        if let Some(driver) = state.driver.as_mut() {
            for target in &registrations {
                let outcome = driver.request(PcRequest::RegisterTarget {
                    id: target.bsp_id.clone(),
                    config: target.clone(),
                });
                if !accepted(&outcome) {
                    rejected = Some(format!(
                        "PC target '{}' re-registration rejected by the island",
                        target.bsp_id
                    ));
                    break;
                }
            }
        }
        if let Some(detail) = rejected {
            state.boot_error = Some(detail);
        }
    }

    fn inlay_hints(&self, uri: &str, range: Rng, flags: u32) -> Vec<InlayHint> {
        self.payload_query_reply(
            PayloadQueryKind::InlayHints,
            InlayHintParams {
                uri: uri.to_string(),
                range,
                flags,
            }
            .encode(),
        )
        .and_then(|reply| InlayHintsResult::decode(&reply).ok())
        .map(|result| result.hints)
        .unwrap_or_default()
    }

    fn semantic_tokens(&self, uri: &str) -> Vec<SemanticNode> {
        self.payload_query_reply(
            PayloadQueryKind::SemanticTokens,
            UriParams {
                uri: uri.to_string(),
            }
            .encode(),
        )
        .and_then(|reply| SemanticTokensResult::decode(&reply).ok())
        .map(|result| result.nodes)
        .unwrap_or_default()
    }

    fn selection_range(&self, uri: &str, positions: &[Pos]) -> Vec<Vec<Rng>> {
        self.payload_query_reply(
            PayloadQueryKind::SelectionRange,
            SelectionRangeParams {
                uri: uri.to_string(),
                positions: positions.to_vec(),
            }
            .encode(),
        )
        .and_then(|reply| SelectionRangesResult::decode(&reply).ok())
        .map(|result| result.chains)
        .unwrap_or_default()
    }

    fn code_action(
        &self,
        uri: &str,
        action: i32,
        position: Pos,
        extraction_end: Option<Pos>,
        arg_indices: Option<Vec<i32>>,
    ) -> CodeActionResult {
        self.payload_query_reply(
            PayloadQueryKind::CodeAction,
            CodeActionParams {
                uri: uri.to_string(),
                action,
                position,
                extraction_end,
                arg_indices,
            }
            .encode(),
        )
        .and_then(|reply| CodeActionResult::decode(&reply).ok())
        .unwrap_or_default()
    }

    fn auto_imports(
        &self,
        uri: &str,
        position: Pos,
        name: &str,
        is_extension: bool,
    ) -> Vec<AutoImport> {
        self.payload_query_reply(
            PayloadQueryKind::AutoImports,
            AutoImportParams {
                uri: uri.to_string(),
                position,
                name: name.to_string(),
                is_extension,
            }
            .encode(),
        )
        .and_then(|reply| AutoImportsResult::decode(&reply).ok())
        .map(|result| result.imports)
        .unwrap_or_default()
    }

    fn pc_diagnostics(&self, uri: &str) -> Vec<PcDiagnostic> {
        self.payload_query_reply(
            PayloadQueryKind::PcDiagnostics,
            UriParams {
                uri: uri.to_string(),
            }
            .encode(),
        )
        .and_then(|reply| PcDiagnosticsResult::decode(&reply).ok())
        .map(|result| result.diagnostics)
        .unwrap_or_default()
    }

    fn folding_range(&self, uri: &str) -> Vec<FoldingRange> {
        self.payload_query_reply(
            PayloadQueryKind::FoldingRange,
            UriParams {
                uri: uri.to_string(),
            }
            .encode(),
        )
        .and_then(|reply| FoldingRangesResult::decode(&reply).ok())
        .map(|result| result.ranges)
        .unwrap_or_default()
    }

    fn on_config_changed(&self) {
        let mut guard = self.state.lock().expect("pc island state mutex");
        let state = &mut *guard;
        // A booted island cannot re-home its one-per-process JVM: the change is
        // logged and ignored (a new `javaHome` applies from the next server
        // start).
        if state.driver.is_some() {
            eprintln!(
                "ls-server: workspace/didChangeConfiguration ignored: \
                 the PC island is already booted"
            );
            return;
        }
        // Still cold: un-latch a recorded boot failure so the NEXT PC query
        // re-reads config.json and re-attempts the boot (the user may have just
        // fixed `javaHome`).
        state.boot_error = None;
    }
}

/// The workspace-level `javaHome` override, read from the optional
/// `.scala3-bsp-semantic-ls/config.json` at the workspace root. Absent file,
/// unparseable JSON, or a missing/non-string `javaHome` key all resolve to
/// `None` — the config tier simply does not apply.
fn workspace_config_java_home(workspace_root: &std::path::Path) -> Option<PathBuf> {
    let text =
        std::fs::read_to_string(workspace_root.join(".scala3-bsp-semantic-ls/config.json")).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    value.get("javaHome")?.as_str().map(PathBuf::from)
}

/// Normalizes a resolved Java home for the nixpkgs JDK package layout: a
/// nixpkgs JDK store path is a package ROOT whose real Java home is nested at
/// `<root>/lib/openjdk` (`release`, `lib/server/libjvm.so`, and friends live
/// there). When `home/release` is not a file but `home/lib/openjdk/release` is,
/// the nested home is returned; any other layout passes through unchanged.
fn normalize_java_home(home: PathBuf) -> PathBuf {
    if !home.join("release").is_file() {
        let nested = home.join("lib/openjdk");
        if nested.join("release").is_file() {
            return nested;
        }
    }
    home
}

/// Resolves the island's Java HOME by the same precedence
/// [`resolve_island_paths`] applies to the libjvm path — workspace config
/// `javaHome`, then `LS_LIBJVM` (stripping the `lib/server/libjvm.so` tail to
/// recover the home), then `JAVA_HOME` — normalized for the nixpkgs
/// package-root layout ([`normalize_java_home`]). `None` when no tier is set.
/// The doctor's `Runtime` probe reads the Java version from this home, so the
/// reported JVM is the one the island boot would actually use.
pub(crate) fn resolve_java_home(
    workspace_root: &std::path::Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    let home = if let Some(home) = workspace_config_java_home(workspace_root) {
        home
    } else if let Some(libjvm) = env("LS_LIBJVM") {
        // `<home>/lib/server/libjvm.so` -> `<home>` (three components up).
        std::path::Path::new(&libjvm)
            .ancestors()
            .nth(3)?
            .to_path_buf()
    } else if let Some(home) = env("JAVA_HOME") {
        PathBuf::from(home)
    } else {
        return None;
    };
    Some(normalize_java_home(home))
}

/// Resolves the embedded JVM's `libjvm.so` and the island host agent jar.
///
/// Java home precedence is config > env > nix-baked: the workspace config's
/// `javaHome` wins; then the environment (`LS_LIBJVM` as an exact libjvm path,
/// else `JAVA_HOME`); the nix-baked default is the packaged wrapper's
/// `--set-default JAVA_HOME`/`PC_HOST_AGENT_JAR`, which by construction only
/// applies when the variable is not already set. A resolved Java home locates
/// libjvm at `<home>/lib/server/libjvm.so`.
fn resolve_island_paths(
    workspace_root: &std::path::Path,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<(PathBuf, PathBuf), String> {
    let libjvm = if let Some(home) = workspace_config_java_home(workspace_root) {
        home.join("lib/server/libjvm.so")
    } else if let Some(path) = env("LS_LIBJVM") {
        PathBuf::from(path)
    } else if let Some(home) = env("JAVA_HOME") {
        // A nixpkgs JDK's JAVA_HOME is often the package ROOT, whose real home
        // (and libjvm) is nested at `lib/openjdk`; normalize so a bare
        // `JAVA_HOME=<nix package root>` boots without needing LS_LIBJVM.
        normalize_java_home(PathBuf::from(home)).join("lib/server/libjvm.so")
    } else {
        return Err("no Java home for the PC island: set javaHome in \
             .scala3-bsp-semantic-ls/config.json, or LS_LIBJVM / JAVA_HOME in the environment"
            .to_string());
    };
    let agent_jar = env("PC_HOST_AGENT_JAR").map(PathBuf::from).ok_or_else(|| {
        "PC_HOST_AGENT_JAR must point at the island host agent jar to boot the PC island"
            .to_string()
    })?;
    Ok((libjvm, agent_jar))
}

/// Boots the island: installs the resolver (once), reads the JVM environment,
/// boots, registers the targets, and replays the mirrored buffers. Records a
/// boot failure so a broken environment does not re-attempt per request.
/// Returns whether the supervisor is now available.
fn boot(state: &mut IslandState) -> bool {
    if state.boot_error.is_some() {
        return false;
    }
    let (libjvm, agent_jar) =
        match resolve_island_paths(&state.workspace_root, &|k| std::env::var(k).ok()) {
            Ok(paths) => paths,
            Err(detail) => {
                state.boot_error = Some(detail);
                return false;
            }
        };
    // The resolver slots are global and set-once; a second install (e.g. a
    // second workspace in the process) is ignored, which is correct — one
    // server, one process. Installed before boot so the premain sees them.
    if let Some(resolver) = state.resolver.take() {
        install_symbol_definition_resolver(resolver);
    }
    if let Some(search_resolver) = state.search_resolver.take() {
        install_search_methods_resolver(search_resolver);
    }
    if let Some(toplevels_resolver) = state.toplevels_resolver.take() {
        install_definition_source_toplevels_resolver(toplevels_resolver);
    }
    // A clearly test-scoped fault seam: when `LS_PC_TEST_FAULT` is set, arm the
    // Java-side fault property (`-Dls.pc.host.testFault=<kind>`) and tighten the
    // per-request deadline so a wedged dispatch times out — and the watchdog's
    // dispatch-generation recovery becomes observable — within a test. The env
    // var is unset in production, so production boot behavior is unchanged.
    let test_fault = std::env::var("LS_PC_TEST_FAULT").ok();
    let fault_options: Vec<String> = test_fault
        .as_deref()
        .filter(|kind| !kind.is_empty())
        .map(|kind| vec![format!("-Dls.pc.host.testFault={kind}")])
        .unwrap_or_default();
    let request_deadline = if fault_options.is_empty() {
        REQUEST_DEADLINE
    } else {
        FAULT_REQUEST_DEADLINE
    };
    let config = IslandConfig {
        libjvm: &libjvm,
        agent_jar: &agent_jar,
        extra_classpath: &[],
        workspace_root: Some(&state.workspace_root),
        extra_jvm_options: &fault_options,
        rendezvous_timeout: RENDEZVOUS_TIMEOUT,
        max_abandoned_generations: 4,
        request_deadline,
        cancel_grace: Duration::from_millis(500),
    };
    let sup = match boot_island(&config) {
        Ok(sup) => sup,
        Err(error) => {
            state.boot_error = Some(error.to_string());
            return false;
        }
    };
    install_driver(state, Box::new(sup))
}

/// Registers the workspace targets and replays the mirrored open buffers into a
/// freshly booted `driver`, then installs it — but only if the island accepted
/// every registration and buffer. A clean backend status on any `RegisterTarget`
/// or replayed `DidOpen` means the island missed part of the state, so the driver
/// is NOT installed (a half-populated island is never served) and the failure is
/// recorded. Split from [`boot`] so the register/replay discipline is testable
/// with a fake driver, without a live JVM.
fn install_driver(state: &mut IslandState, mut driver: Box<dyn PcDriver>) -> bool {
    // Snapshot the desired state so the register/replay loop does not alias the
    // `state` it may record a `boot_error` into.
    let targets = state.targets.clone();
    let buffers: Vec<(String, Buffered)> = state
        .buffers
        .iter()
        .map(|(uri, buffered)| (uri.clone(), buffered.clone()))
        .collect();
    for target in &targets {
        let outcome = driver.request(PcRequest::RegisterTarget {
            id: target.bsp_id.clone(),
            config: target.clone(),
        });
        if !accepted(&outcome) {
            state.boot_error = Some(format!(
                "PC target '{}' registration rejected by the island",
                target.bsp_id
            ));
            return false;
        }
    }
    for (uri, buffered) in &buffers {
        let outcome = driver.request(PcRequest::DidOpen {
            target_id: buffered.target_id.clone(),
            uri: uri.clone(),
            text: buffered.text.clone(),
        });
        if !accepted(&outcome) {
            state.boot_error = Some(format!("PC buffer '{uri}' replay rejected by the island"));
            return false;
        }
    }
    state.driver = Some(driver);
    true
}

/// ABI location carrier -> the seam's [`PcLocation`].
fn pc_location_of(loc: ls_pc_abi::payloads::Location) -> PcLocation {
    PcLocation {
        uri: loc.uri,
        start_line: loc.range.start_line,
        start_character: loc.range.start_character,
        end_line: loc.range.end_line,
        end_character: loc.range.end_character,
    }
}

fn pc_span_of(range: Rng) -> PcSpan {
    PcSpan {
        start_line: range.start_line,
        start_character: range.start_character,
        end_line: range.end_line,
        end_character: range.end_character,
    }
}

/// Maps an ABI `origin` ordinal to a [`PcDefOrigin`]. `WORKSPACE` and `PLUGIN`
/// map exactly; `SYNTHETIC` and any unknown ordinal fold to `Synthetic` (the
/// overlay only distinguishes workspace from non-workspace, so an unknown
/// ordinal safely reads as non-workspace).
fn pc_def_origin_of(ordinal: u32) -> PcDefOrigin {
    match ordinal {
        origin::WORKSPACE => PcDefOrigin::Workspace,
        origin::PLUGIN => PcDefOrigin::Plugin,
        _ => PcDefOrigin::Synthetic,
    }
}

/// ABI `PluginStatus` carrier -> the seam's [`PcPluginStatusReport`].
fn plugin_status_report_of(status: PluginStatus) -> PcPluginStatusReport {
    PcPluginStatusReport {
        compiler_plugins: status
            .compiler_plugins
            .into_iter()
            .map(|p| PcCompilerPluginStatus {
                jars: p.jars,
                options: p.options,
                loaded: p.loaded,
                detail: p.detail,
            })
            .collect(),
        service_plugins: status
            .service_plugins
            .into_iter()
            .map(|p| PcServicePluginStatus {
                id: p.id,
                source: p.source,
                enabled: p.enabled,
                self_test_ok: p.self_test_ok,
                self_test_detail: p.self_test_detail,
            })
            .collect(),
        disabled: status
            .disabled
            .into_iter()
            .map(|p| PcDisabledPlugin {
                id: p.id,
                reason: p.reason,
            })
            .collect(),
    }
}

fn pc_definition_of(result: DefinitionResult) -> PcDefinition {
    PcDefinition {
        symbol: result.symbol,
        locations: result
            .locations
            .into_iter()
            .map(|loc| PcDefLocation {
                uri: loc.uri,
                span: pc_span_of(loc.range),
                origin: pc_def_origin_of(loc.origin),
            })
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_of(pairs: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> {
        let owned: Vec<(String, String)> = pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |key: &str| owned.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
    }

    #[test]
    fn island_paths_resolve_java_home_from_the_environment() {
        let ws = tempfile::tempdir().unwrap();
        let env = env_of(&[("JAVA_HOME", "/jdk"), ("PC_HOST_AGENT_JAR", "/a.jar")]);
        let (libjvm, jar) = resolve_island_paths(ws.path(), &env).unwrap();
        assert_eq!(libjvm, PathBuf::from("/jdk/lib/server/libjvm.so"));
        assert_eq!(jar, PathBuf::from("/a.jar"));
    }

    #[test]
    fn island_paths_prefer_the_exact_ls_libjvm_override_over_java_home() {
        let ws = tempfile::tempdir().unwrap();
        let env = env_of(&[
            ("LS_LIBJVM", "/custom/libjvm.so"),
            ("JAVA_HOME", "/jdk"),
            ("PC_HOST_AGENT_JAR", "/a.jar"),
        ]);
        let (libjvm, _) = resolve_island_paths(ws.path(), &env).unwrap();
        assert_eq!(libjvm, PathBuf::from("/custom/libjvm.so"));
    }

    #[test]
    fn island_paths_prefer_the_workspace_config_over_every_env_tier() {
        let ws = tempfile::tempdir().unwrap();
        let conf_dir = ws.path().join(".scala3-bsp-semantic-ls");
        std::fs::create_dir_all(&conf_dir).unwrap();
        std::fs::write(
            conf_dir.join("config.json"),
            r#"{ "javaHome": "/config/jdk" }"#,
        )
        .unwrap();
        let env = env_of(&[
            ("LS_LIBJVM", "/custom/libjvm.so"),
            ("JAVA_HOME", "/jdk"),
            ("PC_HOST_AGENT_JAR", "/a.jar"),
        ]);
        let (libjvm, _) = resolve_island_paths(ws.path(), &env).unwrap();
        assert_eq!(libjvm, PathBuf::from("/config/jdk/lib/server/libjvm.so"));
    }

    // The nixpkgs JDK package-root layout: `JAVA_HOME=<store path>` where the
    // real home (release file + libjvm) is nested at `<store path>/lib/openjdk`.
    // A bare package-root JAVA_HOME must still locate libjvm (before this
    // normalization only LS_LIBJVM saved the dev shell).
    #[test]
    fn island_paths_normalize_a_nixpkgs_package_root_java_home() {
        let jdk = tempfile::tempdir().unwrap();
        let nested = jdk.path().join("lib/openjdk");
        std::fs::create_dir_all(nested.join("lib/server")).unwrap();
        std::fs::write(nested.join("release"), "JAVA_VERSION=\"25.0.4\"\n").unwrap();
        let ws = tempfile::tempdir().unwrap();
        let env = env_of(&[
            ("JAVA_HOME", jdk.path().to_str().unwrap()),
            ("PC_HOST_AGENT_JAR", "/a.jar"),
        ]);
        let (libjvm, _) = resolve_island_paths(ws.path(), &env).unwrap();
        assert_eq!(libjvm, nested.join("lib/server/libjvm.so"));
    }

    #[test]
    fn island_paths_ignore_an_unparseable_workspace_config() {
        let ws = tempfile::tempdir().unwrap();
        let conf_dir = ws.path().join(".scala3-bsp-semantic-ls");
        std::fs::create_dir_all(&conf_dir).unwrap();
        std::fs::write(conf_dir.join("config.json"), "{ not json").unwrap();
        let env = env_of(&[("JAVA_HOME", "/jdk"), ("PC_HOST_AGENT_JAR", "/a.jar")]);
        let (libjvm, _) = resolve_island_paths(ws.path(), &env).unwrap();
        assert_eq!(libjvm, PathBuf::from("/jdk/lib/server/libjvm.so"));
    }

    #[test]
    fn island_paths_without_any_java_home_tier_are_a_typed_error() {
        let ws = tempfile::tempdir().unwrap();
        let env = env_of(&[("PC_HOST_AGENT_JAR", "/a.jar")]);
        let err = resolve_island_paths(ws.path(), &env).unwrap_err();
        assert!(err.contains("javaHome"), "{err}");
        assert!(err.contains("JAVA_HOME"), "{err}");
    }

    #[test]
    fn island_paths_without_the_agent_jar_are_a_typed_error() {
        let ws = tempfile::tempdir().unwrap();
        let env = env_of(&[("JAVA_HOME", "/jdk")]);
        let err = resolve_island_paths(ws.path(), &env).unwrap_err();
        assert!(err.contains("PC_HOST_AGENT_JAR"), "{err}");
    }

    // Ports Bootstrap.pcOptions: strips the single-token generation flags, both
    // forms of the two-token flags, and keeps everything else in order.
    #[test]
    fn pc_options_strips_semanticdb_flags_in_every_form() {
        let options = vec![
            "-deprecation".to_string(),
            "-Xsemanticdb".to_string(),
            "-Ysemanticdb".to_string(),
            "-semanticdb-target".to_string(),
            "/out/meta".to_string(),
            "-sourceroot".to_string(),
            "/ws".to_string(),
            "-semanticdb-target:/out/meta2".to_string(),
            "-sourceroot:/ws2".to_string(),
            "-feature".to_string(),
        ];
        assert_eq!(
            pc_options(&options),
            vec!["-deprecation".to_string(), "-feature".to_string()]
        );
    }

    #[test]
    fn pc_options_keeps_a_two_token_flag_with_no_value_token() {
        // A trailing two-token flag with no following value is not treated as a
        // value-skip (mirrors the `i + 1 < length` guard); it is kept as-is.
        let options = vec!["-deprecation".to_string(), "-sourceroot".to_string()];
        assert_eq!(pc_options(&options), options);
    }

    #[test]
    fn pc_options_is_identity_without_semanticdb_flags() {
        let options = vec!["-deprecation".to_string(), "-explain".to_string()];
        assert_eq!(pc_options(&options), options);
    }

    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    fn empty_resolver() -> Box<SymbolResolver> {
        Box::new(|_symbol, _from_uri| LocationsResult {
            locations: Vec::new(),
        })
    }

    fn empty_search_resolver() -> Box<SearchMethodsResolver> {
        Box::new(|_query, _target| MethodHitsResult { hits: Vec::new() })
    }

    fn empty_toplevels_resolver() -> Box<ToplevelsResolver> {
        Box::new(|_symbol, _uri| ToplevelsResult {
            symbols: Vec::new(),
        })
    }

    fn target_config(id: &str) -> TargetConfig {
        TargetConfig {
            bsp_id: id.to_string(),
            scala_version: "3".to_string(),
            classpath: Vec::new(),
            scalac_options: Vec::new(),
            source_dirs: Vec::new(),
        }
    }

    /// A fake booted-island driver that returns queued lifecycle outcomes (and
    /// queued control-lane plugin-status replies) and counts the forwards it
    /// received, so the status-aware mirror discipline is exercised without a
    /// live JVM.
    struct FakeDriver {
        outcomes: VecDeque<Result<Vec<u8>, PcError>>,
        status: VecDeque<Result<Vec<u8>, i32>>,
        calls: Arc<AtomicUsize>,
    }

    impl FakeDriver {
        fn new(outcomes: Vec<Result<Vec<u8>, PcError>>) -> (FakeDriver, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            (
                FakeDriver {
                    outcomes: outcomes.into(),
                    status: VecDeque::new(),
                    calls: calls.clone(),
                },
                calls,
            )
        }

        /// Queue control-lane plugin-status replies (raw encoded bytes or a
        /// vtable status).
        fn with_status(mut self, status: Vec<Result<Vec<u8>, i32>>) -> FakeDriver {
            self.status = status.into();
            self
        }
    }

    impl PcDriver for FakeDriver {
        fn request(&mut self, _request: PcRequest) -> Result<Vec<u8>, PcError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.outcomes.pop_front().unwrap_or(Ok(Vec::new()))
        }

        fn plugin_status(&mut self) -> Result<Vec<u8>, i32> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.status.pop_front().unwrap_or(Err(-1))
        }
    }

    /// A service with `target` registered and a fake driver installed (as if the
    /// island had already booted), returning the queued lifecycle outcomes.
    fn booted(
        target: &str,
        outcomes: Vec<Result<Vec<u8>, PcError>>,
    ) -> (IslandPcService, Arc<AtomicUsize>) {
        let pc = IslandPcService::new(
            PathBuf::from("/ws"),
            vec![target_config(target)],
            empty_resolver(),
            empty_search_resolver(),
            empty_toplevels_resolver(),
        );
        let (driver, calls) = FakeDriver::new(outcomes);
        pc.state.lock().unwrap().driver = Some(Box::new(driver));
        (pc, calls)
    }

    /// Like [`booted`], with queued control-lane plugin-status replies.
    fn booted_with_status(
        target: &str,
        status: Vec<Result<Vec<u8>, i32>>,
    ) -> (IslandPcService, Arc<AtomicUsize>) {
        let pc = IslandPcService::new(
            PathBuf::from("/ws"),
            vec![target_config(target)],
            empty_resolver(),
            empty_search_resolver(),
            empty_toplevels_resolver(),
        );
        let (driver, calls) = FakeDriver::new(Vec::new());
        let driver = driver.with_status(status);
        pc.state.lock().unwrap().driver = Some(Box::new(driver));
        (pc, calls)
    }

    // The document notifications keep the open-buffer mirror in sync WITHOUT
    // booting the JVM island: an index-only session that only opens/edits/closes
    // buffers (never issuing a PC query) keeps a zero-JVM process. A change/close
    // for a buffer the mirror never held is a no-op, not a panic (replay-safe).
    #[test]
    fn document_notifications_mirror_without_booting_the_island() {
        let pc = IslandPcService::new(
            PathBuf::from("/ws"),
            vec![target_config("t")],
            empty_resolver(),
            empty_search_resolver(),
            empty_toplevels_resolver(),
        );
        let a = "file:///ws/a.scala";

        assert!(!pc.is_open(a));
        pc.did_open("t", a, "v1");
        assert!(pc.is_open(a));
        pc.did_change(a, "v2");
        assert_eq!(pc.buffer_text(a).as_deref(), Some("v2"));
        pc.did_close(a);
        assert!(!pc.is_open(a));

        // A change/close for a never-opened buffer is a harmless no-op.
        pc.did_change("file:///ws/ghost.scala", "x");
        pc.did_close("file:///ws/ghost.scala");
        assert!(!pc.is_open("file:///ws/ghost.scala"));

        assert!(
            !pc.is_booted(),
            "a document notification must never boot the embedded JVM island"
        );
    }

    // A build-target change reconfigures the registered set on the SAME island
    // (the Scala `reloadBuildModel` reuse of `s.pc`): the `is_registered` gate
    // reflects the new set, a still-cold island does not boot, and a booted island
    // (re-)registers the new targets into the running island.
    #[test]
    fn reconfigure_targets_updates_the_gate_and_registers_into_a_booted_island() {
        // Cold: the mirror (the gate + boot-replay source) updates without booting.
        let cold = IslandPcService::new(
            PathBuf::from("/ws"),
            vec![target_config("old")],
            empty_resolver(),
            empty_search_resolver(),
            empty_toplevels_resolver(),
        );
        cold.reconfigure_targets(vec![target_config("new")]);
        assert!(cold.is_registered("new"));
        assert!(!cold.is_registered("old"));
        assert!(!cold.is_booted(), "reconfigure must not boot the island");

        // Booted: the new targets are (re-)registered into the running island.
        let (booted_pc, calls) = booted("old", vec![Ok(Vec::new()), Ok(Vec::new())]);
        booted_pc.reconfigure_targets(vec![target_config("new"), target_config("old")]);
        assert!(booted_pc.is_registered("new"));
        assert_eq!(
            calls.load(Ordering::SeqCst),
            2,
            "both targets re-registered into the booted island"
        );
    }

    // `PcFacade.didOpen` requires the target registered before adding the buffer;
    // an open for an unregistered target is never mirrored (and never boots).
    #[test]
    fn did_open_for_an_unregistered_target_is_ignored() {
        let pc = IslandPcService::new(
            PathBuf::from("/ws"),
            vec![target_config("known")],
            empty_resolver(),
            empty_search_resolver(),
            empty_toplevels_resolver(),
        );
        pc.did_open("unknown", "file:///ws/a.scala", "v1");
        assert!(!pc.is_open("file:///ws/a.scala"));
        assert!(!pc.is_booted());
    }

    // A booted `did_open` whose live forward returns a clean backend status must
    // NOT mirror the buffer — the island rejected it, so `is_open`/`withPcBuffer`
    // must not claim it. The island is forwarded to first.
    #[test]
    fn failed_live_did_open_leaves_the_buffer_unmirrored() {
        let (pc, calls) = booted("t", vec![Err(PcError::Backend(-7))]);
        let a = "file:///ws/a.scala";
        pc.did_open("t", a, "v1");
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "the island must be forwarded the open first"
        );
        assert!(
            !pc.is_open(a),
            "a rejected didOpen must not mark the buffer open"
        );
    }

    // A booted `did_open` the island accepts mirrors the buffer; a timeout
    // recovery also mirrors (the editor's notification is a fact the replay honors).
    #[test]
    fn accepted_and_timeout_live_did_open_mirror_the_buffer() {
        let (pc, _calls) = booted("t", vec![Ok(Vec::new())]);
        pc.did_open("t", "file:///ws/ok.scala", "v1");
        assert_eq!(pc.buffer_text("file:///ws/ok.scala").as_deref(), Some("v1"));

        let (pc, _calls) = booted("t", vec![Err(PcError::RequestTimeout)]);
        pc.did_open("t", "file:///ws/recovered.scala", "v1");
        assert!(pc.is_open("file:///ws/recovered.scala"));
    }

    // A booted `did_change` the island rejects preserves the last accepted text,
    // so a later boot replay feeds the island what it actually holds.
    #[test]
    fn failed_live_did_change_preserves_the_replayable_text() {
        let (pc, _calls) = booted("t", vec![Ok(Vec::new()), Err(PcError::Backend(-3))]);
        let a = "file:///ws/a.scala";
        pc.did_open("t", a, "v1");
        pc.did_change(a, "v2");
        assert_eq!(
            pc.buffer_text(a).as_deref(),
            Some("v1"),
            "a rejected didChange must keep the last accepted text"
        );
    }

    // A booted `did_close` the island rejects keeps the buffer (the island still
    // holds it), avoiding an outer/inner replay divergence.
    #[test]
    fn failed_live_did_close_keeps_the_buffer() {
        let (pc, _calls) = booted("t", vec![Ok(Vec::new()), Err(PcError::Backend(-9))]);
        let a = "file:///ws/a.scala";
        pc.did_open("t", a, "v1");
        pc.did_close(a);
        assert!(
            pc.is_open(a),
            "a rejected didClose must keep the buffer to avoid a replay divergence"
        );
    }

    // A backend status on a boot replay (the target registers, then a replayed
    // DidOpen is rejected) fails the boot: the driver is NOT installed and the
    // failure is recorded, so a half-populated island is never served.
    #[test]
    fn failed_boot_replay_does_not_install_a_driver() {
        let pc = IslandPcService::new(
            PathBuf::from("/ws"),
            vec![target_config("t")],
            empty_resolver(),
            empty_search_resolver(),
            empty_toplevels_resolver(),
        );
        // A buffer is pending replay (opened while cold).
        pc.did_open("t", "file:///ws/a.scala", "v1");
        // RegisterTarget succeeds; the replayed DidOpen is rejected.
        let (driver, _calls) = FakeDriver::new(vec![Ok(Vec::new()), Err(PcError::Backend(-4))]);
        let installed = {
            let mut guard = pc.state.lock().unwrap();
            install_driver(&mut guard, Box::new(driver))
        };
        assert!(
            !installed,
            "a rejected boot replay must not install a driver"
        );
        assert!(
            !pc.is_booted(),
            "no driver is installed after a failed replay"
        );
        assert!(
            pc.state.lock().unwrap().boot_error.is_some(),
            "the boot failure is recorded"
        );
    }

    // A boot that registers the target and replays the buffer cleanly installs the
    // driver.
    #[test]
    fn clean_boot_replay_installs_the_driver() {
        let pc = IslandPcService::new(
            PathBuf::from("/ws"),
            vec![target_config("t")],
            empty_resolver(),
            empty_search_resolver(),
            empty_toplevels_resolver(),
        );
        pc.did_open("t", "file:///ws/a.scala", "v1");
        let (driver, _calls) = FakeDriver::new(vec![Ok(Vec::new()), Ok(Vec::new())]);
        let installed = {
            let mut guard = pc.state.lock().unwrap();
            install_driver(&mut guard, Box::new(driver))
        };
        assert!(installed);
        assert!(pc.is_booted());
    }

    // A booted query dispatches `PcRequest::Query` through the driver and decodes
    // the `DefinitionResult` reply to LSP-coordinate locations; a backend error on
    // the query degrades to an empty result (no panic across the seam).
    #[test]
    fn a_booted_query_dispatches_through_the_driver_and_decodes_locations() {
        use ls_pc_abi::payloads::{origin, Location, Rng};

        let a = "file:///ws/a.scala";
        let reply = DefinitionResult {
            symbol: "foo".to_string(),
            locations: vec![Location {
                uri: a.to_string(),
                range: Rng {
                    start_line: 1,
                    start_character: 2,
                    end_line: 1,
                    end_character: 5,
                },
                origin: origin::WORKSPACE,
            }],
        }
        .encode()
        .expect("encode definition result");

        // The first Ok is consumed by `did_open` (mirrors the buffer); the query
        // consumes the encoded reply.
        let (pc, _calls) = booted("t", vec![Ok(Vec::new()), Ok(reply)]);
        pc.did_open("t", a, "object A");
        assert_eq!(
            pc.definition(a, 0, 0),
            vec![PcLocation {
                uri: a.to_string(),
                start_line: 1,
                start_character: 2,
                end_line: 1,
                end_character: 5,
            }]
        );

        // A backend error on the query degrades to empty.
        let (pc, _calls) = booted("t", vec![Ok(Vec::new()), Err(PcError::Backend(-1))]);
        pc.did_open("t", a, "object A");
        assert!(pc.definition(a, 0, 0).is_empty());
    }

    // A booted completion decodes the `CompletionList` carrier and renders it as
    // LSP JSON; a backend error degrades to an empty, complete completion list.
    #[test]
    fn a_booted_completion_decodes_the_list_and_degrades_to_empty() {
        let a = "file:///ws/a.scala";
        let reply = CompletionList {
            is_incomplete: false,
            item_defaults: None,
            apply_kind: None,
            items: vec![ls_pc_abi::payloads::CompletionItem {
                label: "foo".to_string(),
                label_details: None,
                kind: Some(2),
                tags: None,
                detail: None,
                documentation: None,
                deprecated: None,
                preselect: None,
                sort_text: None,
                filter_text: None,
                insert_text: None,
                insert_text_format: None,
                insert_text_mode: None,
                text_edit: None,
                text_edit_text: None,
                additional_text_edits: None,
                commit_characters: None,
                command: None,
                data: None,
            }],
        }
        .encode()
        .expect("encode completion list");

        // did_open consumes the first Ok; the completion query consumes the list.
        let (pc, _calls) = booted("t", vec![Ok(Vec::new()), Ok(reply)]);
        pc.did_open("t", a, "object A");
        let value = pc.completion(a, 0, 0);
        assert_eq!(value["isIncomplete"], false);
        assert_eq!(value["items"][0]["label"], "foo");
        assert_eq!(value["items"][0]["kind"], 2);

        // A backend error on the query degrades to an empty, complete list.
        let (pc, _calls) = booted("t", vec![Ok(Vec::new()), Err(PcError::Backend(-1))]);
        pc.did_open("t", a, "object A");
        assert_eq!(
            pc.completion(a, 0, 0),
            crate::pc_convert::empty_completions()
        );
    }

    // A booted payload-query op dispatches through the driver and decodes its
    // typed carrier; a backend error — including the island's STATUS_NOT_YET
    // transport-stub answer — degrades to the empty fallback (no panic, no
    // error surfaced across the seam).
    #[test]
    fn booted_payload_queries_decode_their_carriers_and_not_yet_degrades_to_empty() {
        use ls_pc_abi::payloads::{
            AutoImport, FoldingRange, InlayHint, InlayLabelPart, PcDiagnostic, SemanticNode,
            TextEdit,
        };

        let a = "file:///ws/a.scala";
        let rng = Rng {
            start_line: 0,
            start_character: 0,
            end_line: 9,
            end_character: 0,
        };
        let pos = Pos {
            line: 1,
            character: 2,
        };

        let hints = InlayHintsResult {
            hints: vec![InlayHint {
                position: pos,
                label_parts: vec![InlayLabelPart {
                    text: ": Int".to_string(),
                    location: Some((a.to_string(), rng.clone())),
                    tooltip: None,
                }],
                kind: 1,
                padding_left: true,
                padding_right: false,
                text_edits: None,
                data: Some(vec![1, 2]),
            }],
        };
        let (pc, _calls) = booted("t", vec![Ok(hints.encode().unwrap())]);
        assert_eq!(pc.inlay_hints(a, rng.clone(), 3), hints.hints);

        let tokens = SemanticTokensResult {
            nodes: vec![SemanticNode {
                start: 0,
                end: 6,
                token_type: 3,
                token_modifier: 1,
            }],
        };
        let (pc, _calls) = booted("t", vec![Ok(tokens.encode().unwrap())]);
        assert_eq!(pc.semantic_tokens(a), tokens.nodes);

        let chains = SelectionRangesResult {
            chains: vec![vec![rng.clone()], vec![]],
        };
        let (pc, _calls) = booted("t", vec![Ok(chains.encode().unwrap())]);
        assert_eq!(
            pc.selection_range(
                a,
                &[
                    pos,
                    Pos {
                        line: 3,
                        character: 4
                    }
                ]
            ),
            chains.chains
        );

        let action = CodeActionResult {
            edits: vec![TextEdit {
                range: rng.clone(),
                new_text: ": Int".to_string(),
            }],
            refusal: None,
        };
        let (pc, _calls) = booted("t", vec![Ok(action.encode().unwrap())]);
        assert_eq!(pc.code_action(a, 4, pos, None, None), action);

        // A typed refusal is data on the decoded result, not an error.
        let refused = CodeActionResult {
            edits: vec![],
            refusal: Some("Cannot extract selection".to_string()),
        };
        let (pc, _calls) = booted("t", vec![Ok(refused.encode().unwrap())]);
        assert_eq!(
            pc.code_action(
                a,
                2,
                pos,
                Some(Pos {
                    line: 7,
                    character: 0
                }),
                None
            )
            .refusal
            .as_deref(),
            Some("Cannot extract selection")
        );

        let imports = AutoImportsResult {
            imports: vec![AutoImport {
                package_name: "scala.concurrent".to_string(),
                edits: vec![],
                symbol: None,
            }],
        };
        let (pc, _calls) = booted("t", vec![Ok(imports.encode().unwrap())]);
        assert_eq!(pc.auto_imports(a, pos, "Future", false), imports.imports);

        let diags = PcDiagnosticsResult {
            diagnostics: vec![PcDiagnostic {
                range: rng.clone(),
                severity: 1,
                code: "E007".to_string(),
                message: "not found".to_string(),
            }],
        };
        let (pc, _calls) = booted("t", vec![Ok(diags.encode().unwrap())]);
        assert_eq!(pc.pc_diagnostics(a), diags.diagnostics);

        let folds = FoldingRangesResult {
            ranges: vec![FoldingRange {
                range: rng.clone(),
                kind: 2,
            }],
        };
        let (pc, _calls) = booted("t", vec![Ok(folds.encode().unwrap())]);
        assert_eq!(pc.folding_range(a), folds.ranges);

        // The island's NOT_YET transport stub degrades every op to its empty
        // fallback, exactly like any other backend status.
        let not_yet = || Err(PcError::Backend(ls_pc_abi::STATUS_NOT_YET));
        let (pc, _calls) = booted(
            "t",
            vec![
                not_yet(),
                not_yet(),
                not_yet(),
                not_yet(),
                not_yet(),
                not_yet(),
                not_yet(),
            ],
        );
        assert!(pc.inlay_hints(a, rng.clone(), 0).is_empty());
        assert!(pc.semantic_tokens(a).is_empty());
        assert!(pc.selection_range(a, &[pos]).is_empty());
        assert_eq!(
            pc.code_action(a, 0, pos, None, None),
            CodeActionResult::default()
        );
        assert!(pc.auto_imports(a, pos, "X", true).is_empty());
        assert!(pc.pc_diagnostics(a).is_empty());
        assert!(pc.folding_range(a).is_empty());
    }

    // A cold service must not boot the island for a payload query either: with
    // no JAVA_HOME-style environment the boot fails typed, the query degrades
    // to empty, and no driver is installed.
    #[test]
    fn a_payload_query_on_an_unbootable_service_degrades_to_empty() {
        let pc = IslandPcService::new(
            PathBuf::from("/nonexistent-ws"),
            vec![target_config("t")],
            empty_resolver(),
            empty_search_resolver(),
            empty_toplevels_resolver(),
        );
        // Latch a boot error so the dispatch path degrades without touching the
        // real environment.
        pc.state.lock().unwrap().boot_error = Some("no javaHome".to_string());
        assert!(pc.semantic_tokens("file:///ws/a.scala").is_empty());
        assert!(!pc.is_booted());
    }

    // `workspace/didChangeConfiguration` un-latches a recorded boot failure ONLY
    // while the island is still cold, so the next PC query re-reads config.json
    // and re-attempts the boot; a booted island ignores the notification (the
    // one-per-process JVM cannot re-home). The settings payload never reaches
    // this hook — config.json stays the single configuration source.
    #[test]
    fn config_change_clears_a_latched_boot_error_only_while_cold() {
        // Cold: the latch clears (and the island is still not booted).
        let cold = IslandPcService::new(
            PathBuf::from("/ws"),
            vec![target_config("t")],
            empty_resolver(),
            empty_search_resolver(),
            empty_toplevels_resolver(),
        );
        cold.state.lock().unwrap().boot_error = Some("bad javaHome".to_string());
        cold.on_config_changed();
        assert!(
            cold.state.lock().unwrap().boot_error.is_none(),
            "a cold island's latched boot error must clear on a config change"
        );
        assert!(
            !cold.is_booted(),
            "a config change must not boot the island"
        );

        // Booted: the notification is ignored — the latch survives and no
        // request reaches the island.
        let (booted_pc, calls) = booted("t", Vec::new());
        booted_pc.state.lock().unwrap().boot_error = Some("re-registration rejected".to_string());
        booted_pc.on_config_changed();
        assert!(
            booted_pc.state.lock().unwrap().boot_error.is_some(),
            "a booted island must ignore a config change"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 0, "no island traffic");
    }

    // `is_registered` reflects the registered PC targets — the resolve gate.
    #[test]
    fn is_registered_reflects_the_registered_targets() {
        let (pc, _calls) = booted("t", Vec::new());
        assert!(pc.is_registered("t"));
        assert!(!pc.is_registered("other"));
    }

    // A booted resolve encodes the echoed item, dispatches `PcRequest::Resolve`,
    // and renders the enriched reply; a backend error degrades to the original item.
    #[test]
    fn a_booted_resolve_dispatches_and_renders_the_enriched_item() {
        let enriched = CompletionItem {
            label: "foo".to_string(),
            label_details: None,
            kind: Some(2),
            tags: None,
            detail: Some("def foo: Int".to_string()),
            documentation: None,
            deprecated: None,
            preselect: None,
            sort_text: None,
            filter_text: None,
            insert_text: None,
            insert_text_format: None,
            insert_text_mode: None,
            text_edit: None,
            text_edit_text: None,
            additional_text_edits: None,
            commit_characters: None,
            command: None,
            data: None,
        }
        .encode()
        .expect("encode enriched item");
        let item = serde_json::json!({ "label": "foo", "data": { "symbol": "s" } });

        // The resolve dispatch consumes the single queued reply (no did_open needed).
        let (pc, _calls) = booted("t", vec![Ok(enriched)]);
        let out = pc.resolve_completion_item("t", "s", &item);
        assert_eq!(out["label"], "foo");
        assert_eq!(out["detail"], "def foo: Int");

        // A backend error degrades to the original item, unchanged.
        let (pc, _calls) = booted("t", vec![Err(PcError::Backend(-1))]);
        assert_eq!(pc.resolve_completion_item("t", "s", &item), item);
    }

    // THE pre-boot invariant: a plugin-status inspection of a still-cold island
    // answers `None` and never takes the boot path — only a PC query boots.
    #[test]
    fn plugin_status_on_a_cold_service_is_none_and_never_boots() {
        let pc = IslandPcService::new(
            PathBuf::from("/ws"),
            vec![target_config("t")],
            empty_resolver(),
            empty_search_resolver(),
            empty_toplevels_resolver(),
        );
        assert_eq!(pc.plugin_status(), None);
        assert!(
            !pc.is_booted(),
            "a plugin-status inspection must never boot the island"
        );
    }

    // A booted plugin-status fetch decodes the control-lane `PluginStatus`
    // carrier into the seam report, field for field.
    #[test]
    fn a_booted_plugin_status_decodes_the_seam_report() {
        let reply = PluginStatus {
            compiler_plugins: vec![ls_pc_abi::payloads::CompilerPlugin {
                jars: vec!["/plugins/zaozi.jar".to_string()],
                options: vec!["-P:zaozi:on".to_string()],
                loaded: true,
                detail: "ok".to_string(),
            }],
            service_plugins: vec![ls_pc_abi::payloads::ServicePlugin {
                id: "zaozi.nav".to_string(),
                source: "workspace pc-plugins.json".to_string(),
                enabled: true,
                self_test_ok: false,
                self_test_detail: "self-test failed: boom".to_string(),
            }],
            disabled: vec![ls_pc_abi::payloads::DisabledPlugin {
                id: "old.plugin".to_string(),
                reason: "disabled by config".to_string(),
            }],
        }
        .encode()
        .expect("encode plugin status");

        let (pc, _calls) = booted_with_status("t", vec![Ok(reply)]);
        assert_eq!(
            pc.plugin_status(),
            Some(PcPluginStatusReport {
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
                    self_test_ok: false,
                    self_test_detail: "self-test failed: boom".to_string(),
                }],
                disabled: vec![PcDisabledPlugin {
                    id: "old.plugin".to_string(),
                    reason: "disabled by config".to_string(),
                }],
            })
        );
    }

    // A control-lane error (vtable status) and an undecodable reply both degrade
    // to `None` — no panic across the seam, the caller renders its cold/typed
    // wording.
    #[test]
    fn a_failed_plugin_status_degrades_to_none() {
        let (pc, _calls) = booted_with_status("t", vec![Err(-7)]);
        assert_eq!(pc.plugin_status(), None);

        let (pc, _calls) = booted_with_status("t", vec![Ok(vec![0xde, 0xad])]);
        assert_eq!(pc.plugin_status(), None);
    }
}
