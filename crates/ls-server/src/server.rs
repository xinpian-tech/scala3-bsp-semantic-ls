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
//! The request/command handlers are reached through an explicit
//! [`RequestContext`], so a production [`Bootstrap`]/[`Handlers`] pair — over BSP
//! discovery, the embedded JVM, ingest, and the engine — attaches to the ready
//! state without a second copy of server state. Bootstrap runs OFF the message
//! loop on a worker thread ([`Bootstrap::build`]): `initialized` spawns it, the
//! workspace stays `NotReady` (so pre-ready requests are served concurrently with
//! the per-method fallbacks), and the loop adopts Ready/Failed and replays the
//! open buffers ([`Bootstrap::replay`]) when the worker completes.

use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

use serde::Serialize;
use serde_json::{json, Value};

use ls_index_model::uri::{normalize, normalize_uri, uri_to_path};

use crate::capabilities::{commands, initialize_result, InitializeResult};
use crate::doctor::DoctorReport;
use crate::documents::{ContentChange, DocumentStore};
use crate::jsonrpc::{
    error_codes, parse_incoming, read_frame, write_frame, Incoming, Notification, Request,
    RequestId, Response, ResponseError,
};
use crate::lifecycle::{pre_ready_outcome, require_ready, Method, PreReadyOutcome, WorkspaceState};
use crate::protocol::PublishDiagnosticsParams;

/// The workspace bootstrap. Its [`build`](Bootstrap::build) discovers the build
/// server, boots the JVM, and ingests, producing either the ready services or a
/// failure; it runs on `initialized`, OFF the message loop, on a worker thread,
/// so it takes an OWNED workspace root and borrows nothing from the server. Open
/// buffers accumulated during the pre-ready window are replayed by
/// [`replay`](Bootstrap::replay) on the loop once Ready is installed. It also
/// reloads the ready services after a build-target change, refetching over the
/// retained session (default: keep the current services — a fixed/fake bootstrap
/// has nothing to refetch). Tests inject a fixed transition.
pub trait Bootstrap<S> {
    /// Build the ready services (or a failure) from an owned workspace root. Runs
    /// on the bootstrap worker thread; must not borrow server state.
    fn build(&self, workspace_root: Option<PathBuf>) -> WorkspaceState<S>;

    /// Seed the freshly-ready services from the open buffers (and install any
    /// document-backed overlay), on the message loop after Ready is installed.
    /// Receives the shared document-store handle so a Ready bundle can retain it.
    /// Default no-op (a fake has no buffer mirror).
    fn replay(&self, _services: &S, _documents: &Arc<DocumentStore>) {}

    /// Reload the ready services after the build server reports its targets
    /// changed, reusing the durable handles. `old` is the current ready bundle.
    fn reload(&self, old: S, _documents: &Arc<DocumentStore>) -> WorkspaceState<S> {
        WorkspaceState::Ready(old)
    }
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
/// report renders in every state ([`doctor_result`]); its live sections come from
/// the [`Handlers::doctor`] hook when ready. The production impl is wired over the
/// real subsystems; tests inject a fake.
///
/// The document-lifecycle hooks (`on_did_open`/`on_did_change`/`on_did_close`)
/// let the ready services react to buffer notifications — the production impl
/// forwards them to the presentation-compiler buffer mirror so an unsaved open
/// buffer is visible to a later PC query and a closed buffer is dropped. They are
/// invoked only when the workspace is ready and default to no-ops, so a services
/// bundle that needs no buffer mirror (and the test fakes) inherit the empty
/// behavior. Ports the `TextDocs.didOpen`/`didChange`/`didClose` PC forwarding.
pub trait Handlers<S> {
    fn handle(&self, cx: RequestContext<'_, S>) -> Response;

    /// A buffer was opened (already synced into the document store). `uri` is
    /// normalized.
    fn on_did_open(&self, _services: &S, _uri: &str, _text: &str) {}

    /// An open buffer's text changed. `text` is the FULL post-edit document —
    /// the loop folds the incremental `contentChanges` events into the store
    /// before this hook, so the seam stays full-text. `uri` is normalized.
    fn on_did_change(&self, _services: &S, _uri: &str, _text: &str) {}

    /// A buffer was closed (already dropped from the document store). `uri` is
    /// normalized.
    fn on_did_close(&self, _services: &S, _uri: &str) {}

    /// A buffer was saved (its text, if sent, already re-synced). `uri` is
    /// normalized. Ports the build-job tail of `ScalaLs.didSave`: schedule the
    /// debounced compile-first reingest of the saved file's reverse-dependency
    /// closure.
    fn on_did_save(&self, _services: &S, _uri: &str) {}

    /// `workspace/didChangeConfiguration` arrived while ready. The notification's
    /// `settings` payload is deliberately ignored — the workspace
    /// `.scala3-bsp-semantic-ls/config.json` stays the single configuration
    /// source — the hook only lets the ready services re-read that file.
    /// Default no-op.
    fn on_did_change_configuration(&self, _services: &S) {}

    /// The live doctor report for a ready workspace (the `Runtime`/`Nix`/`Store`
    /// plus the live `BSP`/`SemanticDB`/`PC` sections). `None` when this handler
    /// has no live report to add (a fake, or a non-`CoreServices` bundle), in
    /// which case the offline report is rendered from the workspace root. Never
    /// boots the embedded JVM.
    fn doctor(&self, _services: &S, _workspace_root: Option<&Path>) -> Option<DoctorReport> {
        None
    }
}

/// A thread-safe, frame-atomic output sink for the client connection. Both the
/// message loop (request responses) and the build server's reader thread (async
/// `textDocument/publishDiagnostics`) write whole frames through the one lock, so
/// frames never interleave on the wire and a diagnostic that arrives while the
/// loop is parked reading the next request still reaches the editor immediately —
/// the loop no longer has to wake to flush it. Ports the LSP client's inherent
/// thread-safe `publishDiagnostics`.
pub struct OutputSink<W> {
    writer: Mutex<W>,
}

impl<W: Write> OutputSink<W> {
    pub fn new(writer: W) -> OutputSink<W> {
        OutputSink {
            writer: Mutex::new(writer),
        }
    }

    /// Writes one framed message, holding the lock across the whole frame.
    pub fn send(&self, message: &impl Serialize) -> io::Result<()> {
        write_frame(&mut *self.writer.lock().unwrap(), message)
    }

