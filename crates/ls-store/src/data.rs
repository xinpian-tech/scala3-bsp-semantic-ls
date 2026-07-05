//! In-memory segment description consumed by [`crate::SegmentWriter`]. A port of
//! the Scala `ls.postings.SegmentData` model, extended with the per-target and
//! per-symbol metadata sections and the search-row plumbing.

use ls_index_model::Span;

/// One indexed document (indexed by `doc_ord` = position in `SegmentData::docs`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentDoc {
    pub uri: String,
    pub doc_id: i64,
    pub epoch: i32,
    pub target_ord: i32,
    pub generated: bool,
    pub readonly: bool,
}

/// One semantic symbol. Provided in any order; sorted by UTF-8 bytes on disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SegmentSymbol {
    pub semantic_symbol: String,
    pub symbol_id: i64,
    /// `-1` = none.
    pub ref_group_ord: i32,
    /// `-1` = none.
    pub rename_group_ord: i32,
    /// `-1` = unknown.
    pub def_target_ord: i32,
}

/// One group-postings occurrence (reference/definition/rename role).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GroupOcc {
    pub doc_ord: i32,
    pub doc_epoch: i32,
    pub target_ord: i32,
    pub span: Span,
    /// `ls_index_model::occ_flags` bits.
    pub flags: i32,
}

/// One doc-postings occurrence (per document, symbol-at-position source).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocOcc {
    /// Caller symbol ordinal; the writer remaps it to the sorted symbol ordinal.
    pub symbol_ord: i32,
    pub span: Span,
    pub flags: i32,
}

/// A rename group's profile — the eight `ls.index.RenameProfile` fields, stored
/// as six `profile_flags` bits + `editable_occurrence_count` + a reason mask.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct RenameProfile {
    pub is_local: bool,
    pub is_external: bool,
    pub has_generated_occurrences: bool,
    pub has_readonly_occurrences: bool,
    pub has_override_family: bool,
    pub has_companion: bool,
    pub editable_occurrence_count: i32,
    pub unsafe_reason_mask: i64,
}

/// Per-target metadata (`target-meta.bin`, indexed by `target_ord`). Replaces
/// the SQLite `targets` table.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TargetMeta {
    pub bsp_id: String,
    pub scala_version: String,
    pub sourceroot: String,
    pub semanticdb_root: String,
    pub content_hash: i64,
    pub options_hash: i64,
}

/// Per-symbol metadata (`symbol-meta.bin`, in the same sorted order as
/// `symbol-index.bin`). Replaces the SQLite `symbol_metadata` table.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SymbolMeta {
    pub display: String,
    pub owner: String,
    pub package_name: String,
    /// `ls_index_model::SymKind` code.
    pub kind: i32,
    /// `ls_index_model::sym_props` bits.
    pub properties: u32,
    pub def_packed_start: i32,
    pub def_packed_end: i32,
    /// `-1` = unknown.
    pub def_doc_ord: i32,
}

/// One `search.bin` row (task6 fills the ranking; task4 only plumbs the
/// section). Rows are written sorted by `normalized_name`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchRow {
    pub normalized_name: String,
    pub symbol_ord: i32,
}

/// The complete logical content of one segment generation.
#[derive(Clone, Debug, Default)]
pub struct SegmentData {
    pub docs: Vec<SegmentDoc>,
    /// Persistent target ids, indexed by `target_ord`.
    pub targets: Vec<i64>,
    /// Symbols in caller order (sorted by UTF-8 bytes on disk).
    pub symbols: Vec<SegmentSymbol>,
    /// Reference-role occurrences per `ref_group_ord`.
    pub ref_occurrences: Vec<Vec<GroupOcc>>,
    /// Definition-role occurrences per `ref_group_ord` (shared ordinal space).
    pub def_occurrences: Vec<Vec<GroupOcc>>,
    /// Rename edit candidates per `rename_group_ord`.
    pub rename_occurrences: Vec<Vec<GroupOcc>>,
    /// One profile per `rename_group_ord`.
    pub rename_profiles: Vec<RenameProfile>,
    /// Doc-postings occurrences per `doc_ord`.
    pub doc_occurrences: Vec<Vec<DocOcc>>,
    /// Per-target metadata, parallel to `targets`.
    pub target_meta: Vec<TargetMeta>,
    /// Per-symbol metadata, parallel to `symbols` (caller order; remapped).
    pub symbol_meta: Vec<SymbolMeta>,
    /// Search rows (plumbing; empty is a valid, empty `search.bin`).
    pub search_rows: Vec<SearchRow>,
}
