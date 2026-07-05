//! Client-side handling of server-originated messages. `BspClientHandlers`
//! collects the callbacks a caller cares about (diagnostics, log/show messages,
//! target-change events, server stderr); every callback defaults to a no-op.
//! `dispatch_notification` routes a decoded notification to the right one.

use std::sync::Arc;

use crate::jsonrpc::Notification;
use crate::protocol::{
    DidChangeBuildTarget, LogMessageParams, PublishDiagnosticsParams, ShowMessageParams,
};

type Handler<T> = Arc<dyn Fn(T) + Send + Sync>;

/// Callbacks for asynchronous server messages. Unset handlers drop their event.
/// Notification methods without a handler here (task start/progress/finish,
/// run stdout) are intentionally ignored.
#[derive(Clone, Default)]
pub struct BspClientHandlers {
    pub(crate) on_diagnostics: Option<Handler<PublishDiagnosticsParams>>,
    pub(crate) on_log_message: Option<Handler<LogMessageParams>>,
    pub(crate) on_show_message: Option<Handler<ShowMessageParams>>,
    pub(crate) on_did_change_build_target: Option<Handler<DidChangeBuildTarget>>,
    pub(crate) on_server_stderr: Option<Handler<String>>,
}

impl BspClientHandlers {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn on_diagnostics(
        mut self,
        f: impl Fn(PublishDiagnosticsParams) + Send + Sync + 'static,
    ) -> Self {
        self.on_diagnostics = Some(Arc::new(f));
        self
    }

    pub fn on_log_message(mut self, f: impl Fn(LogMessageParams) + Send + Sync + 'static) -> Self {
        self.on_log_message = Some(Arc::new(f));
        self
    }

    pub fn on_show_message(
        mut self,
        f: impl Fn(ShowMessageParams) + Send + Sync + 'static,
    ) -> Self {
        self.on_show_message = Some(Arc::new(f));
        self
    }

    pub fn on_did_change_build_target(
        mut self,
        f: impl Fn(DidChangeBuildTarget) + Send + Sync + 'static,
    ) -> Self {
        self.on_did_change_build_target = Some(Arc::new(f));
        self
    }

    pub fn on_server_stderr(mut self, f: impl Fn(String) + Send + Sync + 'static) -> Self {
        self.on_server_stderr = Some(Arc::new(f));
        self
    }

    pub(crate) fn emit_stderr(&self, line: String) {
        if let Some(h) = &self.on_server_stderr {
            h(line);
        }
    }
}

/// Decodes a notification's params and invokes the matching handler. Unknown
/// methods and payloads that fail to decode are dropped silently, mirroring a
/// forwarding client that must never crash the session on a stray message.
pub(crate) fn dispatch_notification(handlers: &BspClientHandlers, note: Notification) {
    match note.method.as_str() {
        "build/publishDiagnostics" => {
            if let Some(h) = &handlers.on_diagnostics {
                if let Ok(p) = serde_json::from_value(note.params) {
                    h(p);
                }
            }
        }
        "build/logMessage" => {
            if let Some(h) = &handlers.on_log_message {
                if let Ok(p) = serde_json::from_value(note.params) {
                    h(p);
                }
            }
        }
        "build/showMessage" => {
            if let Some(h) = &handlers.on_show_message {
                if let Ok(p) = serde_json::from_value(note.params) {
                    h(p);
                }
            }
        }
        "buildTarget/didChange" => {
            if let Some(h) = &handlers.on_did_change_build_target {
                if let Ok(p) = serde_json::from_value(note.params) {
                    h(p);
                }
            }
        }
        _ => {}
    }
}