    /// Publishes diagnostics for one file as a `textDocument/publishDiagnostics`
    /// notification. Callable from the build server's reader thread.
    pub fn publish_diagnostics(&self, params: &PublishDiagnosticsParams) -> io::Result<()> {
        self.send(&json!({
            "jsonrpc": "2.0",
            "method": "textDocument/publishDiagnostics",
            "params": params,
        }))
    }
}

impl<W: Write + Clone> OutputSink<W> {
    /// A copy of everything written so far (test-only; production writes stdout).
    pub fn written(&self) -> W {
        self.writer.lock().unwrap().clone()
    }
}

/// The mutable server state driven by the message loop.
pub struct ServerCore<S> {
    pub state: WorkspaceState<S>,
    /// The open-buffer store, a shared handle so the ready bundle's PC-backed
    /// dirty-buffer overlay reads the SAME live buffers the message loop updates.
    pub docs: Arc<DocumentStore>,
    pub workspace_root: Option<PathBuf>,
    pub shutting_down: bool,
    initialized: bool,
    /// Set (from the build server's reader thread) when the build targets change;
    /// drained on the message loop, which reloads the ready model. An `AtomicBool`
    /// is the only state shared with the reader thread — the reload itself runs on
    /// the loop, so the ready services stay single-threaded.
    reload_requested: Arc<AtomicBool>,
    /// The in-flight bootstrap worker's result channel, set when `initialized`
    /// spawns the worker and cleared when its result is adopted. While it is
    /// `Some`, the workspace stays `NotReady` and pre-ready fallbacks are served;
    /// the loop adopts Ready/Failed only when the worker actually sends a result.
    bootstrap_rx: Option<mpsc::Receiver<WorkspaceState<S>>>,
    /// The worker thread handle, joined when its result is adopted.
    bootstrap_handle: Option<thread::JoinHandle<()>>,
}

impl<S> ServerCore<S> {
    pub fn new() -> ServerCore<S> {
        ServerCore {
            state: WorkspaceState::NotReady {
                detail: "initialize has not run".to_string(),
            },
            docs: Arc::new(DocumentStore::new()),
            workspace_root: None,
            shutting_down: false,
            initialized: false,
            reload_requested: Arc::new(AtomicBool::new(false)),
            bootstrap_rx: None,
            bootstrap_handle: None,
        }
    }

