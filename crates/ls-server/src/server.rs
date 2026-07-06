//! The stdio server loop and the workspace lifecycle state machine.
//!
//! [`serve`] reads framed JSON-RPC messages, answers `initialize` with the
//! capability surface (leaving the workspace [`WorkspaceState::NotReady`]), runs
//! bootstrap on `initialized` (transitioning to [`WorkspaceState::Ready`], which
//! owns the ready services, or [`WorkspaceState::Failed`]), keeps the document
//! store in sync, serves the per-method pre-ready fallbacks until the workspace
//! is ready, delegates ready-path requests to the services, and handles
//! `shutdown`/`exit`. A behavior-preserving port of the `ls.core.ScalaLs`
//! lifecycle.
//!
//! The ready services and the request/command handlers are reached through an
//! explicit context ([`BootstrapContext`], [`RequestContext`]), so a production
//! [`Bootstrap`]/[`Handlers`] pair — over BSP discovery, the embedded JVM,
//! ingest, and the engine — attaches to the ready state without a second copy of
//! server state. Bootstrap runs on the message loop here; running it off the
//! loop so pre-ready requests are served concurrently is an orthogonal
//! concurrency change.

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
use crate::protocol::PublishDiagnosticsParams;

/// The inputs and callbacks the bootstrap needs: the normalized workspace root,
/// the open-buffer document store, a hook to publish build diagnostics to the
/// client, and a hook the build server calls when its target set changes.
pub struct BootstrapContext<'a> {
    pub workspace_root: Option<&'a Path>,
    pub documents: &'a DocumentStore,
    pub publish_diagnostics: &'a dyn Fn(PublishDiagnosticsParams),
    pub on_build_targets_changed: &'a dyn Fn(),
}

/// The workspace bootstrap, run on `initialized`. It discovers the build server,
/// boots the JVM, and ingests, producing either the ready services or a failure;
/// tests inject a fixed transition.
pub trait Bootstrap<S> {
    fn run(&self, cx: BootstrapContext<'_>) -> WorkspaceState<S>;
}

/// The context a ready-path handler receives: the request plus everything the
/// retained server reads to answer it — the ready services, the workspace root,
/// the open documents, and whether the server is shutting down.
pub struct RequestContext<'a, S> {
    pub request: &'a Request,
    pub services: &'a S,
    pub workspace_root: Option<&'a Path>,
    pub documents: &'a DocumentStore,
    pub shutting_down: bool,
}

/// The subsystem-backed request handlers, delegated to for the work that needs
/// the engine/BSP/PC services: the ready-path query answers, `completionItem/
/// resolve` when ready, and the ready-path `executeCommand` actions. The doctor
/// report is a context built-in ([`doctor_report`]) so it renders in every
/// state. The production impl is wired over the real subsystems; tests inject a
/// fake.
pub trait Handlers<S> {
    fn handle(&self, cx: RequestContext<'_, S>) -> Response;
}

/// The client-facing callbacks the server is wired with: publishing diagnostics
/// and reacting to build-target changes.
pub struct ServerHooks<'a> {
    pub publish_diagnostics: &'a dyn Fn(PublishDiagnosticsParams),
    pub on_build_targets_changed: &'a dyn Fn(),
}

/// The mutable server state driven by the message loop.
pub struct ServerCore<S> {
    pub state: WorkspaceState<S>,
    pub docs: DocumentStore,
    pub workspace_root: Option<PathBuf>,
    pub shutting_down: bool,
    initialized: bool,
}

