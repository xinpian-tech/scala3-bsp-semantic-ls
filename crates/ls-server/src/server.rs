//! The stdio server loop and the workspace lifecycle state machine.
//!
//! [`serve`] reads framed JSON-RPC messages, answers `initialize` with the
//! capability surface (leaving the workspace [`WorkspaceState::NotReady`]), runs
//! bootstrap on `initialized` (transitioning to [`WorkspaceState::Ready`] or
//! [`WorkspaceState::Failed`]), keeps the document store in sync, serves the
//! per-method pre-ready fallbacks until the workspace is ready, and handles
//! `shutdown`/`exit`. A behavior-preserving port of the `ls.core.ScalaLs`
//! lifecycle.
//!
//! Two collaborators are injected seams, wired to the real subsystems in later
//! slices: [`Bootstrap`] (build-server discovery + JVM boot + ingest, producing
//! the ready state) and [`Handlers`] (the ready-path request handlers over the
//! engine/BSP/PC layers). This slice runs bootstrap inline on `initialized`; the
//! production server must run it off the message loop so pre-ready requests are
//! served while bootstrap is in flight.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use ls_index_model::uri::{normalize, normalize_uri, uri_to_path};

use crate::capabilities::{commands, initialize_result, InitializeResult};
use crate::documents::DocumentStore;
use crate::jsonrpc::{
    error_codes, parse_incoming, read_frame, write_frame, write_null_id_error, Incoming,
    Notification, Request, RequestId, Response, ResponseError,
};
use crate::lifecycle::{pre_ready_outcome, require_ready, Method, PreReadyOutcome, WorkspaceState};

/// The workspace bootstrap, run on `initialized`: it produces the next state
/// (ready or failed) for the given workspace root. The production impl discovers
/// the build server, boots the JVM, and ingests; tests inject a fixed outcome.
pub trait Bootstrap {
    fn run(&self, workspace_root: Option<&Path>) -> WorkspaceState;
}

/// The subsystem-backed request handlers, delegated to for the work that needs
/// the engine/BSP/PC layers: the ready-path query answers, `completionItem/
/// resolve` when ready, the ready-path `executeCommand` actions, and the doctor
/// report (which renders in any state). Wired to the real subsystems in a later
/// slice; the loop owns the pre-ready fallbacks that need no subsystem.
pub trait Handlers {
    fn handle(&self, request: &Request) -> Response;
}

/// The mutable server state driven by the message loop.
pub struct ServerCore {
    pub state: WorkspaceState,
    pub docs: DocumentStore,
    pub workspace_root: Option<PathBuf>,
    pub shutting_down: bool,
    initialized: bool,
}

impl ServerCore {
    pub fn new() -> ServerCore {
        ServerCore {
            state: WorkspaceState::NotReady {
                detail: "initialize has not run".to_string(),
            },
            docs: DocumentStore::new(),
            workspace_root: None,
            shutting_down: false,
            initialized: false,
        }
    }

    /// Handles `initialize`: records the workspace root and, unless the workspace
    /// is already ready, moves to `NotReady("waiting for the initialized
    /// notification")`. Returns the capability surface.
    pub fn initialize(&mut self, params: &Value) -> InitializeResult {
        self.workspace_root = root_from_params(params);
        self.initialized = true;
        if !self.state.is_ready() {
            self.state = WorkspaceState::NotReady {
                detail: "waiting for the initialized notification".to_string(),
            };
        }
        initialize_result()
    }

    /// Handles `initialized` by running bootstrap and adopting its outcome.
    pub fn run_bootstrap(&mut self, bootstrap: &impl Bootstrap) {
        self.state = bootstrap.run(self.workspace_root.as_deref());
    }

    /// Handles `shutdown`: idempotently marks the server shutting down and moves
    /// to `NotReady("server is shut down")`. Ready-service teardown lands with
    /// the services.
    pub fn shutdown(&mut self) {
        if !self.shutting_down {
            self.shutting_down = true;
            self.state = WorkspaceState::NotReady {
                detail: "server is shut down".to_string(),
            };
        }
    }

    fn did_open(&self, params: &Value) {
        if let (Some(uri), Some(text)) = (document_uri(params), document_text(params)) {
            self.docs.open(&uri, &text);
        }
    }

    fn did_change(&self, params: &Value) {
        // Full-text sync: the last content change carries the whole document.
        if let (Some(uri), Some(text)) = (document_uri(params), last_change_text(params)) {
            self.docs.change(&uri, &text);
        }
    }