    /// A handle to the build-targets-changed flag, for the live model source to
    /// set from the BSP reader thread when the server reports a target change.
    pub fn reload_flag(&self) -> Arc<AtomicBool> {
        self.reload_requested.clone()
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

    fn did_open(&self, handlers: &impl Handlers<S>, params: &Value) {
        let (Some(uri), Some(text)) = (document_uri(params), document_text(params)) else {
            return;
        };
        self.docs
            .open_versioned(&uri, &text, document_version(params).unwrap_or(0));
        if let Some(services) = self.state.ready() {
            handlers.on_did_open(services, &uri, &text);
        }
    }

    fn did_change(&self, handlers: &impl Handlers<S>, params: &Value) {
        // Incremental sync: fold the contentChanges event list onto the buffer
        // and forward the FULL post-edit text (the downstream seam is full-text).
        let Some(uri) = document_uri(params) else {
            return;
        };
        let Some(changes) = content_changes(params) else {
            eprintln!("ls-server: ignoring a didChange with unparseable contentChanges for {uri}");
            return;
        };
        if changes.is_empty() {
            return;
        }
        let Some(text) = self
            .docs
            .apply_changes(&uri, document_version(params), &changes)
        else {
            eprintln!(
                "ls-server: dropping a ranged didChange for {uri}: the buffer was never opened"
            );
            return;
        };
        if let Some(services) = self.state.ready() {
            handlers.on_did_change(services, &uri, &text);
        }
    }

    fn did_close(&self, handlers: &impl Handlers<S>, params: &Value) {
        let Some(uri) = document_uri(params) else {
            return;
        };
        self.docs.close(&uri);
        if let Some(services) = self.state.ready() {
            handlers.on_did_close(services, &uri);
        }
    }

    fn did_save(&self, handlers: &impl Handlers<S>, params: &Value) {
        let Some(uri) = document_uri(params) else {
            return;
        };
        // A save that carries the text refreshes the open buffer so dirtiness
        // clears even when the editor folded the last edit into the save. The
        // save still schedules its build job when no text is sent.
        if let Some(text) = params.get("text").and_then(Value::as_str) {
            if self.docs.is_open(&uri) {
                self.docs.change(&uri, text);
            }
        }
        // The reverse-dependency compile + reingest build job (Scala didSave tail).
        if let Some(services) = self.state.ready() {
            handlers.on_did_save(services, &uri);
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
pub fn serve<S, B, W>(
    reader: &mut impl BufRead,
    sink: &OutputSink<W>,
    core: &mut ServerCore<S>,
    handlers: &impl Handlers<S>,
    bootstrap: B,
) -> io::Result<()>
where
    S: Send + 'static,
    B: Bootstrap<S> + Send + Sync + 'static,
    W: Write,
{
    // Build-server diagnostics are written to `sink` directly from the session
    // reader thread (see the live model source's `on_diagnostics`); the loop only
    // writes request responses, and the shared lock keeps the two serialized.
    let bootstrap = Arc::new(bootstrap);
    let mut exiting = false;
    while let Some(body) = read_frame(reader)? {
        // At the top of each turn, adopt a completed async bootstrap and then drain
        // a build-target reload, so the message just read sees the freshest state.
        poll_bootstrap(core, &*bootstrap);
        poll_reload(core, &*bootstrap);
        match parse_incoming(&body) {
            Ok(Incoming::Request(request)) => {
                let response = dispatch_request(core, handlers, request);
                sink.send(&response)?;
            }
            Ok(Incoming::Notification(note)) => {
                if let Flow::Stop = dispatch_notification(core, handlers, &bootstrap, note) {
                    exiting = true;
                    break;
                }
            }
            Ok(Incoming::Response(response)) => {
                // An inbound client response (a reply to a server-to-client
                // request): the server has no such request in flight yet, so it
                // is consumed and dropped — never answered with an error frame.
                eprintln!(
                    "ls-server: ignoring a client response with no matching request (id: {:?})",
                    response.id
                );
            }
            Err(error) => sink.send(&null_id_error(&error))?,
        }
    }
    if exiting || core.shutting_down {
        // `exit`, or a shutting-down server: detach any in-flight worker and return
        // promptly — a late result must neither resurrect the shut-down state nor
        // block the client's teardown on bootstrap.
        detach_bootstrap(core);
    } else {
        // A clean EOF with no shutdown/exit: adopt an in-flight bootstrap result so a
        // `serve` call that stops right after `initialized` still reaches Ready
        // (pump-until-ready).
        drain_bootstrap(core, &*bootstrap);
    }
    Ok(())
}

/// The `null`-id error response for a frame that could not be parsed into a
/// request (so no id is available to correlate the reply).
fn null_id_error(error: &ResponseError) -> Value {
    json!({ "jsonrpc": "2.0", "id": Value::Null, "error": error })
}

/// Spawns the bootstrap worker on `initialized`: one worker per session (a second
/// `initialized` is ignored while one is in flight). The worker owns the workspace
/// root and the shared bootstrap, borrows nothing from the server, and sends its
/// `WorkspaceState` result over the channel; the workspace stays `NotReady` until
/// the loop adopts it. Ports `ScalaLs.initialized` submitting the index bootstrap.
fn spawn_bootstrap<S, B>(core: &mut ServerCore<S>, bootstrap: &Arc<B>)
where
    S: Send + 'static,
    B: Bootstrap<S> + Send + Sync + 'static,
{
    if core.bootstrap_rx.is_some() {
        return;
    }
    let (tx, rx) = mpsc::channel();
    let worker = Arc::clone(bootstrap);
    let root = core.workspace_root.clone();
    let handle = thread::spawn(move || {
        // The receiver may already be gone (loop ended before adoption); dropping
        // the result is fine.
        let _ = tx.send(worker.build(root));
    });
    core.bootstrap_rx = Some(rx);
    core.bootstrap_handle = Some(handle);
}

/// Non-blocking: adopt the bootstrap worker's result if it has arrived, else leave
/// the workspace `NotReady`. A worker that dropped its sender without a result
/// (panicked) yields a typed failure rather than a hang.
fn poll_bootstrap<S, B: Bootstrap<S>>(core: &mut ServerCore<S>, bootstrap: &B) {
    let Some(rx) = &core.bootstrap_rx else {
        return;
    };
    match rx.try_recv() {
        Ok(result) => adopt_bootstrap(core, bootstrap, Some(result)),
        Err(mpsc::TryRecvError::Empty) => {}
        Err(mpsc::TryRecvError::Disconnected) => adopt_bootstrap(core, bootstrap, None),
    }
}

/// Blocking: adopt an in-flight bootstrap result, waiting for the worker if
/// necessary. Called at loop end so a `serve` call that stops right after
/// `initialized` still reaches Ready.
fn drain_bootstrap<S, B: Bootstrap<S>>(core: &mut ServerCore<S>, bootstrap: &B) {
    let Some(rx) = core.bootstrap_rx.as_ref() else {
        return;
    };
    let result = rx.recv().ok();
    adopt_bootstrap(core, bootstrap, result);
}

/// Installs a bootstrap result: joins the worker, replays the open buffers into a
/// freshly-ready bundle on the loop (ports `ScalaLs.replayOpenBuffers`), and swaps
/// the state. `None` (a dropped/panicked worker) becomes a typed failure.
///
/// A result delivered after `shutdown` is DISCARDED: it must not resurrect a
/// shut-down server. The worker is detached (its handle dropped, not joined) so a
/// late result cannot delay teardown, and the shut-down `NotReady` state is kept.
fn adopt_bootstrap<S, B: Bootstrap<S>>(
    core: &mut ServerCore<S>,
    bootstrap: &B,
    result: Option<WorkspaceState<S>>,
) {
    if core.shutting_down {
        detach_bootstrap(core);
        return;
    }
    core.bootstrap_rx = None;
    if let Some(handle) = core.bootstrap_handle.take() {
        let _ = handle.join();
    }
    core.state = match result {
        Some(WorkspaceState::Ready(services)) => {
            bootstrap.replay(&services, &core.docs);
            WorkspaceState::Ready(services)
        }
        Some(other) => other,
        None => WorkspaceState::Failed {
            detail: "bootstrap worker terminated without a result".to_string(),
        },
    };
}

/// Stops tracking the in-flight bootstrap worker WITHOUT blocking on it: drops the
/// result channel and detaches the worker thread (it owns its data, and its `send`
/// fails harmlessly once the receiver is gone). Used when the server is shutting
/// down or exiting, so a late result neither resurrects the shut-down state nor
/// delays `serve` from returning.
fn detach_bootstrap<S>(core: &mut ServerCore<S>) {
    core.bootstrap_rx = None;
    core.bootstrap_handle = None;
}

/// Drains the build-targets-changed flag: when the build server has reported a
/// target change and the workspace is ready (and not shutting down), reload the
/// model on the message loop, reusing the durable handles. The flag is left set
/// while the workspace is not yet ready, so a change during bootstrap reloads on
/// the first ready turn (ports `ScalaLs.onBuildTargetsChanged`). The reload runs
/// here on the loop thread, so the ready services stay single-threaded.
fn poll_reload<S>(core: &mut ServerCore<S>, bootstrap: &impl Bootstrap<S>) {
    if core.shutting_down || !core.state.is_ready() {
        return;
    }
    if !core.reload_requested.swap(false, Ordering::SeqCst) {
        return;
    }
    let taken = std::mem::replace(
        &mut core.state,
        WorkspaceState::NotReady {
            detail: "reloading the build model".to_string(),
        },
    );
    core.state = match taken {
        WorkspaceState::Ready(old) => bootstrap.reload(old, &core.docs),
        other => other,
    };
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
/// renders in any state from the context; reindex/compile/pcPluginStatus run
/// through the services when ready and otherwise answer a typed "unavailable"
/// status string; an unknown command is an invalid-params error.
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
            Response::success(request.id.clone(), doctor_result(core, handlers, request))
        }
        Some(commands::REINDEX) if ready => ready_handle(core, handlers, request),
        Some(commands::REINDEX) => unavailable("reindex"),
        Some(commands::COMPILE) if ready => ready_handle(core, handlers, request),
        Some(commands::COMPILE) => unavailable("compile"),
        Some(commands::PC_PLUGIN_STATUS) if ready => ready_handle(core, handlers, request),
        // The Scala pre-ready answer: `pc plugin status unavailable: workspace
        // is <status>` (ready-but-cold is the services' typed cold answer).
        Some(commands::PC_PLUGIN_STATUS) => unavailable("pc plugin status"),
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

/// The `scala3SemanticLs.doctor` result (the Scala `DoctorCommand.report`).
/// Renders the full typed report in every state: a ready workspace gets the live
/// `BSP`/`SemanticDB`/`PC` sections from `Handlers::doctor`; any other state gets
/// the offline report (`Runtime`/`Nix`/`Store` populated, the live sections
/// `unavailable`). Text carries a `state:` header; the `{"json": true}` argument
/// returns the structured object with a `state` field. Boots no JVM.
fn doctor_result<S>(core: &ServerCore<S>, handlers: &impl Handlers<S>, request: &Request) -> Value {
    let root = core.workspace_root.clone();
    let report = core
        .state
        .ready()
        .and_then(|services| handlers.doctor(services, root.as_deref()))
        .unwrap_or_else(|| DoctorReport::offline(&doctor_root(root.as_deref())));
    let status = core.state.status_line();
    if doctor_json_requested(request) {
        let mut value = report.render_json();
        if let Value::Object(map) = &mut value {
            map.insert("state".to_string(), Value::String(status));
        }
        value
    } else {
        Value::String(format!("state: {status}\n\n{}", report.render_text()))
    }
}

/// The workspace root for an offline doctor report, defaulting to the current
/// directory when the server never received one (the Scala `Path.of(".")`).
fn doctor_root(workspace_root: Option<&Path>) -> PathBuf {
    workspace_root
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Whether the doctor executeCommand asked for JSON output — `arguments: [{
/// "json": true }]`.
fn doctor_json_requested(request: &Request) -> bool {
    request
        .params
        .get("arguments")
        .and_then(Value::as_array)
        .and_then(|args| args.first())
        .and_then(|arg| arg.get("json"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

fn dispatch_notification<S, B>(
    core: &mut ServerCore<S>,
    handlers: &impl Handlers<S>,
    bootstrap: &Arc<B>,
    note: Notification,
) -> Flow
where
    S: Send + 'static,
    B: Bootstrap<S> + Send + Sync + 'static,
{
    match note.method.as_str() {
        "initialized" => spawn_bootstrap(core, bootstrap),
        "exit" => return Flow::Stop,
        "textDocument/didOpen" => core.did_open(handlers, &note.params),
        "textDocument/didChange" => core.did_change(handlers, &note.params),
        "textDocument/didClose" => core.did_close(handlers, &note.params),
        "textDocument/didSave" => core.did_save(handlers, &note.params),
        // `workspace/didChangeConfiguration`: `params.settings` is ignored
        // (config.json is the single configuration source); the ready services
        // are only nudged to re-read it. Before ready there is nothing to nudge.
        "workspace/didChangeConfiguration" => {
            if let Some(services) = core.state.ready() {
                handlers.on_did_change_configuration(services);
            }
        }
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

fn document_version(params: &Value) -> Option<i64> {
    params.get("textDocument")?.get("version")?.as_i64()
}

/// The typed `contentChanges` event list (ranged and/or whole-document items).
/// `None` when the field is missing or any item fails to parse.
fn content_changes(params: &Value) -> Option<Vec<ContentChange>> {
    serde_json::from_value(params.get("contentChanges")?.clone()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
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
        fn build(&self, _workspace_root: Option<PathBuf>) -> WorkspaceState<FakeServices> {
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

    /// The method of a single framed message, for splitting scripted input.
    fn frame_method(frame: &[u8]) -> Option<String> {
        let mut reader = Cursor::new(frame.to_vec());
        let body = read_frame(&mut reader).ok()??;
        let value: Value = serde_json::from_slice(&body).ok()?;
        value.get("method")?.as_str().map(str::to_string)
    }

    /// Feeds one group of frames through `serve` on the given persistent `core`
    /// with the echo handlers, a fixed bootstrap outcome, and no-op hooks, and
    /// returns the responses written.
    fn serve_frames(
        core: &mut ServerCore<FakeServices>,
        frames: &[Vec<u8>],
        bootstrap: &WorkspaceState<FakeServices>,
    ) -> Vec<Value> {
        let mut reader = Cursor::new(frames.concat());
        let sink = OutputSink::new(Vec::new());
        serve(
            &mut reader,
            &sink,
            core,
            &EchoHandlers,
            FixedBootstrap(bootstrap.clone()),
        )
        .unwrap();
        responses(sink.written())
    }

    #[test]
    fn output_sink_publishes_diagnostics_as_a_notification_frame() {
        // The sink the BSP reader thread publishes through (independent of the
        // message loop) writes a well-formed `textDocument/publishDiagnostics`.
        let publish: PublishDiagnosticsParams = serde_json::from_value(json!({
            "uri": "file:///x.scala",
            "diagnostics": [{
                "range": {"start": {"line": 0, "character": 0}, "end": {"line": 0, "character": 4}},
                "severity": 1,
                "message": "boom",
            }],
        }))
        .unwrap();
        let sink = OutputSink::new(Vec::new());
        sink.publish_diagnostics(&publish).unwrap();

        let out = responses(sink.written());
        let published = out
            .iter()
            .find(|m| m["method"] == "textDocument/publishDiagnostics")
            .expect("a publishDiagnostics notification");
        assert_eq!(published["params"]["uri"], "file:///x.scala");
        assert_eq!(
            published["params"]["diagnostics"].as_array().unwrap().len(),
            1
        );
    }

    /// Drives `serve` over the scripted input, pumping to ready across the
    /// `initialized` boundary: a real client waits between `initialized` and its
    /// first ready-requiring request while the async bootstrap runs, so the input
    /// is fed in two `serve` passes split just after `initialized` (the first pass
    /// drains the worker at loop end, installing the ready state before the queries).
    fn run(
        input: Vec<Vec<u8>>,
        bootstrap: WorkspaceState<FakeServices>,
    ) -> (ServerCore<FakeServices>, Vec<Value>) {
        let mut core = ServerCore::new();
        let split = input
            .iter()
            .position(|f| frame_method(f).as_deref() == Some("initialized"))
            .map(|i| i + 1)
            .unwrap_or(input.len());
        let mut out = serve_frames(&mut core, &input[..split], &bootstrap);
        out.extend(serve_frames(&mut core, &input[split..], &bootstrap));
        (core, out)
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

    // The async bootstrap worker receives the owned workspace root (in `build`);
    // open-buffer replay on the loop receives the documents opened during the
    // pre-ready window (in `replay`, after Ready is installed).
    #[test]
    fn bootstrap_build_receives_root_and_replay_receives_documents() {
        #[derive(Clone)]
        struct RecordingBootstrap {
            replayed_docs: Arc<std::sync::atomic::AtomicUsize>,
        }
        impl Bootstrap<FakeServices> for RecordingBootstrap {
            fn build(&self, workspace_root: Option<PathBuf>) -> WorkspaceState<FakeServices> {
                WorkspaceState::Ready(FakeServices {
                    tag: format!(
                        "root={}",
                        workspace_root
                            .map(|p| p.display().to_string())
                            .unwrap_or_default()
                    ),
                })
            }
            fn replay(&self, _services: &FakeServices, documents: &Arc<DocumentStore>) {
                self.replayed_docs
                    .store(documents.open_uris().len(), Ordering::SeqCst);
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
                // A clean EOF (no `exit`) block-drains the worker to Ready, so replay
                // runs; an `exit` would instead detach the worker without adopting.
            ]
            .concat(),
        );
        let mut core = ServerCore::new();
        let replayed_docs = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let sink = OutputSink::new(Vec::new());
        serve(
            &mut reader,
            &sink,
            &mut core,
            &EchoHandlers,
            RecordingBootstrap {
                replayed_docs: Arc::clone(&replayed_docs),
            },
        )
        .unwrap();

        assert_eq!(core.state.ready().unwrap().tag, "root=/ws");
        assert_eq!(
            replayed_docs.load(Ordering::SeqCst),
            1,
            "replay did not observe the pre-ready open buffer"
        );
    }

    // poll_reload reloads only when the workspace is ready, not shutting down, and
    // a build-target change was flagged; a change flagged before ready stays
    // pending (drained on the first ready turn), and a shutting-down server never
    // reloads. Ports ScalaLs.onBuildTargetsChanged's gating.
    #[test]
    fn poll_reload_reloads_only_when_ready_and_requested() {
        struct ReloadingBootstrap;
        impl Bootstrap<FakeServices> for ReloadingBootstrap {
            fn build(&self, _workspace_root: Option<PathBuf>) -> WorkspaceState<FakeServices> {
                ready("initial")
            }
            fn reload(
                &self,
                _old: FakeServices,
                _documents: &Arc<DocumentStore>,
            ) -> WorkspaceState<FakeServices> {
                ready("reloaded")
            }
        }

        // Ready + flagged -> reload runs and clears the flag.
        let mut core: ServerCore<FakeServices> = ServerCore::new();
        core.state = ready("initial");
        core.reload_flag().store(true, Ordering::SeqCst);
        poll_reload(&mut core, &ReloadingBootstrap);
        assert_eq!(core.state.ready().unwrap().tag, "reloaded");
        assert!(!core.reload_requested.load(Ordering::SeqCst));

        // Not ready + flagged -> no reload; the flag stays set for the ready turn.
        let mut pending: ServerCore<FakeServices> = ServerCore::new();
        pending.reload_flag().store(true, Ordering::SeqCst);
        poll_reload(&mut pending, &ReloadingBootstrap);
        assert!(!pending.state.is_ready());
        assert!(pending.reload_requested.load(Ordering::SeqCst));

        // Shutting down + ready + flagged -> no reload.
        let mut down: ServerCore<FakeServices> = ServerCore::new();
        down.state = ready("initial");
        down.shutting_down = true;
        down.reload_flag().store(true, Ordering::SeqCst);
        poll_reload(&mut down, &ReloadingBootstrap);
        assert_eq!(down.state.ready().unwrap().tag, "initial");
    }

    fn references_request(id: i64) -> Request {
        Request {
            id: RequestId::Number(id),
            method: "textDocument/references".to_string(),
            params: json!({}),
        }
    }

    // The async bootstrap is adopted only when the worker actually delivers a
    // result: while its result channel is empty the workspace stays `NotReady`, so
    // a readiness-sensitive request gets the typed pre-ready fallback; once the
    // worker sends `Ready`, `poll_bootstrap` installs it (replaying the open
    // buffers) and the same request is served. Deterministic — the channel is fed
    // by hand, with no dependence on worker-thread timing.
    #[test]
    fn poll_bootstrap_adopts_the_workspace_only_when_the_worker_result_arrives() {
        let bootstrap = FixedBootstrap(ready("svc"));
        let mut core: ServerCore<FakeServices> = ServerCore::new();
        core.initialize(&json!({ "rootUri": "file:///ws" }));
        // Model a spawned worker whose result has not arrived yet.
        let (tx, rx) = mpsc::channel();
        core.bootstrap_rx = Some(rx);

        // Worker in flight: no result to adopt, so the workspace stays not-ready and
        // references answers the not-ready contract (an error), not a served result.
        poll_bootstrap(&mut core, &bootstrap);
        assert!(!core.state.is_ready(), "an empty channel must not adopt");
        assert!(core.bootstrap_rx.is_some(), "the channel must be retained");
        let pre = dispatch_request(&mut core, &EchoHandlers, references_request(1));
        assert!(
            pre.error.is_some(),
            "a pre-ready request must get the not-ready fallback, got {pre:?}"
        );

        // Worker completes: the result is adopted and the request is now served.
        tx.send(ready("svc")).unwrap();
        poll_bootstrap(&mut core, &bootstrap);
        assert!(
            core.state.is_ready(),
            "the delivered result must be adopted"
        );
        assert!(core.bootstrap_rx.is_none(), "the channel must be cleared");
        let post = dispatch_request(&mut core, &EchoHandlers, references_request(2));
        assert!(
            post.error.is_none(),
            "a ready request must be served, got {post:?}"
        );
        assert_eq!(post.result.unwrap()["services"], "svc");
    }

    // A worker that drops its sender without a result (a panicked build) is adopted
    // as a typed failure rather than leaving the workspace wedged forever.
    #[test]
    fn poll_bootstrap_adopts_a_dropped_worker_as_a_failure() {
        let bootstrap = FixedBootstrap(ready("svc"));
        let mut core: ServerCore<FakeServices> = ServerCore::new();
        let (tx, rx) = mpsc::channel::<WorkspaceState<FakeServices>>();
        core.bootstrap_rx = Some(rx);
        drop(tx);
        poll_bootstrap(&mut core, &bootstrap);
        assert!(matches!(core.state, WorkspaceState::Failed { .. }));
        assert!(core.bootstrap_rx.is_none());
    }

    // A bootstrap result delivered AFTER shutdown must not resurrect the server: the
    // workspace stays shut down and the worker is detached. Deterministic — the
    // channel is fed by hand.
    #[test]
    fn poll_bootstrap_discards_a_late_ready_after_shutdown() {
        let bootstrap = FixedBootstrap(ready("svc"));
        let mut core: ServerCore<FakeServices> = ServerCore::new();
        core.initialize(&json!({ "rootUri": "file:///ws" }));
        let (tx, rx) = mpsc::channel();
        core.bootstrap_rx = Some(rx);

        // Shutdown arrives while the worker is in flight.
        core.shutdown();
        assert!(core.shutting_down);
        // The worker then delivers Ready — it must NOT overwrite the shutdown state.
        tx.send(ready("svc")).unwrap();
        poll_bootstrap(&mut core, &bootstrap);

        assert!(
            !core.state.is_ready(),
            "a late Ready must not resurrect shutdown"
        );
        match &core.state {
            WorkspaceState::NotReady { detail } => assert_eq!(detail, "server is shut down"),
            other => panic!("expected shut-down NotReady, got {:?}", other.status_line()),
        }
        assert!(
            core.bootstrap_rx.is_none(),
            "the worker is detached after shutdown"
        );
    }

    // `shutdown` then `exit` while the bootstrap worker is still in flight: `serve`
    // must return WITHOUT blocking on the worker and must NOT install Ready over the
    // shut-down state. The worker is gated so it is genuinely in flight at exit — if
    // `serve` block-drained, this test would deadlock (only the test releases the
    // gate, after `serve` returns).
    #[test]
    fn serve_neither_resurrects_nor_blocks_on_shutdown_then_exit_during_bootstrap() {
        struct GatedBootstrap {
            gate: Arc<std::sync::Barrier>,
            outcome: WorkspaceState<FakeServices>,
        }
        impl Bootstrap<FakeServices> for GatedBootstrap {
            fn build(&self, _workspace_root: Option<PathBuf>) -> WorkspaceState<FakeServices> {
                self.gate.wait();
                self.outcome.clone()
            }
        }

        let gate = Arc::new(std::sync::Barrier::new(2));
        let bootstrap = GatedBootstrap {
            gate: Arc::clone(&gate),
            outcome: ready("svc"),
        };
        let mut reader = Cursor::new(
            [
                frame(request(1, "initialize", json!({ "rootUri": "file:///ws" }))),
                frame(notification("initialized", json!({}))),
                frame(request(2, "shutdown", json!({}))),
                frame(notification("exit", json!({}))),
            ]
            .concat(),
        );
        let mut core = ServerCore::new();
        let sink = OutputSink::new(Vec::new());
        // Returns promptly (no block on the gated worker).
        serve(&mut reader, &sink, &mut core, &EchoHandlers, bootstrap).unwrap();

        assert!(
            !core.state.is_ready(),
            "a late Ready must not resurrect shutdown"
        );
        match &core.state {
            WorkspaceState::NotReady { detail } => assert_eq!(detail, "server is shut down"),
            other => panic!("expected shut-down NotReady, got {:?}", other.status_line()),
        }
        // The shutdown response was served; no request was answered as ready.
        let out = responses(sink.written());
        assert!(out
            .iter()
            .any(|r| r["id"] == 2 && r["result"] == Value::Null));
        // Release the worker so its thread finishes (its send fails harmlessly).
        gate.wait();
    }

    // A clean EOF with no shutdown/exit still block-drains the worker to Ready, so
    // the pump-until-ready path is preserved.
    #[test]
    fn serve_reaches_ready_on_clean_eof_without_shutdown_or_exit() {
        let mut reader = Cursor::new(
            [
                frame(request(1, "initialize", json!({ "rootUri": "file:///ws" }))),
                frame(notification("initialized", json!({}))),
            ]
            .concat(),
        );
        let mut core = ServerCore::new();
        let sink = OutputSink::new(Vec::new());
        serve(
            &mut reader,
            &sink,
            &mut core,
            &EchoHandlers,
            FixedBootstrap(ready("svc")),
        )
        .unwrap();
        assert!(
            core.state.is_ready(),
            "a clean EOF after initialized must drain to Ready"
        );
        assert_eq!(core.state.ready().unwrap().tag, "svc");
    }

    // While the bootstrap worker is in flight (bootstrap_rx present but empty), the
    // workspace is NotReady, so every readiness-sensitive request returns its exact
    // per-method pre-ready fallback. Pins the whole surface so a future change
    // cannot silently regress one method's fallback.
    #[test]
    fn pre_ready_in_flight_bootstrap_serves_every_method_fallback() {
        #[derive(Debug)]
        enum Shape {
            Error,
            Null,
            Empty(Value),
        }
        let cases = [
            ("textDocument/references", Shape::Error),
            ("textDocument/rename", Shape::Error),
            ("textDocument/documentHighlight", Shape::Empty(json!([]))),
            ("workspace/symbol", Shape::Empty(json!([]))),
            ("textDocument/definition", Shape::Empty(json!([]))),
            ("textDocument/typeDefinition", Shape::Empty(json!([]))),
            (
                "textDocument/completion",
                Shape::Empty(json!({ "isIncomplete": false, "items": [] })),
            ),
            ("textDocument/prepareRename", Shape::Null),
            ("textDocument/hover", Shape::Null),
            ("textDocument/signatureHelp", Shape::Null),
        ];
        for (method, expected) in cases {
            let bootstrap = FixedBootstrap(ready("svc"));
            let mut core: ServerCore<FakeServices> = ServerCore::new();
            core.initialize(&json!({ "rootUri": "file:///ws" }));
            // Worker in flight: the sender is held (channel stays connected, empty).
            let (_tx, rx) = mpsc::channel::<WorkspaceState<FakeServices>>();
            core.bootstrap_rx = Some(rx);
            poll_bootstrap(&mut core, &bootstrap);
            assert!(
                !core.state.is_ready(),
                "{method}: worker in flight is not ready"
            );

            let resp = dispatch_request(
                &mut core,
                &EchoHandlers,
                Request {
                    id: RequestId::Number(1),
                    method: method.to_string(),
                    params: json!({}),
                },
            );
            match expected {
                Shape::Error => assert!(
                    resp.error.is_some(),
                    "{method}: expected a not-ready error, got {resp:?}"
                ),
                Shape::Null => {
                    assert!(resp.error.is_none(), "{method}: unexpected error {resp:?}");
                    assert_eq!(resp.result, Some(Value::Null), "{method}: expected null");
                }
                Shape::Empty(value) => {
                    assert!(resp.error.is_none(), "{method}: unexpected error {resp:?}");
                    assert_eq!(resp.result, Some(value), "{method}: wrong empty shape");
                }
            }
        }
    }

    // Doctor renders the state header plus the full typed report in every state.
    // `FakeServices` has no live `doctor` hook, so every state renders the offline
    // report — all seven headings, live-only sections `unavailable`.
    #[test]
    fn doctor_renders_the_state_header_and_all_sections_in_every_state() {
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
        let assert_report = |report: &str, expected_state: &str| {
            assert!(
                report.starts_with(&format!("state: {expected_state}\n\n")),
                "{report}"
            );
            for heading in [
                "Runtime:",
                "Nix:",
                "BSP:",
                "SemanticDB:",
                "Store:",
                "PC:",
                "PC Plugins:",
            ] {
                assert!(report.contains(heading), "missing {heading} in {report}");
            }
            // Live-only sections are unavailable (no ready CoreServices bundle).
            assert!(
                report.contains("BSP:\n  unavailable: no BSP connection"),
                "{report}"
            );
        };
        assert_report(
            &doctor(ready("unused"), false),
            "not ready: waiting for the initialized notification",
        );
        assert_report(&doctor(ready("svc"), true), "ready");
        assert_report(
            &doctor(
                WorkspaceState::Failed {
                    detail: "no build server".to_string(),
                },
                true,
            ),
            "bootstrap failed: no build server",
        );
    }

    // The `{"json": true}` argument returns the structured report with a `state`
    // field and a `store` key (no `sqlite`/`postings`).
    #[test]
    fn doctor_json_argument_returns_the_structured_report() {
        let input = vec![
            frame(request(1, "initialize", json!({}))),
            frame(request(
                2,
                "workspace/executeCommand",
                json!({ "command": "scala3SemanticLs.doctor", "arguments": [{ "json": true }] }),
            )),
            frame(notification("exit", json!({}))),
        ];
        let (_core, out) = run(input, ready("svc"));
        let result = &out[1]["result"];
        assert!(result.is_object(), "json result is an object: {result}");
        assert!(result.get("store").is_some());
        assert!(result.get("sqlite").is_none());
        assert_eq!(
            result["state"],
            "not ready: waiting for the initialized notification"
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
                // pcPluginStatus pre-ready: the typed unavailable status answer.
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
        assert_eq!(
            out[4]["result"],
            "pc plugin status unavailable: workspace is not ready: \
             waiting for the initialized notification"
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
    fn did_change_folds_the_event_list_and_did_close_drops_the_buffer() {
        let core: ServerCore<FakeServices> = ServerCore::new();
        core.did_open(
            &EchoHandlers,
            &json!({ "textDocument": { "uri": "file:///a", "version": 1, "text": "v1" } }),
        );
        assert_eq!(core.docs.version("file:///a"), Some(1));
        // A rangeless full replace followed by a ranged event addressing the
        // replaced text — the fold applies them in order.
        core.did_change(
            &EchoHandlers,
            &json!({
                "textDocument": { "uri": "file:///a", "version": 2 },
                "contentChanges": [
                    { "text": "stale" },
                    { "text": "v2" },
                    {
                        "range": {
                            "start": { "line": 0, "character": 2 },
                            "end": { "line": 0, "character": 2 }
                        },
                        "text": ".1"
                    }
                ]
            }),
        );
        assert_eq!(core.docs.text("file:///a").as_deref(), Some("v2.1"));
        assert_eq!(core.docs.version("file:///a"), Some(2));
        core.did_close(
            &EchoHandlers,
            &json!({ "textDocument": { "uri": "file:///a" } }),
        );
        assert!(!core.docs.is_open("file:///a"));
    }

    // A ranged didChange for a buffer that was never opened has no base text to
    // edit: it is dropped (with a stderr log), never mis-applied.
    #[test]
    fn a_ranged_did_change_for_a_never_opened_buffer_is_dropped() {
        let core: ServerCore<FakeServices> = ServerCore::new();
        core.did_change(
            &EchoHandlers,
            &json!({
                "textDocument": { "uri": "file:///never-opened", "version": 1 },
                "contentChanges": [{
                    "range": {
                        "start": { "line": 0, "character": 0 },
                        "end": { "line": 0, "character": 0 }
                    },
                    "text": "X"
                }]
            }),
        );
        assert!(!core.docs.is_open("file:///never-opened"));
    }

    #[test]
    fn did_save_with_text_refreshes_an_open_buffer_only() {
        let core: ServerCore<FakeServices> = ServerCore::new();
        core.did_save(
            &EchoHandlers,
            &json!({ "textDocument": { "uri": "file:///a" }, "text": "saved" }),
        );
        assert!(!core.docs.is_open("file:///a"));
        core.did_open(
            &EchoHandlers,
            &json!({ "textDocument": { "uri": "file:///a", "text": "v1" } }),
        );
        core.did_save(
            &EchoHandlers,
            &json!({ "textDocument": { "uri": "file:///a" }, "text": "saved" }),
        );
        assert_eq!(core.docs.text("file:///a").as_deref(), Some("saved"));
    }

    /// The document-notification seam: when the workspace is ready, `didOpen`/
    /// `didChange`/`didClose` forward to the handlers' lifecycle hooks with the
    /// normalized URI (so the PC buffer mirror stays in sync); before ready they
    /// only sync the document store and the hooks are NOT invoked.
    #[test]
    fn document_notifications_forward_to_the_lifecycle_hooks_when_ready() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct RecordingHandlers {
            events: Mutex<Vec<String>>,
        }
        impl Handlers<FakeServices> for RecordingHandlers {
            fn handle(&self, cx: RequestContext<'_, FakeServices>) -> Response {
                Response::success(cx.request.id.clone(), Value::Null)
            }
            fn on_did_open(&self, services: &FakeServices, uri: &str, text: &str) {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("open {} {uri} {text}", services.tag));
            }
            fn on_did_change(&self, _services: &FakeServices, uri: &str, text: &str) {
                self.events
                    .lock()
                    .unwrap()
                    .push(format!("change {uri} {text}"));
            }
            fn on_did_close(&self, _services: &FakeServices, uri: &str) {
                self.events.lock().unwrap().push(format!("close {uri}"));
            }
        }

        let drive = |ready_state: bool| {
            let bootstrap_state = if ready_state {
                ready("svc")
            } else {
                WorkspaceState::Failed {
                    detail: "no build server".to_string(),
                }
            };
            let mut core = ServerCore::new();
            let handlers = RecordingHandlers::default();
            // Pass 1 settles the workspace across the `initialized` boundary; pass 2
            // runs the document lifecycle once ready/failed is installed (the async
            // bootstrap makes readiness observable only after the worker is drained).
            let pre = [
                frame(request(1, "initialize", json!({ "rootUri": "file:///ws" }))),
                frame(notification("initialized", json!({}))),
            ]
            .concat();
            let post = [
                frame(notification(
                    "textDocument/didOpen",
                    // The `..` segment must be normalized away before the hook.
                    json!({ "textDocument": { "uri": "file:///ws/x/../a.scala", "text": "v1" } }),
                )),
                frame(notification(
                    "textDocument/didChange",
                    json!({
                        "textDocument": { "uri": "file:///ws/a.scala" },
                        "contentChanges": [ { "text": "v2" } ]
                    }),
                )),
                frame(notification(
                    "textDocument/didClose",
                    json!({ "textDocument": { "uri": "file:///ws/a.scala" } }),
                )),
                frame(notification("exit", json!({}))),
            ]
            .concat();
            for group in [pre, post] {
                let mut reader = Cursor::new(group);
                let sink = OutputSink::new(Vec::new());
                serve(
                    &mut reader,
                    &sink,
                    &mut core,
                    &handlers,
                    FixedBootstrap(bootstrap_state.clone()),
                )
                .unwrap();
            }
            handlers.events.into_inner().unwrap()
        };

        // Ready: the hooks fire in order with the normalized URI and the services.
        assert_eq!(
            drive(true),
            vec![
                "open svc file:///ws/a.scala v1".to_string(),
                "change file:///ws/a.scala v2".to_string(),
                "close file:///ws/a.scala".to_string(),
            ]
        );
        // Not ready (failed bootstrap): the document store still syncs, but no
        // hook is invoked.
        assert!(drive(false).is_empty());
    }

    // An inbound client RESPONSE frame (id + result/error, no method) is consumed
    // and dropped: no error frame is written back, and the loop continues to serve
    // the following messages. Base-protocol tolerance, and the prerequisite for
    // dynamic registration.
    #[test]
    fn a_client_response_frame_is_consumed_without_an_error_frame() {
        let (core, out) = run(
            vec![
                frame(request(1, "initialize", json!({ "rootUri": "file:///ws" }))),
                frame(json!({ "jsonrpc": "2.0", "id": 99, "result": { "ok": true } })),
                frame(json!({
                    "jsonrpc": "2.0",
                    "id": 100,
                    "error": { "code": -32601, "message": "nope" }
                })),
                frame(request(2, "shutdown", json!({}))),
                frame(notification("exit", json!({}))),
            ],
            ready("unused"),
        );
        // Exactly the two request responses — no null-id error frame for the
        // inbound responses — and the loop reached the later messages.
        assert_eq!(out.len(), 2, "{out:?}");
        assert_eq!(out[0]["id"], 1);
        assert_eq!(out[1]["id"], 2);
        assert!(out.iter().all(|m| m.get("error").is_none()), "{out:?}");
        assert!(core.shutting_down, "the loop continued past the responses");
    }

    // `workspace/didChangeConfiguration` reaches the handlers' hook only when the
    // workspace is ready; the settings payload is ignored upstream of the hook
    // (config.json stays the single configuration source).
    #[test]
    fn did_change_configuration_forwards_to_the_hook_only_when_ready() {
        use std::sync::Mutex;

        #[derive(Default)]
        struct ConfigRecordingHandlers {
            calls: Mutex<Vec<String>>,
        }
        impl Handlers<FakeServices> for ConfigRecordingHandlers {
            fn handle(&self, cx: RequestContext<'_, FakeServices>) -> Response {
                Response::success(cx.request.id.clone(), Value::Null)
            }
            fn on_did_change_configuration(&self, services: &FakeServices) {
                self.calls.lock().unwrap().push(services.tag.clone());
            }
        }

        let drive = |ready_state: bool| {
            let bootstrap_state = if ready_state {
                ready("svc")
            } else {
                WorkspaceState::Failed {
                    detail: "no build server".to_string(),
                }
            };
            let mut core = ServerCore::new();
            let handlers = ConfigRecordingHandlers::default();
            let pre = [
                frame(request(1, "initialize", json!({ "rootUri": "file:///ws" }))),
                frame(notification("initialized", json!({}))),
            ]
            .concat();
            let post = [
                frame(notification(
                    "workspace/didChangeConfiguration",
                    json!({ "settings": { "ignored": true } }),
                )),
                frame(notification("exit", json!({}))),
            ]
            .concat();
            for group in [pre, post] {
                let mut reader = Cursor::new(group);
                let sink = OutputSink::new(Vec::new());
                serve(
                    &mut reader,
                    &sink,
                    &mut core,
                    &handlers,
                    FixedBootstrap(bootstrap_state.clone()),
                )
                .unwrap();
            }
            handlers.calls.into_inner().unwrap()
        };

        // Ready: the hook fires once with the ready services.
        assert_eq!(drive(true), vec!["svc".to_string()]);
        // Not ready (failed bootstrap): the notification is ignored.
        assert!(drive(false).is_empty());
    }
}
