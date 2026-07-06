//! The production ready-services bundle and the ready-path request handlers
//! wired over the Rust engine.
//!
//! [`CoreServices`] is the `S` the ready state owns (the Scala `CoreServices`
//! equivalent): the query orchestrator, the workspace URI mapping, the workspace
//! root, and the build model's URI ownership (`uri_to_target`, backing the
//! `requireSemanticdb` gate). [`CoreHandlers`] is the production [`Handlers`]
//! impl. It wires the index-backed, PC-free ready methods — `references`,
//! `documentHighlight`, `workspace/symbol`, `prepareRename`, and the
//! `scala3SemanticLs.reindex` executeCommand — over the engine's
//! [`ReferencesEngine`] / [`DocumentHighlightService`] / workspace-symbol
//! resolver / [`RenameEngine`], each gated (where the source applies it) by
//! `requireSemanticdb`, converting SemanticDB coordinates and URIs to the LSP
//! result shapes. The remaining ready methods (the PC-backed queries, full
//! rename, and the `compile`/`pcPluginStatus` executeCommand actions) attach as
//! their subsystems are wired; until then they answer a typed placeholder error.

use std::collections::HashMap;
use std::path::PathBuf;

use serde_json::{json, Value};

use ls_engine::{
    CompileOutcome, CompileService, DocHighlight, DocumentHighlightService, IngestReport,
    QueryOrchestrator, ReferencesEngine, ReferencesResult, RenameEngine, WorkspaceSymbolEntry,
};
use ls_index_model::uri::normalize_uri;
use ls_index_model::LsError;

use crate::capabilities::commands;
use crate::convert::{self, DocumentHighlight, Location, WorkspaceSymbol};
use crate::jsonrpc::{error_codes, RequestId, Response, ResponseError};
use crate::protocol::Position;
use crate::server::{Handlers, RequestContext};
use crate::workspace_uris::WorkspaceUris;

/// The ready-services bundle owned by [`WorkspaceState::Ready`](crate::WorkspaceState::Ready):
/// the query orchestrator, the workspace URI mapping, the workspace root, and the
/// live build model's URI ownership (`uri_to_target`).
pub struct CoreServices {
    pub orchestrator: QueryOrchestrator,
    pub uris: WorkspaceUris,
    pub workspace_root: Option<PathBuf>,
    /// Normalized `file://` URI -> owning bspId, from the build project model
    /// (`WorkspaceState`'s `uriToTarget`). Backs the `requireSemanticdb` gate.
    pub uri_to_target: HashMap<String, String>,
}

impl CoreServices {
    pub fn new(
        orchestrator: QueryOrchestrator,
        uris: WorkspaceUris,
        workspace_root: Option<PathBuf>,
        uri_to_target: HashMap<String, String>,
    ) -> CoreServices {
        CoreServices {
            orchestrator,
            uris,
            workspace_root,
            uri_to_target,
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

    /// SemanticDB is mandatory: a URI is serviceable only when the live model
    /// owns it via an *indexable* target (one that produces SemanticDB, hence
    /// present in the ingested workspace). Otherwise the gated methods answer
    /// `NoSemanticdb` rather than a stale/empty result. Ports the model-present
    /// branch of `ScalaLs.requireSemanticdb`; the persisted-index fallback (the
    /// no-BSP warm-restart recovery mode) attaches with that mode — a model is
    /// always present here, so it is authoritative.
    pub fn require_semanticdb(&self, raw_uri: &str) -> Result<(), LsError> {
        let uri = normalize_uri(raw_uri);
        let owned_by_indexable = self.uri_to_target.get(&uri).is_some_and(|bsp_id| {
            self.orchestrator
                .workspace()
                .is_some_and(|workspace| workspace.targets.iter().any(|t| &t.bsp_id == bsp_id))
        });
        if owned_by_indexable {
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
            "workspace/executeCommand" => execute_command(id, cx.services, &cx.request.params),
            other => not_implemented(id, other),
        }
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

/// `workspace/executeCommand` for the ready-path commands the message loop
/// routes here. `reindex` re-ingests over the retained workspace; `compile` and
/// `pcPluginStatus` need the live build compiler / PC island and stay a typed
/// placeholder. (`doctor` and unknown commands are handled before this point.)
fn execute_command(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    match params.get("command").and_then(Value::as_str) {
        Some(commands::REINDEX) => Response::success(id, Value::String(reindex(services))),
        Some(other) => not_implemented(id, other),
        None => not_implemented(id, "workspace/executeCommand"),
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

/// The compile hook used to build a [`RenameEngine`] for `prepareRename`, which
/// never compiles. The FreshRequired `rename` ladder supplies the live build
/// compiler instead; until then a compile request reports it is unavailable.
struct UnavailableCompiler;

impl CompileService for UnavailableCompiler {
    fn compile(&self, _targets: &[String]) -> CompileOutcome {
        CompileOutcome::Failed {
            reason: "no build server connected".to_string(),
        }
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
        let store = Store::open(root).unwrap();
        let orchestrator = QueryOrchestrator::with_defaults(store);
        let uris = WorkspaceUris::new(&[root.to_path_buf()]);
        CoreServices::new(orchestrator, uris, Some(root.to_path_buf()), HashMap::new())
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

    // A ready method that is not yet wired answers a typed error through the
    // production handler dispatch — never a silent empty result.
    #[test]
    fn an_unwired_ready_method_answers_a_typed_placeholder_error() {
        let dir = tempfile::tempdir().unwrap();
        let services = unindexed_services(dir.path());
        let documents = DocumentStore::new();
        let request = Request {
            id: RequestId::Number(1),
            method: "textDocument/hover".to_string(),
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