impl<S> ServerCore<S> {
    pub fn new() -> ServerCore<S> {
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

    /// Handles `initialized` by running bootstrap with the context it needs and
    /// adopting its outcome.
    pub fn run_bootstrap(&mut self, bootstrap: &impl Bootstrap<S>, hooks: &ServerHooks<'_>) {
        let next = {
            let cx = BootstrapContext {
                workspace_root: self.workspace_root.as_deref(),
                documents: &self.docs,
                publish_diagnostics: hooks.publish_diagnostics,
                on_build_targets_changed: hooks.on_build_targets_changed,
            };
            bootstrap.run(cx)
        };
        self.state = next;
    }

    /// Handles `shutdown`: idempotently marks the server shutting down and moves
    /// to `NotReady("server is shut down")`. Ready-service teardown is the
    /// bootstrap's inverse and is owned by the services.
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
        // reverse-dependency compile and reingest belong to the didSave build
        // flow, not this buffer sync.
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

impl<S> Default for ServerCore<S> {
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
pub fn serve<S>(
    reader: &mut impl BufRead,
    writer: &mut impl Write,
    core: &mut ServerCore<S>,
    handlers: &impl Handlers<S>,
    bootstrap: &impl Bootstrap<S>,
    hooks: &ServerHooks<'_>,
) -> io::Result<()> {
    while let Some(body) = read_frame(reader)? {
        match parse_incoming(&body) {
            Ok(Incoming::Request(request)) => {
                let response = dispatch_request(core, handlers, request);
                write_frame(writer, &response)?;
            }
            Ok(Incoming::Notification(note)) => {
                if let Flow::Stop = dispatch_notification(core, bootstrap, hooks, note) {
                    break;
                }
            }
            Err(error) => write_null_id_error(writer, &error)?,
        }
    }
    Ok(())
}

fn dispatch_request<S>(
    core: &mut ServerCore<S>,
    handlers: &impl Handlers<S>,
    request: Request,
) -> Response {
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
        // Advertised as `resolveProvider`: resolve via the services when ready,
        // otherwise echo the item back unchanged (the Scala `case _ => item`).
        "completionItem/resolve" => {
            if core.state.is_ready() {
                ready_handle(core, handlers, &request)
            } else {
                Response::success(request.id.clone(), request.params.clone())
            }
        }
        "workspace/executeCommand" => execute_command(core, handlers, &request),
        method => match readiness_method(method) {
            Some(_) if core.state.is_ready() => ready_handle(core, handlers, &request),
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

/// Delegates a ready-path request to the handlers with the full request context.
/// Only called when the workspace is ready.
fn ready_handle<S>(
    core: &ServerCore<S>,
    handlers: &impl Handlers<S>,
    request: &Request,
) -> Response {
    let services = core
        .state
        .ready()
        .expect("ready_handle is only called when the workspace is ready");
    handlers.handle(RequestContext {
        request,
        services,
        workspace_root: core.workspace_root.as_deref(),
        documents: &core.docs,
        shutting_down: core.shutting_down,
    })
}

/// Dispatches `workspace/executeCommand` as ScalaLs does: the doctor report
/// renders in any state from the context; reindex/compile run through the
/// services when ready and otherwise answer a typed "unavailable" status string;
/// an unknown command (including the un-advertised pcPluginStatus) is an
/// invalid-params error.
fn execute_command<S>(
    core: &ServerCore<S>,
    handlers: &impl Handlers<S>,
    request: &Request,
) -> Response {
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
    match request.params.get("command").and_then(Value::as_str) {
        Some(commands::DOCTOR) => {
            Response::success(request.id.clone(), Value::String(doctor_report(core)))
        }
        Some(commands::REINDEX) if ready => ready_handle(core, handlers, request),
        Some(commands::REINDEX) => unavailable("reindex"),
        Some(commands::COMPILE) if ready => ready_handle(core, handlers, request),
        Some(commands::COMPILE) => unavailable("compile"),
        // A missing command is `null` in the Scala `ExecuteCommandParams`, so its
        // unknown-command message interpolates the string "null"; a present but
        // unknown command uses its own text.
        other => Response::failure(
            request.id.clone(),
            ResponseError::new(
                error_codes::INVALID_PARAMS,
                format!("unknown command '{}'", other.unwrap_or("null")),
            ),
        ),
    }
}

/// The `scala3SemanticLs.doctor` result. Renders the `state:` header from the
/// current context in every state, matching `DoctorCommand.report`. The
/// runtime/store/semanticdb/postings/pc report sections are gathered by the
/// doctor module.
fn doctor_report<S>(core: &ServerCore<S>) -> String {
    format!("state: {}\n\n", core.state.status_line())
}

fn dispatch_notification<S>(
    core: &mut ServerCore<S>,
    bootstrap: &impl Bootstrap<S>,
    hooks: &ServerHooks<'_>,
    note: Notification,
) -> Flow {
    match note.method.as_str() {
        "initialized" => core.run_bootstrap(bootstrap, hooks),
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
fn pre_ready_response<S>(id: RequestId, method: Method, state: &WorkspaceState<S>) -> Response {
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
    use std::cell::Cell;
    use std::io::Cursor;

    /// A fake ready-services bundle carrying a marker the handler echoes, so a
    /// test can prove the `Ready(services)` value reached the handler.
    #[derive(Clone, Debug, PartialEq, Eq)]
    struct FakeServices {
        tag: String,
    }

    /// Handlers that echo the whole request context back, so a dropped context
    /// field fails the asserting test rather than passing silently.
    struct EchoHandlers;
    impl Handlers<FakeServices> for EchoHandlers {
        fn handle(&self, cx: RequestContext<'_, FakeServices>) -> Response {
            Response::success(
                cx.request.id.clone(),
                json!({
                    "method": cx.request.method,
                    "services": cx.services.tag,
                    "root": cx.workspace_root.map(|p| p.display().to_string()),
                    "openDocs": cx.documents.open_uris(),
                    "shuttingDown": cx.shutting_down,
                }),
            )
        }
    }

    struct FixedBootstrap(WorkspaceState<FakeServices>);
    impl Bootstrap<FakeServices> for FixedBootstrap {
        fn run(&self, _cx: BootstrapContext<'_>) -> WorkspaceState<FakeServices> {
            self.0.clone()
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

    fn responses(bytes: Vec<u8>) -> Vec<Value> {
        let mut reader = Cursor::new(bytes);
        let mut out = Vec::new();
        while let Some(body) = read_frame(&mut reader).unwrap() {
            out.push(serde_json::from_slice(&body).unwrap());
        }
        out
    }

    fn ready(tag: &str) -> WorkspaceState<FakeServices> {
        WorkspaceState::Ready(FakeServices {
            tag: tag.to_string(),
        })
    }

    /// Drives `serve` over the scripted input with the echo handlers, a fixed
    /// bootstrap outcome, and no-op hooks.
    fn run(
        input: Vec<Vec<u8>>,
        bootstrap: WorkspaceState<FakeServices>,
    ) -> (ServerCore<FakeServices>, Vec<Value>) {
        let mut reader = Cursor::new(input.concat());
        let mut writer = Vec::new();
        let mut core = ServerCore::new();
        let publish = |_p: PublishDiagnosticsParams| {};
        let on_changed = || {};
        let hooks = ServerHooks {
            publish_diagnostics: &publish,
            on_build_targets_changed: &on_changed,
        };
        serve(
            &mut reader,
            &mut writer,
            &mut core,
            &EchoHandlers,
            &FixedBootstrap(bootstrap),
            &hooks,
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
            ready("unused"),
        );

        assert_eq!(out[0]["id"], 1);
        assert_eq!(
            out[0]["result"]["serverInfo"]["name"],
            "scala3-bsp-semantic-ls"
        );
        assert!(!core.state.is_ready());
        assert_eq!(core.workspace_root, Some(PathBuf::from("/ws")));
        assert_eq!(
            core.docs.text("file:///ws/a.scala").as_deref(),
            Some("hello")
        );

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
        assert_eq!(out[5]["result"], Value::Null);
        assert!(core.shutting_down);
    }

    // The ready seam: the services, workspace root, and open documents all reach
    // the ready handler through the request context.
    #[test]
    fn ready_context_exposes_services_root_and_documents() {
        let (core, out) = run(
            vec![
                frame(request(1, "initialize", json!({ "rootUri": "file:///ws" }))),
                frame(notification(
                    "textDocument/didOpen",
                    json!({ "textDocument": { "uri": "file:///ws/a.scala", "text": "x" } }),
                )),
                frame(notification("initialized", json!({}))),
                frame(request(2, "textDocument/completion", json!({}))),
                frame(notification("exit", json!({}))),
            ],
            ready("svc-42"),
        );
        assert!(core.state.is_ready());
        let result = &out[1]["result"];
        assert_eq!(result["method"], "textDocument/completion");
        assert_eq!(result["services"], "svc-42");
        assert_eq!(result["root"], "/ws");
        assert_eq!(result["openDocs"], json!(["file:///ws/a.scala"]));
        assert_eq!(result["shuttingDown"], false);
    }

    // The ready seam reaches references/rename, completionItem/resolve, and the
    // ready executeCommand actions too — all through the same context.
    #[test]
    fn ready_seam_reaches_every_delegated_method() {
        for (method, params) in [
            ("textDocument/references", json!({})),
            ("textDocument/rename", json!({})),
            ("completionItem/resolve", json!({ "label": "x" })),
            (
                "workspace/executeCommand",
                json!({ "command": "scala3SemanticLs.reindex" }),
            ),
            (
                "workspace/executeCommand",
                json!({ "command": "scala3SemanticLs.compile" }),
            ),
        ] {
            let (_core, out) = run(
                vec![
                    frame(request(1, "initialize", json!({}))),
                    frame(notification("initialized", json!({}))),
                    frame(request(2, method, params)),
                    frame(notification("exit", json!({}))),
                ],
                ready("svc"),
            );
            assert_eq!(
                out[1]["result"]["services"], "svc",
                "{method} lost the services"
            );
        }
    }

    // Bootstrap receives the workspace root, the open documents, and both hooks.
    #[test]
    fn bootstrap_receives_root_documents_and_hooks() {
        struct RecordingBootstrap;
        impl Bootstrap<FakeServices> for RecordingBootstrap {
            fn run(&self, cx: BootstrapContext<'_>) -> WorkspaceState<FakeServices> {
                (cx.on_build_targets_changed)();
                (cx.publish_diagnostics)(PublishDiagnosticsParams {
                    uri: "file:///ws/a.scala".to_string(),
                    diagnostics: Vec::new(),
                });
                WorkspaceState::Ready(FakeServices {
                    tag: format!(
                        "root={} docs={}",
                        cx.workspace_root
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                        cx.documents.open_uris().len()
                    ),
                })
            }
        }

        let mut reader = Cursor::new(
            [
                frame(request(1, "initialize", json!({ "rootUri": "file:///ws" }))),
                frame(notification(
                    "textDocument/didOpen",
                    json!({ "textDocument": { "uri": "file:///ws/a.scala", "text": "x" } }),
                )),
                frame(notification("initialized", json!({}))),
                frame(notification("exit", json!({}))),
            ]
            .concat(),
        );
        let mut writer = Vec::new();
        let mut core = ServerCore::new();
        let changed = Cell::new(0);
        let published = Cell::new(0);
        let on_changed = || changed.set(changed.get() + 1);
        let publish = |_p: PublishDiagnosticsParams| published.set(published.get() + 1);
        let hooks = ServerHooks {
            publish_diagnostics: &publish,
            on_build_targets_changed: &on_changed,
        };
        serve(
            &mut reader,
            &mut writer,
            &mut core,
            &EchoHandlers,
            &RecordingBootstrap,
            &hooks,
        )
        .unwrap();

        assert_eq!(
            changed.get(),
            1,
            "on_build_targets_changed hook not received"
        );
        assert_eq!(published.get(), 1, "publish_diagnostics hook not received");
        assert_eq!(core.state.ready().unwrap().tag, "root=/ws docs=1");
    }

    // Doctor renders the state header from the context in every state.
    #[test]
    fn doctor_renders_the_state_header_in_every_state() {
        let doctor = |state: WorkspaceState<FakeServices>, send_initialized: bool| {
            let mut input = vec![frame(request(1, "initialize", json!({})))];
            if send_initialized {
                input.push(frame(notification("initialized", json!({}))));
            }
            input.push(frame(request(
                2,
                "workspace/executeCommand",
                json!({ "command": "scala3SemanticLs.doctor" }),
            )));
            input.push(frame(notification("exit", json!({}))));
            let (_core, out) = run(input, state);
            out[1]["result"].as_str().unwrap().to_string()
        };
        // Not ready (no initialized): the pre-ready state header.
        assert_eq!(
            doctor(ready("unused"), false),
            "state: not ready: waiting for the initialized notification\n\n"
        );
        // Ready.
        assert_eq!(doctor(ready("svc"), true), "state: ready\n\n");
        // Failed.
        assert_eq!(
            doctor(
                WorkspaceState::Failed {
                    detail: "no build server".to_string()
                },
                true
            ),
            "state: bootstrap failed: no build server\n\n"
        );
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
        assert!(matches!(core.state, WorkspaceState::Failed { .. }));
        assert_eq!(
            core.state.status_line(),
            "bootstrap failed: no build server"
        );
    }

    #[test]
    fn a_request_before_initialize_is_server_not_initialized() {
        let (_core, out) = run(
            vec![
                frame(request(1, "textDocument/hover", json!({}))),
                frame(notification("exit", json!({}))),
            ],
            ready("unused"),
        );
        assert_eq!(out[0]["error"]["code"], error_codes::SERVER_NOT_INITIALIZED);
    }

    // Ports the ls.core.ScalaLs.executeCommand pre-ready dispatch.
    #[test]
    fn execute_command_pre_ready_and_unknown() {
        let (_core, out) = run(
            vec![
                frame(request(1, "initialize", json!({}))),
                frame(request(
                    2,
                    "workspace/executeCommand",
                    json!({ "command": "scala3SemanticLs.reindex" }),
                )),
                frame(request(
                    3,
                    "workspace/executeCommand",
                    json!({ "command": "bogus.command" }),
                )),
                // No `command` field: Scala's null getCommand renders "null".
                frame(request(4, "workspace/executeCommand", json!({}))),
                // The un-advertised pcPluginStatus is an unknown command.
                frame(request(
                    5,
                    "workspace/executeCommand",
                    json!({ "command": "scala3SemanticLs.pcPluginStatus" }),
                )),
                frame(notification("exit", json!({}))),
            ],
            ready("unused"),
        );
        assert_eq!(
            out[1]["result"],
            "reindex unavailable: workspace is not ready: waiting for the initialized notification"
        );
        assert_eq!(out[2]["error"]["code"], error_codes::INVALID_PARAMS);
        assert_eq!(
            out[2]["error"]["message"],
            "unknown command 'bogus.command'"
        );
        assert_eq!(out[3]["error"]["message"], "unknown command 'null'");
        assert_eq!(out[4]["error"]["code"], error_codes::INVALID_PARAMS);
        assert_eq!(
            out[4]["error"]["message"],
            "unknown command 'scala3SemanticLs.pcPluginStatus'"
        );
    }

    // Ports ls.core.ScalaLs.resolveCompletionItem: echo pre-ready.
    #[test]
    fn completion_item_resolve_echoes_pre_ready() {
        let (_core, out) = run(
            vec![
                frame(request(1, "initialize", json!({}))),
                frame(request(
                    2,
                    "completionItem/resolve",
                    json!({ "label": "foo", "data": 7 }),
                )),
                frame(notification("exit", json!({}))),
            ],
            ready("unused"),
        );
        assert_eq!(out[1]["result"], json!({ "label": "foo", "data": 7 }));
    }

    #[test]
    fn shutdown_is_idempotent() {
        let mut core: ServerCore<FakeServices> = ServerCore::new();
        core.shutdown();
        core.state = ready("late");
        core.shutdown();
        assert!(core.state.is_ready());
        assert!(core.shutting_down);
    }

    #[test]
    fn did_change_full_sync_takes_the_last_change_and_did_close_drops_the_buffer() {
        let core: ServerCore<FakeServices> = ServerCore::new();
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
        let core: ServerCore<FakeServices> = ServerCore::new();
        core.did_save(&json!({ "textDocument": { "uri": "file:///a" }, "text": "saved" }));
        assert!(!core.docs.is_open("file:///a"));
        core.did_open(&json!({ "textDocument": { "uri": "file:///a", "text": "v1" } }));
        core.did_save(&json!({ "textDocument": { "uri": "file:///a" }, "text": "saved" }));
        assert_eq!(core.docs.text("file:///a").as_deref(), Some("saved"));
    }
}
