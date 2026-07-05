//! `ls-store` — immutable index-segment storage.
//!
//! A faithful Rust port of the `ls.postings` v1 on-disk segment format
//! (`docs/index-format.md`): little-endian, unaligned, CRC32C-validated files
//! published with the fsync + atomic-rename protocol. The format is extended
//! with the snapshot-resident [`TargetMeta`] and [`SymbolMeta`] sections
//! (`target-meta.bin` / `symbol-meta.bin`) and the `search.bin` plumbing that
//! the search-ranking layer fills.
//!
//! [`SegmentWriter::write`] builds a segment; [`SegmentReader::open`] mmaps and
//! validates it whole, rejecting any corruption with a typed [`SegmentError`].
//! On top of the segment layer, [`Store`] adds the `manifest.json` single commit
//! point, generational `workspace-state-<gen>.bin` files, and the immutable
//! [`Snapshot`] lifecycle (publish → recover → janitor) with typed
//! [`StoreError`]s.

pub mod crc;
pub mod data;
mod durable;
pub mod error;
pub mod format;
pub mod manifest;
mod reader;
pub mod search;
mod snapshot;
mod wire;
pub mod workspace_state;
mod writer;

pub use data::{
    DocOcc, GroupOcc, RenameProfile, SearchRow, SegmentData, SegmentDoc, SegmentSymbol, SymbolMeta,
    TargetMeta,
};
pub use error::{Result, SegmentError, StoreError, StoreResult};
pub use manifest::{Manifest, MANIFEST_SCHEMA_VERSION};
pub use reader::{
    BlockView, DocEntryView, DocRecord, GroupIndexView, GroupRecord, IntervalView, OccurrenceHit,
    SegmentReader, SymbolView,
};
pub use search::{SearchIndex, WorkspaceSymbolHit, FUZZY_CANDIDATE_CAP};
pub use snapshot::{Failpoint, Snapshot, Store};
pub use workspace_state::{WorkspaceState, STATE_VERSION};
pub use writer::SegmentWriter;
