//! The production ready-services bundle and the ready-path request handlers
//! wired over the Rust engine.
//!
//! [`CoreServices`] is the `S` the ready state owns (the Scala `CoreServices`
//! equivalent): the query orchestrator, the workspace URI mapping, and the
//! workspace root. [`CoreHandlers`] is the production [`Handlers`] impl. It
//! wires the index-backed, PC-free query methods — `references` and
//! `documentHighlight` — over the engine's [`ReferencesEngine`] /
//! [`DocumentHighlightService`], converting SemanticDB coordinates and URIs to
//! the LSP result shapes. The remaining ready methods (the PC-backed queries,
//! rename, `workspace/symbol`, and the `executeCommand` actions) attach as their
//! subsystems are wired; until then they answer a typed placeholder error.

use std::path::PathBuf;

use serde_json::{json, Value};

use ls_engine::{
    DocHighlight, DocumentHighlightService, QueryOrchestrator, ReferencesEngine, ReferencesResult,
};
use ls_index_model::uri::normalize_uri;
use ls_index_model::LsError;

use crate::convert::{self, DocumentHighlight, Location};
use crate::jsonrpc::{error_codes, RequestId, Response, ResponseError};
use crate::protocol::Position;
use crate::server::{Handlers, RequestContext};
use crate::workspace_uris::WorkspaceUris;

/// The ready-services bundle owned by [`WorkspaceState::Ready`](crate::WorkspaceState::Ready):
/// the query orchestrator, the workspace URI mapping, and the workspace root.
pub struct CoreServices {
    pub orchestrator: QueryOrchestrator,
    pub uris: WorkspaceUris,
    pub workspace_root: Option<PathBuf>,
}

impl CoreServices {
    pub fn new(
        orchestrator: QueryOrchestrator,
        uris: WorkspaceUris,
        workspace_root: Option<PathBuf>,
    ) -> CoreServices {
        CoreServices {
            orchestrator,
            uris,
            workspace_root,
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
/// within the one document, from the index doc-postings. Every failure (an
/// unmappable URI, a missing position, or an engine error) collapses to an empty
/// list, matching `ScalaLs.documentHighlight`'s catch-all.
fn document_highlight(id: RequestId, services: &CoreServices, params: &Value) -> Response {
    let Some(raw) = text_document_uri(params) else {
        return Response::success(id, json!([]));
    };
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

    use ls_engine::{DocHighlight, HighlightKind, QueryOrchestrator, ReferenceHit};
    use ls_index_model::uri::path_to_uri;
    use ls_index_model::{Loc, Role, Span};
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
        CoreServices::new(orchestrator, uris, Some(root.to_path_buf()))
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

    #[test]
    fn document_highlight_over_an_unindexed_workspace_is_an_empty_list() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("A.scala"), "object A").unwrap();
        let services = unindexed_services(dir.path());
        let response = document_highlight(
            RequestId::Number(1),
            &services,
            &position_params(dir.path()),
        );
        let value = serde_json::to_value(&response).unwrap();
        assert_eq!(value["result"], json!([]));
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
}