    fn did_close(&self, params: &Value) {
        if let Some(uri) = document_uri(params) {
            self.docs.close(&uri);
        }
    }

    fn did_save(&self, params: &Value) {
        // A save that carries the text refreshes the open buffer so dirtiness
        // clears even when the editor folded the last edit into the save. The
        // reverse-dependency compile/reingest is a later slice.
        let (Some(uri), Some(text)) = (
            document_uri(params),
            params.get("text").and_then(Value::as_str),
        ) else {
            return;
        };
        if self.docs.is_open(&uri) {
            self.docs.change(&uri, text);
        }
    }
}

impl Default for ServerCore {
    fn default() -> Self {
        ServerCore::new()
    }
}

/// Controls the message loop after a notification.
enum Flow {
    Continue,
    Stop,
}

/// Runs the stdio server loop until `exit` or a clean end of input.
pub fn serve(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    core: &mut ServerCore,
    handlers: &impl Handlers,
    bootstrap: &impl Bootstrap,
) -> io::Result<()> {
    while let Some(body) = read_frame(reader)? {
        match parse_incoming(&body) {
            Ok(Incoming::Request(request)) => {
                let response = dispatch_request(core, handlers, request);
                write_frame(writer, &response)?;
            }
            Ok(Incoming::Notification(note)) => {
                if let Flow::Stop = dispatch_notification(core, bootstrap, note) {
                    break;
                }
            }
            Err(error) => write_null_id_error(writer, &error)?,
        }
    }
    Ok(())
}

fn dispatch_request(core: &mut ServerCore, handlers: &impl Handlers, request: Request) -> Response {
    // `request.id` is cloned rather than moved so the `request.method` borrow
    // taken by the match scrutinee stays valid across the arms; a request id is
    // a small integer or short string.
    match request.method.as_str() {
        "initialize" => {
            let result = core.initialize(&request.params);
            Response::success(
                request.id.clone(),
                serde_json::to_value(result).unwrap_or(Value::Null),
            )
        }
        "shutdown" => {
            core.shutdown();
            Response::success(request.id.clone(), Value::Null)
        }
        method if !core.initialized => Response::failure(
            request.id.clone(),
            ResponseError::new(
                error_codes::SERVER_NOT_INITIALIZED,
                format!("received {method} before initialize"),
            ),
        ),
        // Advertised as `resolveProvider`: resolve via the subsystems when ready,
        // otherwise echo the item back unchanged (the Scala `case _ => item`).
        "completionItem/resolve" => {
            if core.state.is_ready() {
                handlers.handle(&request)
            } else {
                Response::success(request.id.clone(), request.params.clone())
            }
        }
        "workspace/executeCommand" => execute_command(core, handlers, &request),
        method => match readiness_method(method) {
            Some(_) if core.state.is_ready() => handlers.handle(&request),
            Some(kind) => pre_ready_response(request.id.clone(), kind, &core.state),
            None => Response::failure(
                request.id.clone(),
                ResponseError::new(
                    error_codes::METHOD_NOT_FOUND,
                    format!("unhandled request: {method}"),
                ),
            ),
        },
    }
}

/// Dispatches `workspace/executeCommand` as ScalaLs does: the doctor report
/// renders in any state; reindex/compile/pcPluginStatus run through the
/// subsystems when ready and otherwise answer a typed "unavailable" status
/// string; an unknown command is an invalid-params error.
fn execute_command(core: &ServerCore, handlers: &impl Handlers, request: &Request) -> Response {
    let command = request
        .params
        .get("command")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let ready = core.state.is_ready();
    let unavailable = |action: &str| {
        Response::success(
            request.id.clone(),
            Value::String(format!(
                "{action} unavailable: workspace is {}",
                core.state.status_line()
            )),
        )
    };
    match command {
        commands::DOCTOR => handlers.handle(request),
        commands::REINDEX if ready => handlers.handle(request),
        commands::REINDEX => unavailable("reindex"),
        commands::COMPILE if ready => handlers.handle(request),
        commands::COMPILE => unavailable("compile"),
        commands::PC_PLUGIN_STATUS if ready => handlers.handle(request),
        commands::PC_PLUGIN_STATUS => unavailable("pc plugin status"),
        other => Response::failure(
            request.id.clone(),
            ResponseError::new(
                error_codes::INVALID_PARAMS,
                format!("unknown command '{other}'"),
            ),
        ),
    }
}

