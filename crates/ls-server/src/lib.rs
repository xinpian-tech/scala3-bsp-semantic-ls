//! The LSP server layer: command-line entry, the advertised capability surface,
//! the workspace lifecycle state machine with its per-method pre-ready policy,
//! the BSP diagnostics router, and the dirty-buffer document store. A
//! behavior-preserving port of the `ls.core` server, wired over the Rust engine,
//! BSP client, and PC-island boundary crates.

pub mod bootstrap;
mod build_scheduler;
pub mod capabilities;
pub mod cli;
pub mod convert;
pub mod diagnostics;
pub mod doctor;
pub mod documents;
pub mod jsonrpc;
pub mod lifecycle;
pub mod pc;
mod pc_convert;
mod pc_lsp;
pub mod pc_overlay;
pub mod protocol;
pub mod server;
pub mod services;
pub mod store_dump;
pub mod workspace_uris;

pub use bootstrap::{
    from_bsp, ready_model_from_session, reload_build_model, workspace_source_facts, BspDocFacts,
    IndexBootstrap, LiveBspModelSource, LoadOutcome, ModelSource, ReadyModel,
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
// Re-exported so the doctor `PC` section and the cold-start assertion share one
// non-invasive "is the embedded island mapped into this process?" check.
pub use ls_jvm::libjvm_mapped;
pub use pc::{
    pc_options, IslandPcService, PcCompilerPluginStatus, PcDisabledPlugin, PcLocation,
    PcPluginStatusReport, PcQueryService, PcServicePluginStatus, SearchMethodsResolver,
    SymbolResolver, ToplevelsResolver,
};
pub use protocol::{Diagnostic, DiagnosticCode, Position, PublishDiagnosticsParams, Range};
pub use server::{
    serve, Bootstrap, Handlers, OutputSink, RequestContext, ServerCore, WatchedFileEvent,
};
pub use services::{
    highlights_to_lsp, pc_locations_to_lsp, references_locations, workspace_symbol_of,
    BuildCompiler, CoreHandlers, CoreServices,
};
pub use store_dump::{dump_report, store_section};
pub use workspace_uris::WorkspaceUris;
