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
                // The build server signals "targets changed" here; lsp4j/Gson fires
                // the handler regardless of the params shape (missing/null fields
                // tolerated). A change payload serde would reject (an event missing
                // `target`, or null/absent params) is still a valid reload signal,
                // so fall back to an empty event rather than dropping the reload.
                let event = serde_json::from_value(note.params).unwrap_or_default();
                h(event);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn didchange(params: serde_json::Value) -> Notification {
        Notification {
            method: "buildTarget/didChange".to_string(),
            params,
        }
    }

    // The build server's `buildTarget/didChange` fires the reload handler no matter
    // the params shape (lsp4j/Gson tolerance parity): a change event missing
    // `target`, an empty object, or null/absent params are all still a valid
    // "targets changed" signal, not a dropped notification.
    #[test]
    fn buildtarget_didchange_fires_the_handler_even_on_a_malformed_payload() {
        let count = Arc::new(AtomicUsize::new(0));
        let seen = count.clone();
        let handlers = BspClientHandlers::new().on_did_change_build_target(move |_| {
            seen.fetch_add(1, Ordering::SeqCst);
        });

        dispatch_notification(&handlers, didchange(json!({"changes": [{}]})));
        dispatch_notification(&handlers, didchange(json!({})));
        dispatch_notification(&handlers, didchange(serde_json::Value::Null));
        assert_eq!(
            count.load(Ordering::SeqCst),
            3,
            "every didChange must fire the reload handler, even a malformed one"
        );
    }
}