fn dispatch_notification(
    core: &mut ServerCore,
    bootstrap: &impl Bootstrap,
    note: Notification,
) -> Flow {
    match note.method.as_str() {
        "initialized" => core.run_bootstrap(bootstrap),
        "exit" => return Flow::Stop,
        "textDocument/didOpen" => core.did_open(&note.params),
        "textDocument/didChange" => core.did_change(&note.params),
        "textDocument/didClose" => core.did_close(&note.params),
        "textDocument/didSave" => core.did_save(&note.params),
        // Any other notification (including `$/setTrace`) is ignored.
        _ => {}
    }
    Flow::Continue
}

/// The pre-ready response for a readiness-sensitive request: the fixed per-method
/// fallback the server returns before the workspace is ready.
fn pre_ready_response(id: RequestId, method: Method, state: &WorkspaceState) -> Response {
    match pre_ready_outcome(method) {
        PreReadyOutcome::NotReadyError => {
            let message = require_ready(state)
                .expect_err("a not-ready state yields the typed error")
                .message;
            Response::failure(id, ResponseError::new(error_codes::REQUEST_FAILED, message))
        }
        PreReadyOutcome::Null => Response::success(id, Value::Null),
        PreReadyOutcome::Empty => Response::success(id, empty_result(method)),
    }
}

/// The empty result for a list-producing method: an empty, complete completion
/// list for completion, an empty array for the location/symbol/highlight lists.
fn empty_result(method: Method) -> Value {
    match method {
        Method::Completion => json!({ "isIncomplete": false, "items": [] }),
        _ => json!([]),
    }
}

/// Maps a request method name to the readiness-sensitive [`Method`], or `None`
/// for a method with no pre-ready fallback.
fn readiness_method(method: &str) -> Option<Method> {
    match method {
        "textDocument/completion" => Some(Method::Completion),
        "textDocument/hover" => Some(Method::Hover),
        "textDocument/signatureHelp" => Some(Method::SignatureHelp),
        "textDocument/definition" => Some(Method::Definition),
        "textDocument/typeDefinition" => Some(Method::TypeDefinition),
        "textDocument/references" => Some(Method::References),
        "textDocument/documentHighlight" => Some(Method::DocumentHighlight),
        "textDocument/prepareRename" => Some(Method::PrepareRename),
        "textDocument/rename" => Some(Method::Rename),
        "workspace/symbol" => Some(Method::WorkspaceSymbol),
        _ => None,
    }
}

/// The workspace root from `initialize` params: `rootUri`, else the first
/// workspace folder's uri, resolved to an absolute normalized path (dropped when
/// it does not parse).
fn root_from_params(params: &Value) -> Option<PathBuf> {
    let uri = params.get("rootUri").and_then(Value::as_str).or_else(|| {
        params
            .get("workspaceFolders")
            .and_then(Value::as_array)
            .and_then(|folders| folders.first())
            .and_then(|folder| folder.get("uri"))
            .and_then(Value::as_str)
    })?;
    uri_to_path(uri).ok().map(|path| normalize(&path))
}

fn document_uri(params: &Value) -> Option<String> {
    let raw = params.get("textDocument")?.get("uri")?.as_str()?;
    Some(normalize_uri(raw))
}

fn document_text(params: &Value) -> Option<String> {
    Some(
        params
            .get("textDocument")?
            .get("text")?
            .as_str()?
            .to_string(),
    )
}

