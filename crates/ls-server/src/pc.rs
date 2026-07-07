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
use ls_jvm::watchdog::{PcError, PcRequest, QueryKind, Supervisor};
use ls_jvm::{boot_island, install_symbol_definition_resolver, IslandConfig};
use ls_pc_abi::payloads::{
    origin, CompletionItem, CompletionList, DefinitionResult, HoverResult, LocationsResult,
    PrepareRenameResult, Rng, SignatureHelp, TargetConfig,
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
}

/// The `symbol_definition` resolver the island calls when the presentation
/// compiler has no in-buffer source position for a cross-file symbol. Answers
/// from the global index (`QueryOrchestrator::symbol_definition`).
pub type SymbolResolver = dyn Fn(&str, &str) -> LocationsResult + Send + Sync;

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

/// The booted-island dispatch seam: the one operation the outer mirror needs
/// from the `ls-jvm` [`Supervisor`]. A trait so the status-aware mirror logic is
/// testable with a fake that returns `Ok`/`RequestTimeout`/`Backend` outcomes
/// without a live JVM.
trait PcDriver: Send {
    fn request(&mut self, request: PcRequest) -> Result<Vec<u8>, PcError>;
}

impl PcDriver for Supervisor<VtableBackend> {
    fn request(&mut self, request: PcRequest) -> Result<Vec<u8>, PcError> {
        Supervisor::request(self, request)
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
/// target registrations and the index-backed `symbol_definition` resolver, but
/// the JVM is not started until the first PC request.
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
    /// `symbol_definition` resolver. Does not boot the JVM.
    pub fn new(
        workspace_root: PathBuf,
        targets: Vec<TargetConfig>,
        resolver: Box<SymbolResolver>,
    ) -> IslandPcService {
        IslandPcService {
            state: Mutex::new(IslandState {
                workspace_root,
                targets,
                resolver: Some(resolver),
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
}

/// Boots the island: installs the resolver (once), reads the JVM environment,
/// boots, registers the targets, and replays the mirrored buffers. Records a
/// boot failure so a broken environment does not re-attempt per request.
/// Returns whether the supervisor is now available.
fn boot(state: &mut IslandState) -> bool {
    if state.boot_error.is_some() {
        return false;
    }
    let (Some(libjvm), Some(agent_jar)) = (
        std::env::var_os("LS_LIBJVM").map(PathBuf::from),
        std::env::var_os("PC_HOST_AGENT_JAR").map(PathBuf::from),
    ) else {
        state.boot_error =
            Some("LS_LIBJVM and PC_HOST_AGENT_JAR must be set to boot the PC island".to_string());
        return false;
    };
    // The resolver slot is global and set-once; a second install (e.g. a second
    // workspace in the process) is ignored, which is correct — one server, one
    // process. Installed before boot so the premain sees it.
    if let Some(resolver) = state.resolver.take() {
        install_symbol_definition_resolver(resolver);
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

    fn target_config(id: &str) -> TargetConfig {
        TargetConfig {
            bsp_id: id.to_string(),
            scala_version: "3".to_string(),
            classpath: Vec::new(),
            scalac_options: Vec::new(),
            source_dirs: Vec::new(),
        }
    }

    /// A fake booted-island driver that returns queued lifecycle outcomes and
    /// counts the forwards it received, so the status-aware mirror discipline is
    /// exercised without a live JVM.
    struct FakeDriver {
        outcomes: VecDeque<Result<Vec<u8>, PcError>>,
        calls: Arc<AtomicUsize>,
    }

    impl FakeDriver {
        fn new(outcomes: Vec<Result<Vec<u8>, PcError>>) -> (FakeDriver, Arc<AtomicUsize>) {
            let calls = Arc::new(AtomicUsize::new(0));
            (
                FakeDriver {
                    outcomes: outcomes.into(),
                    calls: calls.clone(),
                },
                calls,
            )
        }
    }

    impl PcDriver for FakeDriver {
        fn request(&mut self, _request: PcRequest) -> Result<Vec<u8>, PcError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.outcomes.pop_front().unwrap_or(Ok(Vec::new()))
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
        );
        let (driver, calls) = FakeDriver::new(outcomes);
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
}
