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
//! ladder), and the `scala3SemanticLs.reindex` and `scala3SemanticLs.compile`
//! executeCommand actions — over the engine's [`ReferencesEngine`] /
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
//! fallback).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use serde_json::{json, Value};

use ls_bsp::model::BspProjectModel;
use ls_engine::{
    CompileOutcome, CompileService, DocHighlight, DocumentHighlightService, IngestReport,
    QueryOrchestrator, ReferencesEngine, ReferencesResult, RenameEngine, WorkspaceSymbolEntry,
};
use ls_index_model::uri::normalize_uri;
use ls_index_model::{LsError, Span};

use crate::capabilities::commands;
use crate::convert::{self, DocumentHighlight, Location, WorkspaceSymbol};
use crate::jsonrpc::{error_codes, RequestId, Response, ResponseError};
use crate::pc::{PcLocation, PcQueryService};
use crate::protocol::Position;
use crate::server::{Handlers, RequestContext};
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
    /// injections get the disconnected stub.
    pub compiler: Box<dyn BuildCompiler>,
    /// The presentation-compiler query capability (the Scala `CoreServices.pc`):
    /// the definition-family PC methods run through it. In production it lazily
    /// boots the embedded JVM island on the first PC request.
    pub pc: Box<dyn PcQueryService>,
    /// Whether a live BSP session backs this workspace (the Scala `s.session`
    /// being non-empty). `false` only in the no-BSP recovered-index warm-restart
    /// mode; it gates the `require_semanticdb` persisted-index fallback so that
    /// fallback applies exclusively when no live model is authoritative.
    pub bsp_connected: bool,
    /// The target that owned the most recent completion request's buffer (the
    /// Scala `lastCompletionTarget`). `completion` records it; `completionItem/
    /// resolve` reads it to name the PC target the enrichment runs against.
    last_completion_target: Mutex<Option<String>>,
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
        compiler: Box<dyn BuildCompiler>,
        pc: Box<dyn PcQueryService>,
        bsp_connected: bool,
    ) -> CoreServices {
        CoreServices {
            orchestrator,
            uris,
            workspace_root,
            uri_to_target,
            compiler,
            pc,
            bsp_connected,
            last_completion_target: Mutex::new(None),
        }
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
        // `result.needs_reindex` triggers a background reingest in the Scala
        // server; that build scheduler is wired with the reindex flow, not here.
        // The reference list itself is already complete.
        Ok(result) => ok_json(
            id,
            &references_locations(&result, |sdb| services.to_file_uri(sdb)),
        ),
        Err(error) => request_failed(id, &error),
    }
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
    let symbols: Vec<WorkspaceSymbol> = services
        .orchestrator
        .workspace_symbols(query, WORKSPACE_SYMBOL_LIMIT)
        .iter()
        .map(workspace_symbol_of)
        .collect();
    ok_json(id, &symbols)
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
/// compiles the indexable targets through the retained build compiler.
/// (`doctor` and unknown / pre-ready commands are handled before this point.)
fn execute_command(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    match params.get("command").and_then(Value::as_str) {
        Some(commands::REINDEX) => Response::success(id, Value::String(reindex(services))),
        Some(commands::COMPILE) => Response::success(id, Value::String(compile(services))),
        Some(other) => not_implemented(id, other),
        None => not_implemented(id, "workspace/executeCommand"),
    }
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
        let orchestrator = Arc::new(QueryOrchestrator::with_defaults(store));
        let uris = WorkspaceUris::new(&[root.to_path_buf()]);
        CoreServices::new(
            orchestrator,
            uris,
            Some(root.to_path_buf()),
            HashMap::new(),
            Box::new(UnavailableCompiler),
            Box::new(pc),
            true,
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
    #[test]
    fn an_unwired_ready_method_answers_a_typed_placeholder_error() {
        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path());
        let documents = DocumentStore::new();
        let request = Request {
            id: RequestId::Number(1),
            method: "textDocument/foldingRange".to_string(),
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
    // query methods too, so completion/hover/signatureHelp over a URI the model
    // does not own are hard NoSemanticdb errors, not the empty/null fallback.
    #[test]
    fn pc_query_methods_over_an_unowned_uri_are_hard_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("A.scala"), "object A").unwrap();
        let services = unindexed_services(dir.path());
        for handler in [completion, hover, signature_help] {
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
