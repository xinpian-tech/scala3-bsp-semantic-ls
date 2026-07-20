//! Query engines and orchestration for the Rust language server: the full-
//! generation SemanticDB ingest pipeline, the three query paths (index /
//! raw-semanticdb / pc) with three consistency levels, and the references and
//! rename engines. A behavior-preserving port of the Scala `ls.rename` module,
//! re-shaped onto the immutable-segment + generational workspace-state store.

mod hash;
mod state;

pub mod highlight;
pub mod identifiers;
pub mod ingest;
pub mod orchestrator;
pub mod overlay;
pub mod references;
pub mod rename;
pub mod symbol_encoding;
pub mod targets;

pub use highlight::{DocHighlight, DocumentHighlightService, HighlightKind};
pub use ingest::{ingest, IngestReport, SemanticdbFileError};
pub use ls_semanticdb::DocFacts;
pub use orchestrator::{
    current_thread_label, CursorSymbol, DocSymbolEntry, MethodHit, QueryOrchestrator,
    ResolutionSource, WorkspaceSymbolEntry,
};
pub use overlay::{DirtyBufferOverlay, NoopOverlay, OverlayHit};
pub use references::{ReferenceHit, ReferencesEngine, ReferencesResult};
pub use rename::{CompileOutcome, CompileService, RenameEngine, TextEditSpan, WorkspaceEditPlan};
pub use state::IngestState;
pub use targets::{DocFactsFn, TargetSpec, WorkspaceTargets};
