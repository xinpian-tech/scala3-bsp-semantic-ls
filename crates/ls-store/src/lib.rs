//! `ls-store` — immutable index-segment storage.
//!
//! A faithful Rust port of the `ls.postings` v1 on-disk segment format
//! (`docs/index-format.md`): little-endian, unaligned, CRC32C-validated files
//! published with the fsync + atomic-rename protocol. The format is extended
//! with the snapshot-resident [`TargetMeta`] and [`SymbolMeta`] sections
//! (`target-meta.bin` / `symbol-meta.bin`) and the `search.bin` plumbing that
//! task6 fills.
//!
//! [`SegmentWriter::write`] builds a segment; [`SegmentReader::open`] mmaps and
//! validates it whole, rejecting any corruption with a typed [`SegmentError`].
//! The manifest / generational workspace-state / snapshot lifecycle is task5.

pub mod crc;
pub mod data;
pub mod error;
pub mod format;
mod reader;
mod wire;
mod writer;

pub use data::{
    DocOcc, GroupOcc, RenameProfile, SearchRow, SegmentData, SegmentDoc, SegmentSymbol, SymbolMeta,
    TargetMeta,
};
pub use error::{Result, SegmentError};
pub use reader::{
    BlockView, DocEntryView, DocRecord, GroupIndexView, GroupRecord, IntervalView, SegmentReader,
    SymbolView,
};
pub use writer::SegmentWriter;
