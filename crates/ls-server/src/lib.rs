//! The LSP server layer: command-line entry, the advertised capability surface,
//! the workspace lifecycle state machine with its per-method pre-ready policy,
//! the BSP diagnostics router, and the dirty-buffer document store. A
//! behavior-preserving port of the `ls.core` server, wired over the Rust engine,
//! BSP client, and PC-island boundary crates.

pub mod bootstrap;
pub mod capabilities;
pub mod cli;
pub mod convert;
pub mod diagnostics;
pub mod documents;
pub mod jsonrpc;
pub mod lifecycle;
pub mod protocol;
pub mod server;
pub mod services;
pub mod workspace_uris;

pub use bootstrap::{
    from_bsp, workspace_source_facts, BspDocFacts, IndexBootstrap, LiveBspModelSource, LoadOutcome,
    ModelSource, ReadyModel,
};
pub use capabilities::{
    initialize_result, server_capabilities, InitializeResult, ServerCapabilities, SERVER_NAME,
    SERVER_VERSION,
};
pub use cli::{parse_args, resolve_doctor_dir, CliAction};
pub use convert::{
    highlight_kind, location, range, symbol_kind, workspace_edit, DocumentHighlight,
    DocumentHighlightKind, Location, SymbolKind, TextEdit, WorkspaceEdit, WorkspaceSymbol,
};
pub use diagnostics::{to_lsp_diagnostic, DiagnosticRouter};
pub use documents::DocumentStore;
pub use jsonrpc::{
    parse_incoming, read_frame, write_frame, Incoming, Notification, Request, RequestId, Response,
    ResponseError,
};
pub use lifecycle::{
    pre_ready_outcome, require_ready, Method, NotReadyError, PreReadyOutcome, WorkspaceState,
};
pub use protocol::{Diagnostic, DiagnosticCode, Position, PublishDiagnosticsParams, Range};
pub use server::{
    serve, Bootstrap, BootstrapContext, Handlers, RequestContext, ServerCore, ServerHooks,
};
pub use services::{
    highlights_to_lsp, references_locations, workspace_symbol_of, CoreHandlers, CoreServices,
};
pub use workspace_uris::WorkspaceUris;
