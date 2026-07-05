//! The LSP server layer: the advertised capability surface, the BSP diagnostics
//! router, and the dirty-buffer document store. A behavior-preserving port of
//! the `ls.core` server, wired over the Rust engine, BSP client, and PC-island
//! boundary crates. This first slice carries the protocol-facing pure logic; the
//! transport loop, bootstrap state machine, execute-command handlers, doctor,
//! and CLI land in subsequent slices.

pub mod capabilities;
pub mod diagnostics;
pub mod documents;
pub mod protocol;

pub use capabilities::{
    initialize_result, server_capabilities, InitializeResult, ServerCapabilities,
};
pub use diagnostics::{to_lsp_diagnostic, DiagnosticRouter};
pub use documents::DocumentStore;
pub use protocol::{Diagnostic, DiagnosticCode, Position, PublishDiagnosticsParams, Range};
