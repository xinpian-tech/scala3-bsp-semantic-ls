//! On-disk constants for the v1 index-segment format (docs/index-format.md),
//! extended with the snapshot-resident `target-meta.bin` / `symbol-meta.bin`
//! sections and the `search.bin` plumbing.

/// `header.bin` magic — bytes `L S P G` in file order (little-endian `u32`).
pub const MAGIC: u32 = 0x4750_534c;
/// Format version stored in `header.bin`.
pub const VERSION: u16 = 1;
/// Max records per skip block (group and doc-interval blocks).
pub const BLOCK_SIZE: i32 = 256;

pub const HEADER_SIZE: usize = 64;
/// The header self-checksum covers bytes `[0, HEADER_CHECKSUM_OFFSET)`.
pub const HEADER_CHECKSUM_OFFSET: usize = 56;
pub const GROUP_INDEX_ENTRY_SIZE: usize = 16;
pub const RENAME_PROFILE_ENTRY_SIZE: usize = 16;
pub const DOC_ENTRY_SIZE: usize = 48;
pub const INTERVAL_ENTRY_SIZE: usize = 24;
pub const SYMBOL_ENTRY_SIZE: usize = 32;
/// `BlockEntry` fixed prefix, before the `target_words[W]` array.
pub const BLOCK_ENTRY_FIXED_SIZE: usize = 40;
pub const TARGET_META_ENTRY_SIZE: usize = 48;
pub const SYMBOL_META_ENTRY_SIZE: usize = 48;

pub const HEADER_FILE: &str = "header.bin";
pub const REF_GROUP_INDEX_FILE: &str = "ref-group-index.bin";
pub const DEF_GROUP_INDEX_FILE: &str = "definition-group-index.bin";
pub const RENAME_GROUP_INDEX_FILE: &str = "rename-group-index.bin";
pub const DOC_INDEX_FILE: &str = "doc-index.bin";
pub const SYMBOL_INDEX_FILE: &str = "symbol-index.bin";
pub const REF_POSTINGS_FILE: &str = "ref-postings.bin";
pub const DEF_POSTINGS_FILE: &str = "definition-postings.bin";
pub const RENAME_POSTINGS_FILE: &str = "rename-postings.bin";
pub const DOC_POSTINGS_FILE: &str = "doc-postings.bin";
pub const BLOCK_INDEX_FILE: &str = "block-index.bin";
pub const TARGET_META_FILE: &str = "target-meta.bin";
pub const SYMBOL_META_FILE: &str = "symbol-meta.bin";
pub const SEARCH_FILE: &str = "search.bin";
pub const CHECKSUMS_FILE: &str = "checksums.bin";

/// The checksummed files, in the canonical order they appear in `checksums.bin`.
/// The v1 eleven, then the extension sections; `checksums.bin` is never itself
/// checksummed.
pub const CHECKSUMMED_FILES: [&str; 14] = [
    HEADER_FILE,
    REF_GROUP_INDEX_FILE,
    DEF_GROUP_INDEX_FILE,
    RENAME_GROUP_INDEX_FILE,
    DOC_INDEX_FILE,
    SYMBOL_INDEX_FILE,
    REF_POSTINGS_FILE,
    DEF_POSTINGS_FILE,
    RENAME_POSTINGS_FILE,
    DOC_POSTINGS_FILE,
    BLOCK_INDEX_FILE,
    TARGET_META_FILE,
    SYMBOL_META_FILE,
    SEARCH_FILE,
];

/// `DocEntry.doc_flags` bits.
pub mod doc_flags {
    pub const GENERATED: i32 = 1 << 0;
    pub const READONLY: i32 = 1 << 1;
}

/// `RenameProfileEntry.profile_flags` bits.
pub mod prof_flags {
    pub const IS_LOCAL: i32 = 1 << 0;
    pub const IS_EXTERNAL: i32 = 1 << 1;
    pub const HAS_GENERATED: i32 = 1 << 2;
    pub const HAS_READONLY: i32 = 1 << 3;
    pub const HAS_OVERRIDE_FAMILY: i32 = 1 << 4;
    pub const HAS_COMPANION: i32 = 1 << 5;
}

/// Words needed for a `target_count`-wide exact bitset — `max(1, ceil(n/64))`,
/// matching the Scala `SegmentFormat.targetWordCount` (never degenerate).
#[inline]
pub const fn target_word_count(target_count: usize) -> usize {
    let w = (target_count + 63) >> 6;
    if w == 0 {
        1
    } else {
        w
    }
}

/// `BlockEntry` size for a given target count.
#[inline]
pub const fn block_entry_size(target_count: usize) -> usize {
    BLOCK_ENTRY_FIXED_SIZE + 8 * target_word_count(target_count)
}

/// The zero-padded segment directory name (`segment-NNNNNN`).
pub fn segment_dir_name(segment_id: u64) -> String {
    format!("segment-{segment_id:06}")
}