fn last_change_text(params: &Value) -> Option<String> {
    Some(
        params
            .get("contentChanges")?
            .as_array()?
            .last()?
            .get("text")?
            .as_str()?
            .to_string(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    struct FixedBootstrap(WorkspaceState);
    impl Bootstrap for FixedBootstrap {
        fn run(&self, _workspace_root: Option<&Path>) -> WorkspaceState {
            self.0.clone()
        }
    }

    struct EchoHandlers;
    impl Handlers for EchoHandlers {
        fn handle(&self, request: &Request) -> Response {
            Response::success(request.id.clone(), json!({ "handled": request.method }))
        }
    }

    fn frame(body: Value) -> Vec<u8> {
        let text = serde_json::to_string(&body).unwrap();
        format!("Content-Length: {}\r\n\r\n{}", text.len(), text).into_bytes()
    }

    fn request(id: i64, method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
    }

    fn notification(method: &str, params: Value) -> Value {
        json!({ "jsonrpc": "2.0", "method": method, "params": params })
    }

    /// Reads every framed response written to `bytes` and indexes them by id.
    fn responses(bytes: Vec<u8>) -> Vec<Value> {
        let mut reader = Cursor::new(bytes);
        let mut out = Vec::new();
        while let Some(body) = read_frame(&mut reader).unwrap() {
            out.push(serde_json::from_slice(&body).unwrap());
        }
        out
    }

    fn run(input: Vec<Vec<u8>>, bootstrap: WorkspaceState) -> (ServerCore, Vec<Value>) {
        let mut reader = Cursor::new(input.concat());
        let mut writer = Vec::new();
        let mut core = ServerCore::new();
        serve(
            &mut reader,
            &mut writer,
            &mut core,
            &EchoHandlers,
            &FixedBootstrap(bootstrap),
        )
        .unwrap();
        (core, responses(writer))
    }

    // Ports the pre-ready dispatch and lifecycle of ls.core.ScalaLs.
    #[test]
    fn pre_ready_lifecycle_serves_fallbacks_and_syncs_documents() {
        let (core, out) = run(
            vec![
                frame(request(1, "initialize", json!({ "rootUri": "file:///ws" }))),
                frame(notification(
                    "textDocument/didOpen",
                    json!({ "textDocument": { "uri": "file:///ws/a.scala", "text": "hello" } }),
                )),
                frame(request(
                    2,
                    "textDocument/completion",
                    json!({ "textDocument": { "uri": "file:///ws/a.scala" } }),
                )),
                frame(request(3, "textDocument/references", json!({}))),
                frame(request(4, "textDocument/hover", json!({}))),
                frame(request(5, "textDocument/definition", json!({}))),
                frame(request(6, "shutdown", json!({}))),
                frame(notification("exit", json!({}))),
            ],
            // Never sent `initialized`, so the workspace stays not-ready.
            WorkspaceState::Ready,
        );

        // initialize answered the capabilities and stayed not-ready.
        assert_eq!(out[0]["id"], 1);
        assert_eq!(
            out[0]["result"]["serverInfo"]["name"],
            "scala3-bsp-semantic-ls"
        );
        assert!(!core.state.is_ready());
        assert_eq!(core.workspace_root, Some(PathBuf::from("/ws")));

        // Document sync recorded the open buffer under the normalized uri.
        assert_eq!(
            core.docs.text("file:///ws/a.scala").as_deref(),
            Some("hello")
        );

        // completion -> empty complete list; references -> not-ready error;
        // hover -> null; definition -> empty array.
        assert_eq!(out[1]["id"], 2);
        assert_eq!(
            out[1]["result"],
            json!({ "isIncomplete": false, "items": [] })
        );
        assert_eq!(out[2]["id"], 3);
        assert_eq!(out[2]["error"]["code"], error_codes::REQUEST_FAILED);
        assert!(out[2]["error"]["message"]
            .as_str()
            .unwrap()
            .starts_with("workspace is not ready"));
        assert_eq!(out[3]["result"], Value::Null);
        assert_eq!(out[4]["result"], json!([]));

        // shutdown -> null, and the server is marked shutting down.
        assert_eq!(out[5]["id"], 6);
        assert_eq!(out[5]["result"], Value::Null);
        assert!(core.shutting_down);
    }

    #[test]
    fn initialized_runs_bootstrap_and_ready_requests_delegate_to_handlers() {
        let (core, out) = run(
            vec![
                frame(request(1, "initialize", json!({}))),
                frame(notification("initialized", json!({}))),
                frame(request(2, "textDocument/completion", json!({}))),
                frame(notification("exit", json!({}))),
            ],
            WorkspaceState::Ready,
        );
        assert!(core.state.is_ready());
        assert_eq!(out[1]["id"], 2);
        assert_eq!(out[1]["result"]["handled"], "textDocument/completion");
    }

    #[test]
    fn a_failed_bootstrap_leaves_the_workspace_failed() {
        let (core, _out) = run(
            vec![
                frame(request(1, "initialize", json!({}))),
                frame(notification("initialized", json!({}))),
                frame(notification("exit", json!({}))),
            ],
            WorkspaceState::Failed {
                detail: "no build server".to_string(),
            },
        );
        assert_eq!(
            core.state,
            WorkspaceState::Failed {
                detail: "no build server".to_string()
            }
        );
    }

    #[test]
    fn a_request_before_initialize_is_server_not_initialized() {
        let (_core, out) = run(
            vec![
                frame(request(1, "textDocument/hover", json!({}))),
                frame(notification("exit", json!({}))),
            ],
            WorkspaceState::Ready,
        );
        assert_eq!(out[0]["error"]["code"], error_codes::SERVER_NOT_INITIALIZED);
    }

    // Ports the ls.core.ScalaLs.executeCommand dispatch.
    #[test]
    fn execute_command_routes_by_command_and_readiness() {
        // Not ready (no `initialized`): doctor renders via the handlers in any
        // state; reindex answers the unavailable status string; an unknown
        // command is an invalid-params error.
        let (_core, out) = run(
            vec![
                frame(request(1, "initialize", json!({}))),
                frame(request(
                    2,
                    "workspace/executeCommand",
                    json!({ "command": "scala3SemanticLs.doctor" }),
                )),
                frame(request(
                    3,
                    "workspace/executeCommand",
                    json!({ "command": "scala3SemanticLs.reindex" }),
                )),
                frame(request(
                    4,
                    "workspace/executeCommand",
                    json!({ "command": "bogus.command" }),
                )),
                frame(notification("exit", json!({}))),
            ],
            WorkspaceState::Ready,
        );
        assert_eq!(out[1]["id"], 2);
        assert_eq!(out[1]["result"]["handled"], "workspace/executeCommand");
        assert_eq!(out[2]["id"], 3);
        assert_eq!(
            out[2]["result"],
            "reindex unavailable: workspace is not ready: waiting for the initialized notification"
        );
        assert_eq!(out[3]["id"], 4);
        assert_eq!(out[3]["error"]["code"], error_codes::INVALID_PARAMS);
    }

    #[test]
    fn execute_command_reindex_delegates_to_handlers_when_ready() {
        let (_core, out) = run(
            vec![
                frame(request(1, "initialize", json!({}))),
                frame(notification("initialized", json!({}))),
                frame(request(
                    2,
                    "workspace/executeCommand",
                    json!({ "command": "scala3SemanticLs.reindex" }),
                )),
                frame(notification("exit", json!({}))),
            ],
            WorkspaceState::Ready,
        );
        assert_eq!(out[1]["id"], 2);
        assert_eq!(out[1]["result"]["handled"], "workspace/executeCommand");
    }

    // Ports ls.core.ScalaLs.resolveCompletionItem: echo pre-ready, resolve ready.
    #[test]
    fn completion_item_resolve_echoes_pre_ready_and_delegates_when_ready() {
        let (_c1, out1) = run(
            vec![
                frame(request(1, "initialize", json!({}))),
                frame(request(
                    2,
                    "completionItem/resolve",
                    json!({ "label": "foo", "data": 7 }),
                )),
                frame(notification("exit", json!({}))),
            ],
            WorkspaceState::Ready,
        );
        assert_eq!(out1[1]["result"], json!({ "label": "foo", "data": 7 }));

        let (_c2, out2) = run(
            vec![
                frame(request(1, "initialize", json!({}))),
                frame(notification("initialized", json!({}))),
                frame(request(
                    2,
                    "completionItem/resolve",
                    json!({ "label": "foo" }),
                )),
                frame(notification("exit", json!({}))),
            ],
            WorkspaceState::Ready,
        );
        assert_eq!(out2[1]["result"]["handled"], "completionItem/resolve");
    }

    #[test]
    fn shutdown_is_idempotent() {
        let mut core = ServerCore::new();
        core.shutdown();
        core.state = WorkspaceState::Ready; // a stray later transition
        core.shutdown(); // second shutdown must not overwrite state again
        assert!(core.state.is_ready());
        assert!(core.shutting_down);
    }

    #[test]
    fn did_change_full_sync_takes_the_last_change_and_did_close_drops_the_buffer() {
        let core = ServerCore::new();
        core.did_open(&json!({ "textDocument": { "uri": "file:///a", "text": "v1" } }));
        core.did_change(&json!({
            "textDocument": { "uri": "file:///a" },
            "contentChanges": [ { "text": "stale" }, { "text": "v2" } ]
        }));
        assert_eq!(core.docs.text("file:///a").as_deref(), Some("v2"));
        core.did_close(&json!({ "textDocument": { "uri": "file:///a" } }));
        assert!(!core.docs.is_open("file:///a"));
    }

    #[test]
    fn did_save_with_text_refreshes_an_open_buffer_only() {
        let core = ServerCore::new();
        core.did_save(&json!({ "textDocument": { "uri": "file:///a" }, "text": "saved" }));
        // Not open -> the save text is ignored.
        assert!(!core.docs.is_open("file:///a"));
        core.did_open(&json!({ "textDocument": { "uri": "file:///a", "text": "v1" } }));
        core.did_save(&json!({ "textDocument": { "uri": "file:///a" }, "text": "saved" }));
        assert_eq!(core.docs.text("file:///a").as_deref(), Some("saved"));
    }
}
