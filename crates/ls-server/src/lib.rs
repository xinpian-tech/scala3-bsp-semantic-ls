//! The LSP server layer: command-line entry, the advertised capability surface,
//! the workspace lifecycle state machine with its per-method pre-ready policy,
//! the BSP diagnostics router, and the dirty-buffer document store. A
//! behavior-preserving port of the `ls.core` server, wired over the Rust engine,
//! BSP client, and PC-island boundary crates.

pub mod capabilities;
pub mod cli;
pub mod diagnostics;
pub mod documents;
pub mod lifecycle;
pub mod protocol;

pub use capabilities::{
    initialize_result, server_capabilities, InitializeResult, ServerCapabilities,
};
pub use cli::{parse_args, resolve_doctor_dir, CliAction};
pub use diagnostics::{to_lsp_diagnostic, DiagnosticRouter};
pub use documents::DocumentStore;
pub use lifecycle::{
    pre_ready_outcome, require_ready, Method, NotReadyError, PreReadyOutcome, WorkspaceState,
};
pub use protocol::{Diagnostic, DiagnosticCode, Position, PublishDiagnosticsParams, Range};
