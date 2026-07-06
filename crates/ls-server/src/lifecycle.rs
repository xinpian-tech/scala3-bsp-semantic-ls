//! The workspace lifecycle state and the per-method pre-ready policy.
//!
//! `initialize` returns synchronously as [`WorkspaceState::NotReady`];
//! `initialized` spawns the asynchronous bootstrap, which transitions to
//! [`WorkspaceState::Ready`] or [`WorkspaceState::Failed`]. Until the workspace
//! is ready, each request answers a fixed per-method fallback — a typed
//! not-ready error, an empty result, or a null response — never a crash or a
//! guessed answer.

/// The workspace lifecycle state. The wired services attach to `Ready` in the
/// transport layer; the state's observable contract here is its status line and
/// its readiness.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceState {
    NotReady { detail: String },
    Failed { detail: String },
    Ready,
}

impl WorkspaceState {
    /// The human-readable status embedded in not-ready errors and the doctor
    /// report.
    pub fn status_line(&self) -> String {
        match self {
            WorkspaceState::NotReady { detail } => format!("not ready: {detail}"),
            WorkspaceState::Failed { detail } => format!("bootstrap failed: {detail}"),
            WorkspaceState::Ready => "ready".to_string(),
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, WorkspaceState::Ready)
    }
}

/// The typed error references and rename return before the workspace is ready: a
/// request failure carrying the current status, never an empty or crashing
/// response.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NotReadyError {
    pub message: String,
}

/// The readiness gate references and rename use: succeeds only when the
/// workspace is ready, otherwise the typed not-ready error.
pub fn require_ready(state: &WorkspaceState) -> Result<(), NotReadyError> {
    if state.is_ready() {
        Ok(())
    } else {
        Err(NotReadyError {
            message: format!("workspace is {}", state.status_line()),
        })
    }
}

/// A readiness-sensitive LSP method.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    References,
    Rename,
    DocumentHighlight,
    WorkspaceSymbol,
    Completion,
    Definition,
    TypeDefinition,
    Hover,
    SignatureHelp,
    PrepareRename,
}

/// What a method answers before the workspace is ready.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PreReadyOutcome {
    /// Fail with the typed "workspace is `<status>`" error.
    NotReadyError,
    /// An empty result (an empty location/symbol/highlight list, or an empty
    /// completion list).
    Empty,
    /// A null response.
    Null,
}

/// The per-method pre-ready fallback, matching the server's dispatch: references
/// and rename fail typed; document highlight, workspace symbol, completion,
/// definition, and type definition answer empty; hover, signature help, and
/// prepare rename answer null.
pub fn pre_ready_outcome(method: Method) -> PreReadyOutcome {
    match method {
        Method::References | Method::Rename => PreReadyOutcome::NotReadyError,
        Method::DocumentHighlight
        | Method::WorkspaceSymbol
        | Method::Completion
        | Method::Definition
        | Method::TypeDefinition => PreReadyOutcome::Empty,
        Method::Hover | Method::SignatureHelp | Method::PrepareRename => PreReadyOutcome::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Mirrors ls.core.WorkspaceState.statusLine.
    #[test]
    fn status_line_renders_each_state() {
        assert_eq!(
            WorkspaceState::NotReady {
                detail: "waiting for the initialized notification".to_string()
            }
            .status_line(),
            "not ready: waiting for the initialized notification"
        );
        assert_eq!(
            WorkspaceState::Failed {
                detail: "boom".to_string()
            }
            .status_line(),
            "bootstrap failed: boom"
        );
        assert_eq!(WorkspaceState::Ready.status_line(), "ready");
    }

    #[test]
    fn only_ready_is_ready() {
        assert!(WorkspaceState::Ready.is_ready());
        assert!(!WorkspaceState::NotReady {
            detail: "x".to_string()
        }
        .is_ready());
        assert!(!WorkspaceState::Failed {
            detail: "x".to_string()
        }
        .is_ready());
    }

    // Mirrors ls.core.ScalaLs.requireReady.
    #[test]
    fn require_ready_passes_only_when_ready_and_carries_the_status() {
        assert_eq!(require_ready(&WorkspaceState::Ready), Ok(()));
        let err = require_ready(&WorkspaceState::NotReady {
            detail: "waiting".to_string(),
        })
        .unwrap_err();
        assert_eq!(err.message, "workspace is not ready: waiting");
        let failed = require_ready(&WorkspaceState::Failed {
            detail: "boom".to_string(),
        })
        .unwrap_err();
        assert_eq!(failed.message, "workspace is bootstrap failed: boom");
    }

    // Mirrors the per-method pre-ready dispatch in ls.core.ScalaLs.
    #[test]
    fn references_and_rename_are_typed_not_ready_errors() {
        assert_eq!(
            pre_ready_outcome(Method::References),
            PreReadyOutcome::NotReadyError
        );
        assert_eq!(
            pre_ready_outcome(Method::Rename),
            PreReadyOutcome::NotReadyError
        );
    }

    #[test]
    fn list_producing_methods_are_empty_before_ready() {
        for m in [
            Method::DocumentHighlight,
            Method::WorkspaceSymbol,
            Method::Completion,
            Method::Definition,
            Method::TypeDefinition,
        ] {
            assert_eq!(pre_ready_outcome(m), PreReadyOutcome::Empty, "{m:?}");
        }
    }

    #[test]
    fn nullable_methods_are_null_before_ready() {
        for m in [Method::Hover, Method::SignatureHelp, Method::PrepareRename] {
            assert_eq!(pre_ready_outcome(m), PreReadyOutcome::Null, "{m:?}");
        }
    }
}
