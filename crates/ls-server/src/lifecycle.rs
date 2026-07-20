//! The workspace lifecycle state and the per-method pre-ready policy.
//!
//! `initialize` returns synchronously as [`WorkspaceState::NotReady`];
//! `initialized` runs the bootstrap, which transitions to
//! [`WorkspaceState::Ready`] (owning the ready services) or
//! [`WorkspaceState::Failed`]. Until the workspace is ready, each request
//! answers a fixed per-method fallback — a typed not-ready error, an empty
//! result, or a null response — never a crash or a guessed answer.

/// The workspace lifecycle state. `Ready` owns the ready-services bundle `S`
/// (the aggregate of the engine/BSP/PC services, the `CoreServices` equivalent),
/// so the ready-path request and command handlers reach it directly rather than
/// keeping a second copy of server state.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkspaceState<S> {
    NotReady { detail: String },
    Failed { detail: String },
    Ready(S),
}

impl<S> WorkspaceState<S> {
    /// The human-readable status embedded in not-ready errors and the doctor
    /// report.
    pub fn status_line(&self) -> String {
        match self {
            WorkspaceState::NotReady { detail } => format!("not ready: {detail}"),
            WorkspaceState::Failed { detail } => format!("bootstrap failed: {detail}"),
            WorkspaceState::Ready(_) => "ready".to_string(),
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, WorkspaceState::Ready(_))
    }

    /// The ready services, or `None` before the workspace is ready.
    pub fn ready(&self) -> Option<&S> {
        match self {
            WorkspaceState::Ready(services) => Some(services),
            _ => None,
        }
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
pub fn require_ready<S>(state: &WorkspaceState<S>) -> Result<(), NotReadyError> {
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
    InlayHint,
    CodeAction,
    SelectionRange,
    FoldingRange,
    SemanticTokensFull,
    SemanticTokensRange,
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
/// definition, type definition, inlay hint, code action, and folding range
/// answer empty;
/// hover, signature help, prepare rename, selection range, and the two
/// semantic-tokens methods answer null (selection range because the spec ties
/// `result[i]` to `positions[i]` — an empty array against a non-empty position
/// list would break that correspondence, exactly as its ready-path gate
/// fallback answers null; semantic tokens because the spec result is
/// `SemanticTokens | null` and null — "no answer yet" — lets the client keep
/// whatever highlighting it has instead of wiping it with an empty stream).
pub fn pre_ready_outcome(method: Method) -> PreReadyOutcome {
    match method {
        Method::References | Method::Rename => PreReadyOutcome::NotReadyError,
        Method::DocumentHighlight
        | Method::WorkspaceSymbol
        | Method::Completion
        | Method::Definition
        | Method::TypeDefinition
        | Method::InlayHint
        | Method::CodeAction
        | Method::FoldingRange => PreReadyOutcome::Empty,
        Method::Hover
        | Method::SignatureHelp
        | Method::PrepareRename
        | Method::SelectionRange
        | Method::SemanticTokensFull
        | Method::SemanticTokensRange => PreReadyOutcome::Null,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The ready-services bundle is irrelevant to these status/policy tests, so
    // they instantiate the state over the unit type.
    type State = WorkspaceState<()>;

    // Mirrors ls.core.WorkspaceState.statusLine.
    #[test]
    fn status_line_renders_each_state() {
        assert_eq!(
            State::NotReady {
                detail: "waiting for the initialized notification".to_string()
            }
            .status_line(),
            "not ready: waiting for the initialized notification"
        );
        assert_eq!(
            State::Failed {
                detail: "boom".to_string()
            }
            .status_line(),
            "bootstrap failed: boom"
        );
        assert_eq!(State::Ready(()).status_line(), "ready");
    }

    #[test]
    fn only_ready_is_ready_and_exposes_services() {
        let ready = WorkspaceState::Ready("services");
        assert!(ready.is_ready());
        assert_eq!(ready.ready(), Some(&"services"));
        assert!(!State::NotReady {
            detail: "x".to_string()
        }
        .is_ready());
        assert_eq!(
            State::Failed {
                detail: "x".to_string()
            }
            .ready(),
            None
        );
    }

    // Mirrors ls.core.ScalaLs.requireReady.
    #[test]
    fn require_ready_passes_only_when_ready_and_carries_the_status() {
        assert_eq!(require_ready(&State::Ready(())), Ok(()));
        let err = require_ready(&State::NotReady {
            detail: "waiting".to_string(),
        })
        .unwrap_err();
        assert_eq!(err.message, "workspace is not ready: waiting");
        let failed = require_ready(&State::Failed {
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
            Method::InlayHint,
            Method::CodeAction,
            Method::FoldingRange,
        ] {
            assert_eq!(pre_ready_outcome(m), PreReadyOutcome::Empty, "{m:?}");
        }
    }

    #[test]
    fn nullable_methods_are_null_before_ready() {
        for m in [
            Method::Hover,
            Method::SignatureHelp,
            Method::PrepareRename,
            Method::SelectionRange,
            Method::SemanticTokensFull,
            Method::SemanticTokensRange,
        ] {
            assert_eq!(pre_ready_outcome(m), PreReadyOutcome::Null, "{m:?}");
        }
    }
}
