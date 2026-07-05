//! SPI for the presentation-compiler dirty-buffer overlay (the PCPath). The real
//! PC-backed implementation lives in the server module; this crate only consumes
//! the hooks:
//!
//!   - a *dirty* uri (open buffer differs from disk) makes the overlay the only
//!     trusted source for symbol-at-cursor in that file;
//!   - `occurrences_of` contributes extra dirty-buffer occurrences to references
//!     results (never to rename, which is FreshRequired).
//!
//! Overlay data is never written to the store.

use ls_index_model::{Loc, Role, Span};

/// Symbol-at-cursor answer from a dirty-buffer overlay. `pc_only` marks symbols
/// that only exist in PC-plugin synthetic sources / overlays and are therefore
/// excluded from workspace references truth and rejected for rename.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OverlayHit {
    pub semantic_symbol: String,
    pub span: Span,
    pub role: Role,
    pub pc_only: bool,
}

pub trait DirtyBufferOverlay {
    /// True when the open editor buffer for `uri` differs from disk.
    fn is_dirty(&self, uri: &str) -> bool;

    /// Symbol at cursor inside a dirty buffer; `None` when the overlay cannot
    /// answer (the query then degrades instead of using the stale index).
    fn symbol_at(&self, uri: &str, line: u32, character: u32) -> Option<OverlayHit>;

    /// Occurrences of `semantic_symbol` contributed by dirty buffers, or `None`
    /// when the overlay has nothing to add.
    fn occurrences_of(&self, semantic_symbol: &str) -> Option<Vec<Loc>>;

    /// True when [`occurrences_of`](Self::occurrences_of) can contribute anything
    /// at all. When false (the default, and the production PC overlay),
    /// references skip the per-group overlay fan-out entirely.
    fn contributes_occurrences(&self) -> bool {
        false
    }
}

/// Overlay used until the PC worker is wired in: nothing is ever dirty.
pub struct NoopOverlay;

impl DirtyBufferOverlay for NoopOverlay {
    fn is_dirty(&self, _uri: &str) -> bool {
        false
    }
    fn symbol_at(&self, _uri: &str, _line: u32, _character: u32) -> Option<OverlayHit> {
        None
    }
    fn occurrences_of(&self, _semantic_symbol: &str) -> Option<Vec<Loc>> {
        None
    }
}
