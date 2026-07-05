//! Segment reader — mmaps a published segment, validates it whole at open time
//! (magic, version, header self-checksum, every file CRC, structural
//! cross-checks) and exposes typed accessors plus the documented group/doc scan
//! obligations. A port of the read/validate half of `ls.postings.SegmentReader`.

use std::path::Path;

use memmap2::Mmap;

use ls_index_model::{occ_flags, Role, Span, TargetBitset};

use crate::crc::crc32c;
use crate::data::{RenameProfile, SymbolMeta, TargetMeta};
use crate::error::{Result, SegmentError};
use crate::format::{self, target_word_count};
use crate::wire::{read_i32, read_i64, read_u16, read_u32, read_u64};

/// A `GroupIndexEntry` view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupIndexView {
    pub offset: i64,
    pub count: i32,
    pub block_index_offset: i32,
}

/// One group-postings record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GroupRecord {
    pub doc_ord: i32,
    pub doc_epoch: i32,
    pub target_ord: i32,
    pub packed_start: i32,
    pub packed_end: i32,
    pub flags: i32,
}

/// The postings/interval pointers of a `DocEntry`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocEntryView {
    pub interval_first: i32,
    pub postings_offset: i64,
    pub postings_count: i32,
    pub interval_count: i32,
}

/// One `IntervalEntry`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IntervalView {
    pub first_line: i32,
    pub last_line: i32,
    pub offset: i64,
    pub count: i32,
}

/// One doc-postings record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocRecord {
    pub symbol_ord: i32,
    pub packed_start: i32,
    pub packed_end: i32,
    pub flags: i32,
}

/// A `SymbolEntry` view (the string is fetched via `semantic_symbol_of`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SymbolView {
    pub symbol_id: i64,
    pub ref_group_ord: i32,
    pub rename_group_ord: i32,
    pub def_target_ord: i32,
}

/// The occurrence covering a queried position (`symbol_at`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OccurrenceHit {
    pub symbol_ord: i32,
    pub doc_ord: u32,
    pub span: Span,
    pub role: Role,
    pub flags: i32,
}

/// One `BlockEntry`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlockView {
    pub first_record: i64,
    pub record_count: i32,
    pub editable_count: i32,
    pub ref_role_count: i32,
    pub def_role_count: i32,
    pub doc_ord_min: i32,
    pub doc_ord_max: i32,
    pub epoch_min: i32,
    pub epoch_max: i32,
    pub target_words: Vec<u64>,
}

/// A validated, mmapped segment.
pub struct SegmentReader {
    files: [Mmap; 14],
    segment_id: u64,
    created_at_ms: i64,
    ref_group_count: u32,
    rename_group_count: u32,
    doc_count: u32,
    occurrence_count: u64,
    symbol_count: usize,
    target_count: usize,
    words: usize,
    doc_epochs: Vec<i32>,
}

// Index into `files`, matching format::CHECKSUMMED_FILES order.
const HEADER: usize = 0;
const REF_GROUP_INDEX: usize = 1;
const DEF_GROUP_INDEX: usize = 2;
const RENAME_GROUP_INDEX: usize = 3;
const DOC_INDEX: usize = 4;
const SYMBOL_INDEX: usize = 5;
const REF_POSTINGS: usize = 6;
const DEF_POSTINGS: usize = 7;
const RENAME_POSTINGS: usize = 8;
const DOC_POSTINGS: usize = 9;
const BLOCK_INDEX: usize = 10;
const TARGET_META: usize = 11;
const SYMBOL_META: usize = 12;
const SEARCH: usize = 13;

impl SegmentReader {
    /// Open and fully validate the segment in `dir`. Returns the first
    /// validation failure as a typed [`SegmentError`]; never partially serves.
    pub fn open(dir: &Path) -> Result<SegmentReader> {
        // Map the 14 checksummed files (canonical order) + checksums.bin.
        let mut mapped: Vec<Mmap> = Vec::with_capacity(14);
        for name in format::CHECKSUMMED_FILES {
            mapped.push(map_file(&dir.join(name))?);
        }
        let checksums = map_file(&dir.join(format::CHECKSUMS_FILE))?;

        // --- header.bin ---
        let header = &mapped[HEADER][..];
        if header.len() != format::HEADER_SIZE {
            return Err(SegmentError::Truncated {
                file: format::HEADER_FILE.into(),
            });
        }
        let magic = read_u32(header, 0).unwrap();
        if magic != format::MAGIC {
            return Err(SegmentError::BadMagic { found: magic });
        }
        let version = read_u16(header, 4).unwrap();
        if version != format::VERSION {
            return Err(SegmentError::BadVersion { found: version });
        }
        let stored_header_crc = read_u64(header, format::HEADER_CHECKSUM_OFFSET).unwrap();
        let computed = crc32c(&header[..format::HEADER_CHECKSUM_OFFSET]) as u64;
        if stored_header_crc != computed {
            return Err(SegmentError::HeaderChecksumMismatch);
        }
        let segment_id = read_u64(header, 8).unwrap();
        let created_at_ms = read_i64(header, 16).unwrap();
        let ref_group_count = read_u64(header, 24).unwrap();
        let rename_group_count = read_u64(header, 32).unwrap();
        let doc_count = read_u64(header, 40).unwrap();
        let occurrence_count = read_u64(header, 48).unwrap();

        // --- checksums.bin: list exactly the 14 files, in order, CRC each. ---
        verify_checksums(&checksums, &mapped)?;

        let reader = SegmentReader {
            files: mapped.try_into().unwrap_or_else(|_| unreachable!()),
            segment_id,
            created_at_ms,
            ref_group_count: ref_group_count as u32,
            rename_group_count: rename_group_count as u32,
            doc_count: doc_count as u32,
            occurrence_count,
            symbol_count: 0,
            target_count: 0,
            words: 0,
            doc_epochs: Vec::new(),
        };
        reader.validate_structure()
    }

    // ---- header accessors ----
    pub fn segment_id(&self) -> u64 {
        self.segment_id
    }
    pub fn created_at_ms(&self) -> i64 {
        self.created_at_ms
    }
    pub fn ref_group_count(&self) -> u32 {
        self.ref_group_count
    }
    pub fn rename_group_count(&self) -> u32 {
        self.rename_group_count
    }
    pub fn doc_count(&self) -> u32 {
        self.doc_count
    }
    pub fn occurrence_count(&self) -> u64 {
        self.occurrence_count
    }
    pub fn symbol_count(&self) -> usize {
        self.symbol_count
    }
    pub fn target_count(&self) -> usize {
        self.target_count
    }

    fn file(&self, idx: usize) -> &[u8] {
        &self.files[idx][..]
    }

    // ---- group index ----
    fn group_index_view(file: &[u8], ord: u32) -> GroupIndexView {
        let base = 8 + ord as usize * format::GROUP_INDEX_ENTRY_SIZE;
        GroupIndexView {
            offset: read_i64(file, base).unwrap(),
            count: read_i32(file, base + 8).unwrap(),
            block_index_offset: read_i32(file, base + 12).unwrap(),
        }
    }
    pub fn ref_group(&self, ord: u32) -> GroupIndexView {
        Self::group_index_view(self.file(REF_GROUP_INDEX), ord)
    }
    pub fn def_group(&self, ord: u32) -> GroupIndexView {
        Self::group_index_view(self.file(DEF_GROUP_INDEX), ord)
    }
    pub fn rename_group(&self, ord: u32) -> GroupIndexView {
        Self::group_index_view(self.file(RENAME_GROUP_INDEX), ord)
    }

    /// A rename group's profile (stored after the `GroupIndexEntry` array).
    pub fn rename_profile(&self, ord: u32) -> RenameProfile {
        let file = self.file(RENAME_GROUP_INDEX);
        let base = 8
            + self.rename_group_count as usize * format::GROUP_INDEX_ENTRY_SIZE
            + ord as usize * format::RENAME_PROFILE_ENTRY_SIZE;
        let flags = read_i32(file, base).unwrap();
        use format::prof_flags::*;
        RenameProfile {
            is_local: flags & IS_LOCAL != 0,
            is_external: flags & IS_EXTERNAL != 0,
            has_generated_occurrences: flags & HAS_GENERATED != 0,
            has_readonly_occurrences: flags & HAS_READONLY != 0,
            has_override_family: flags & HAS_OVERRIDE_FAMILY != 0,
            has_companion: flags & HAS_COMPANION != 0,
            editable_occurrence_count: read_i32(file, base + 4).unwrap(),
            unsafe_reason_mask: read_i64(file, base + 8).unwrap(),
        }
    }

    // ---- group postings ----
    fn group_record(file: &[u8], r: i64) -> GroupRecord {
        let n = read_i64(file, 0).unwrap();
        let col = |c: i64| read_i32(file, (8 + c * 4 * n + 4 * r) as usize).unwrap();
        GroupRecord {
            doc_ord: col(0),
            doc_epoch: col(1),
            target_ord: col(2),
            packed_start: col(3),
            packed_end: col(4),
            flags: col(5),
        }
    }
    pub fn ref_record(&self, r: i64) -> GroupRecord {
        Self::group_record(self.file(REF_POSTINGS), r)
    }
    pub fn def_record(&self, r: i64) -> GroupRecord {
        Self::group_record(self.file(DEF_POSTINGS), r)
    }
    pub fn rename_record(&self, r: i64) -> GroupRecord {
        Self::group_record(self.file(RENAME_POSTINGS), r)
    }

    // ---- doc index ----
    fn doc_entry_base(idx: usize) -> usize {
        24 + idx * format::DOC_ENTRY_SIZE
    }
    pub fn uri_of(&self, doc_ord: u32) -> &str {
        let file = self.file(DOC_INDEX);
        let base = Self::doc_entry_base(doc_ord as usize);
        let off = read_i32(file, base).unwrap() as usize;
        let len = read_i32(file, base + 4).unwrap() as usize;
        let blob_start = self.uri_blob_start();
        std::str::from_utf8(&file[blob_start + off..blob_start + off + len]).unwrap_or("")
    }
    pub fn doc_id_of(&self, doc_ord: u32) -> i64 {
        read_i64(
            self.file(DOC_INDEX),
            Self::doc_entry_base(doc_ord as usize) + 8,
        )
        .unwrap()
    }
    pub fn epoch_of(&self, doc_ord: u32) -> i32 {
        read_i32(
            self.file(DOC_INDEX),
            Self::doc_entry_base(doc_ord as usize) + 16,
        )
        .unwrap()
    }
    pub fn target_ord_of_doc(&self, doc_ord: u32) -> i32 {
        read_i32(
            self.file(DOC_INDEX),
            Self::doc_entry_base(doc_ord as usize) + 20,
        )
        .unwrap()
    }
    pub fn doc_flags_of(&self, doc_ord: u32) -> i32 {
        read_i32(
            self.file(DOC_INDEX),
            Self::doc_entry_base(doc_ord as usize) + 24,
        )
        .unwrap()
    }
    pub fn doc_generated(&self, doc_ord: u32) -> bool {
        self.doc_flags_of(doc_ord) & format::doc_flags::GENERATED != 0
    }
    pub fn doc_readonly(&self, doc_ord: u32) -> bool {
        self.doc_flags_of(doc_ord) & format::doc_flags::READONLY != 0
    }
    pub fn doc_entry(&self, doc_ord: u32) -> DocEntryView {
        let file = self.file(DOC_INDEX);
        let base = Self::doc_entry_base(doc_ord as usize);
        DocEntryView {
            interval_first: read_i32(file, base + 28).unwrap(),
            postings_offset: read_i64(file, base + 32).unwrap(),
            postings_count: read_i32(file, base + 40).unwrap(),
            interval_count: read_i32(file, base + 44).unwrap(),
        }
    }
    fn interval_count_total(&self) -> i64 {
        read_i64(self.file(DOC_INDEX), 8).unwrap()
    }
    fn uri_blob_start(&self) -> usize {
        let intervals = self.interval_count_total() as usize;
        24 + self.doc_count as usize * format::DOC_ENTRY_SIZE
            + intervals * format::INTERVAL_ENTRY_SIZE
    }
    pub fn interval_entry(&self, idx: i32) -> IntervalView {
        let file = self.file(DOC_INDEX);
        let base = 24
            + self.doc_count as usize * format::DOC_ENTRY_SIZE
            + idx as usize * format::INTERVAL_ENTRY_SIZE;
        IntervalView {
            first_line: read_i32(file, base).unwrap(),
            last_line: read_i32(file, base + 4).unwrap(),
            offset: read_i64(file, base + 8).unwrap(),
            count: read_i32(file, base + 16).unwrap(),
        }
    }

    // ---- doc postings ----
    pub fn doc_record(&self, r: i64) -> DocRecord {
        let file = self.file(DOC_POSTINGS);
        let n = read_i64(file, 0).unwrap();
        let col = |c: i64| read_i32(file, (8 + c * 4 * n + 4 * r) as usize).unwrap();
        DocRecord {
            symbol_ord: col(0),
            packed_start: col(1),
            packed_end: col(2),
            flags: col(3),
        }
    }

    // ---- symbol index ----
    fn symbol_entry_base(idx: usize) -> usize {
        24 + idx * format::SYMBOL_ENTRY_SIZE
    }
    fn sym_blob_start(&self) -> usize {
        24 + self.symbol_count * format::SYMBOL_ENTRY_SIZE + self.target_count * 8
    }
    pub fn semantic_symbol_of(&self, ord: u32) -> &str {
        let file = self.file(SYMBOL_INDEX);
        let base = Self::symbol_entry_base(ord as usize);
        let off = read_i32(file, base).unwrap() as usize;
        let len = read_i32(file, base + 4).unwrap() as usize;
        let blob = self.sym_blob_start();
        std::str::from_utf8(&file[blob + off..blob + off + len]).unwrap_or("")
    }
    pub fn symbol_view(&self, ord: u32) -> SymbolView {
        let file = self.file(SYMBOL_INDEX);
        let base = Self::symbol_entry_base(ord as usize);
        SymbolView {
            symbol_id: read_i64(file, base + 8).unwrap(),
            ref_group_ord: read_i32(file, base + 16).unwrap(),
            rename_group_ord: read_i32(file, base + 20).unwrap(),
            def_target_ord: read_i32(file, base + 24).unwrap(),
        }
    }
    pub fn target_id_of(&self, target_ord: u32) -> i64 {
        let file = self.file(SYMBOL_INDEX);
        let base = 24 + self.symbol_count * format::SYMBOL_ENTRY_SIZE + target_ord as usize * 8;
        read_i64(file, base).unwrap()
    }

    /// Binary-search the (UTF-8-sorted) symbol dictionary. `None` if absent.
    pub fn find_symbol_ord(&self, semantic_symbol: &str) -> Option<u32> {
        let needle = semantic_symbol.as_bytes();
        let file = self.file(SYMBOL_INDEX);
        let blob = self.sym_blob_start();
        let (mut lo, mut hi) = (0usize, self.symbol_count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let base = Self::symbol_entry_base(mid);
            let off = read_i32(file, base).unwrap() as usize;
            let len = read_i32(file, base + 4).unwrap() as usize;
            let hay = &file[blob + off..blob + off + len];
            match hay.cmp(needle) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => return Some(mid as u32),
            }
        }
        None
    }

    // ---- block index ----
    pub fn block_count(&self) -> i64 {
        read_i64(self.file(BLOCK_INDEX), 0).unwrap()
    }
    pub fn block_word_count(&self) -> i32 {
        read_i32(self.file(BLOCK_INDEX), 8).unwrap()
    }
    pub fn block_size(&self) -> i32 {
        read_i32(self.file(BLOCK_INDEX), 12).unwrap()
    }
    fn block_base(&self, idx: usize) -> usize {
        16 + idx * (format::BLOCK_ENTRY_FIXED_SIZE + 8 * self.words)
    }
    pub fn block_entry(&self, idx: i64) -> BlockView {
        let file = self.file(BLOCK_INDEX);
        let base = self.block_base(idx as usize);
        let mut target_words = Vec::with_capacity(self.words);
        for w in 0..self.words {
            target_words.push(read_u64(file, base + 40 + 8 * w).unwrap());
        }
        BlockView {
            first_record: read_i64(file, base).unwrap(),
            record_count: read_i32(file, base + 8).unwrap(),
            editable_count: read_i32(file, base + 12).unwrap(),
            ref_role_count: read_i32(file, base + 16).unwrap(),
            def_role_count: read_i32(file, base + 20).unwrap(),
            doc_ord_min: read_i32(file, base + 24).unwrap(),
            doc_ord_max: read_i32(file, base + 28).unwrap(),
            epoch_min: read_i32(file, base + 32).unwrap(),
            epoch_max: read_i32(file, base + 36).unwrap(),
            target_words,
        }
    }
    /// Does block `idx`'s exact target bitset intersect `allowed_words`? Reads
    /// the little-endian lanes directly (no unaligned transmute).
    fn block_intersects(&self, idx: i64, allowed_words: &[u64]) -> bool {
        let base = self.block_base(idx as usize) + 40;
        let file = self.file(BLOCK_INDEX);
        let n = self.words.min(allowed_words.len());
        (0..n).any(|w| read_u64(file, base + 8 * w).unwrap() & allowed_words[w] != 0)
    }

    // ---- extension sections ----
    pub fn target_meta(&self, target_ord: u32) -> TargetMeta {
        let file = self.file(TARGET_META);
        let base = 16 + target_ord as usize * format::TARGET_META_ENTRY_SIZE;
        let blob = 16 + self.target_count * format::TARGET_META_ENTRY_SIZE;
        let s = |i: usize| read_str(file, base + i * 8, blob);
        TargetMeta {
            bsp_id: s(0),
            scala_version: s(1),
            sourceroot: s(2),
            semanticdb_root: s(3),
            content_hash: read_i64(file, base + 32).unwrap(),
            options_hash: read_i64(file, base + 40).unwrap(),
        }
    }
    pub fn symbol_meta(&self, symbol_ord: u32) -> SymbolMeta {
        let file = self.file(SYMBOL_META);
        let base = 16 + symbol_ord as usize * format::SYMBOL_META_ENTRY_SIZE;
        let blob = 16 + self.symbol_count * format::SYMBOL_META_ENTRY_SIZE;
        let s = |i: usize| read_str(file, base + i * 8, blob);
        SymbolMeta {
            display: s(0),
            owner: s(1),
            package_name: s(2),
            kind: read_i32(file, base + 24).unwrap(),
            properties: read_u32(file, base + 28).unwrap(),
            def_packed_start: read_i32(file, base + 32).unwrap(),
            def_packed_end: read_i32(file, base + 36).unwrap(),
            def_doc_ord: read_i32(file, base + 40).unwrap(),
        }
    }
    pub fn search_row_count(&self) -> i64 {
        read_i64(self.file(SEARCH), 0).unwrap()
    }
    pub fn search_row(&self, idx: i64) -> (String, i32) {
        let file = self.file(SEARCH);
        let base = 16 + idx as usize * 16;
        let blob = 16 + self.search_row_count() as usize * 16;
        (
            read_str(file, base, blob),
            read_i32(file, base + 8).unwrap(),
        )
    }

    // ---- scans (documented reader obligations) ----

    /// Scan a reference group, honoring the optional target filter, block skip,
    /// and epoch filter. `allowed = None` scans everything.
    pub fn scan_ref_group(
        &self,
        group_ord: u32,
        allowed: Option<&TargetBitset>,
        sink: &mut dyn FnMut(GroupRecord),
    ) {
        let entry = self.ref_group(group_ord);
        self.scan_group(REF_POSTINGS, entry, allowed, false, sink);
    }

    /// Scan a definition group (no target filter; definitions are target-local).
    pub fn scan_def_group(&self, group_ord: u32, sink: &mut dyn FnMut(GroupRecord)) {
        let entry = self.def_group(group_ord);
        self.scan_group(DEF_POSTINGS, entry, None, false, sink);
    }

    /// Scan a rename group, yielding only editable occurrences (block-level and
    /// per-record editable filter).
    pub fn scan_rename_group(&self, group_ord: u32, sink: &mut dyn FnMut(GroupRecord)) {
        let entry = self.rename_group(group_ord);
        self.scan_group(RENAME_POSTINGS, entry, None, true, sink);
    }

    fn scan_group(
        &self,
        postings: usize,
        entry: GroupIndexView,
        allowed: Option<&TargetBitset>,
        require_editable: bool,
        sink: &mut dyn FnMut(GroupRecord),
    ) {
        if entry.count == 0 {
            return;
        }
        let file = self.file(postings);
        let block_size = format::BLOCK_SIZE as i64;
        let num_blocks = (entry.count as i64 + block_size - 1) / block_size;
        let allowed_words = allowed.map(|a| a.to_words());
        for b in 0..num_blocks {
            let block_idx = entry.block_index_offset as i64 + b;
            // Block skip on the target filter.
            if let Some(words) = &allowed_words {
                if !self.block_intersects(block_idx, words) {
                    continue;
                }
            }
            let base = self.block_base(block_idx as usize);
            let editable = read_i32(self.file(BLOCK_INDEX), base + 12).unwrap();
            if require_editable && editable == 0 {
                continue;
            }
            let first = read_i64(self.file(BLOCK_INDEX), base).unwrap();
            let count = read_i32(self.file(BLOCK_INDEX), base + 8).unwrap() as i64;
            for r in first..first + count {
                let rec = Self::group_record(file, r);
                if let Some(allowed) = allowed {
                    if rec.target_ord < 0 || !allowed.contains(rec.target_ord as u32) {
                        continue;
                    }
                }
                if rec.doc_epoch != self.doc_epochs[rec.doc_ord as usize] {
                    continue;
                }
                if require_editable && !occ_flags::has(rec.flags as u32, occ_flags::EDITABLE) {
                    continue;
                }
                sink(rec);
            }
        }
    }

    /// Scan one document's postings in `(packed_start, packed_end)` order.
    pub fn scan_doc(&self, doc_ord: u32, require_editable: bool, sink: &mut dyn FnMut(DocRecord)) {
        let entry = self.doc_entry(doc_ord);
        for r in entry.postings_offset..entry.postings_offset + entry.postings_count as i64 {
            let rec = self.doc_record(r);
            if require_editable && !occ_flags::has(rec.flags as u32, occ_flags::EDITABLE) {
                continue;
            }
            sink(rec);
        }
    }

    /// Exact symbol-at-position over the doc interval-block index. Containment is
    /// start/end-inclusive on packed positions; the smallest covering span wins,
    /// then the earliest `packed_start`, then the first record in sort order
    /// (`docs/index-format.md`; a port of `SegmentReader.symbolAt`).
    pub fn symbol_at(&self, doc_ord: u32, line: u32, character: u32) -> Option<OccurrenceHit> {
        self.symbol_at_counting(doc_ord, line, character).0
    }

    /// As [`symbol_at`], also returning how many interval blocks were scanned
    /// (a diagnostic for asserting block-index pruning effectiveness).
    pub fn symbol_at_counting(
        &self,
        doc_ord: u32,
        line: u32,
        character: u32,
    ) -> (Option<OccurrenceHit>, u32) {
        let di = self.file(DOC_INDEX);
        let base = Self::doc_entry_base(doc_ord as usize);
        let interval_first = read_i32(di, base + 28).unwrap();
        let interval_count = read_i32(di, base + 44).unwrap();
        if interval_first < 0 || interval_count == 0 {
            return (None, 0);
        }

        // Packed positions are `u32` and must be compared unsigned: `line << 12`
        // exceeds 2^31 for lines >= 524288, so signed comparison would misorder
        // them. Line numbers (< 2^20) stay `i32`.
        let query = Span::pack(line, character);
        let line = line as i32;
        let dp = self.file(DOC_POSTINGS);
        let n = read_i64(dp, 0).unwrap();
        let col_sym = 8;
        let col_start = 8 + 4 * n;
        let col_end = 8 + 8 * n;
        let col_flags = 8 + 12 * n;
        let interval_base = 24 + self.doc_count as usize * format::DOC_ENTRY_SIZE;

        let mut blocks_scanned = 0u32;
        let mut best: Option<(u32, u32, i32, i32)> = None; // (packed_start, packed_end, sym, flags)
        for b in 0..interval_count {
            let ie = interval_base + (interval_first + b) as usize * format::INTERVAL_ENTRY_SIZE;
            let first_line = read_i32(di, ie).unwrap();
            if first_line > line {
                break; // interval blocks are sorted by first start line
            }
            let last_line = read_i32(di, ie + 4).unwrap();
            if last_line < line {
                continue;
            }
            blocks_scanned += 1;
            let rec_first = read_i64(di, ie + 8).unwrap();
            let rec_count = read_i32(di, ie + 16).unwrap() as i64;
            for k in 0..rec_count {
                let r = rec_first + k;
                let ps = read_u32(dp, (col_start + 4 * r) as usize).unwrap();
                if ps > query {
                    break; // records are sorted by packed_start
                }
                let pe = read_u32(dp, (col_end + 4 * r) as usize).unwrap();
                if pe >= query {
                    let size = pe - ps;
                    if best.is_none_or(|(bs, be, _, _)| size < be - bs) {
                        let sym = read_i32(dp, (col_sym + 4 * r) as usize).unwrap();
                        let flags = read_i32(dp, (col_flags + 4 * r) as usize).unwrap();
                        best = Some((ps, pe, sym, flags));
                    }
                }
            }
        }

        match best {
            None => (None, blocks_scanned),
            Some((ps, pe, sym, flags)) => {
                let role = if occ_flags::has(flags as u32, occ_flags::DEFINITION) {
                    Role::Definition
                } else {
                    Role::Reference
                };
                let span = Span::new(
                    Span::unpack_line(ps),
                    Span::unpack_char(ps),
                    Span::unpack_line(pe),
                    Span::unpack_char(pe),
                );
                let hit = OccurrenceHit {
                    symbol_ord: sym,
                    doc_ord,
                    span,
                    role,
                    flags,
                };
                (Some(hit), blocks_scanned)
            }
        }
    }

    // ---- structural validation ----
    //
    // `open` must reject a *self-consistent* corrupt segment (bytes mutated and
    // `checksums.bin` recomputed), not just CRC-mismatched bytes, and must never
    // panic. Every count is bounded before use (no overflow/negative), every
    // embedded ref/ordinal is range-checked, and the group/block and
    // doc/interval partitions are validated against their postings — so all the
    // accessors' unchecked reads are sound after `open` succeeds.

    fn validate_structure(mut self) -> Result<SegmentReader> {
        // symbol-index counts anchor the target/symbol cross-checks.
        let sym = self.file(SYMBOL_INDEX);
        let symbol_count = bounded(
            req_i64(sym, 0, format::SYMBOL_INDEX_FILE)?,
            sym.len(),
            "symbol_count",
        )?;
        let target_count = bounded(
            req_i64(sym, 8, format::SYMBOL_INDEX_FILE)?,
            sym.len(),
            "target_count",
        )?;
        let sym_blob = bounded(
            req_i64(sym, 16, format::SYMBOL_INDEX_FILE)?,
            sym.len(),
            "sym_blob_len",
        )?;
        expect(
            sym.len() == 24 + symbol_count * 32 + target_count * 8 + sym_blob,
            "symbol-index size",
        )?;
        self.symbol_count = symbol_count;
        self.target_count = target_count;
        self.words = target_word_count(target_count);

        let ref_groups = self.ref_group_count as usize;
        let rename_groups = self.rename_group_count as usize;

        // group + doc postings column sizes (+ occurrence count).
        let ref_recs = self.check_columns(REF_POSTINGS, 6)?;
        let def_recs = self.check_columns(DEF_POSTINGS, 6)?;
        let rename_recs = self.check_columns(RENAME_POSTINGS, 6)?;
        let doc_recs = self.check_columns(DOC_POSTINGS, 4)?;
        expect(
            (ref_recs + def_recs + rename_recs + doc_recs) as u64 == self.occurrence_count,
            "occurrence_count sum",
        )?;

        // group index sizes.
        self.check_group_index(REF_GROUP_INDEX, ref_groups, false)?;
        self.check_group_index(DEF_GROUP_INDEX, ref_groups, false)?;
        self.check_group_index(RENAME_GROUP_INDEX, rename_groups, true)?;

        // doc-index size.
        let di = self.file(DOC_INDEX);
        let dc = bounded(
            req_i64(di, 0, format::DOC_INDEX_FILE)?,
            di.len(),
            "doc_count",
        )?;
        let ic = bounded(
            req_i64(di, 8, format::DOC_INDEX_FILE)?,
            di.len(),
            "interval_count",
        )?;
        let ub = bounded(
            req_i64(di, 16, format::DOC_INDEX_FILE)?,
            di.len(),
            "uri_blob_len",
        )?;
        expect(dc == self.doc_count as usize, "doc-index doc_count")?;
        expect(di.len() == 24 + dc * 48 + ic * 24 + ub, "doc-index size")?;

        // block-index size.
        let bi = self.file(BLOCK_INDEX);
        let bc = bounded(
            req_i64(bi, 0, format::BLOCK_INDEX_FILE)?,
            bi.len(),
            "block_count",
        )?;
        expect(
            req_i32(bi, 8, format::BLOCK_INDEX_FILE)? as i64 == self.words as i64,
            "block target_word_count",
        )?;
        expect(
            req_i32(bi, 12, format::BLOCK_INDEX_FILE)? == format::BLOCK_SIZE,
            "block_size",
        )?;
        expect(
            bi.len() == 16 + bc * (40 + 8 * self.words),
            "block-index size",
        )?;

        // extension section sizes.
        let tm = self.file(TARGET_META);
        let tmc = bounded(
            req_i64(tm, 0, format::TARGET_META_FILE)?,
            tm.len(),
            "target-meta count",
        )?;
        let tmb = bounded(
            req_i64(tm, 8, format::TARGET_META_FILE)?,
            tm.len(),
            "target-meta blob",
        )?;
        expect(tmc == target_count, "target-meta count match")?;
        expect(tm.len() == 16 + tmc * 48 + tmb, "target-meta size")?;

        let sm = self.file(SYMBOL_META);
        let smc = bounded(
            req_i64(sm, 0, format::SYMBOL_META_FILE)?,
            sm.len(),
            "symbol-meta count",
        )?;
        let smb = bounded(
            req_i64(sm, 8, format::SYMBOL_META_FILE)?,
            sm.len(),
            "symbol-meta blob",
        )?;
        expect(smc == symbol_count, "symbol-meta count match")?;
        expect(sm.len() == 16 + smc * 48 + smb, "symbol-meta size")?;

        let se = self.file(SEARCH);
        let src = bounded(
            req_i64(se, 0, format::SEARCH_FILE)?,
            se.len(),
            "search rows",
        )?;
        let srb = bounded(
            req_i64(se, 8, format::SEARCH_FILE)?,
            se.len(),
            "search blob",
        )?;
        expect(se.len() == 16 + src * 16 + srb, "search size")?;

        // ---- deep validation: embedded refs, ordinals, partitions ----
        self.validate_symbol_dict(sym_blob)?;
        self.validate_group_records(REF_POSTINGS, ref_recs)?;
        self.validate_group_records(DEF_POSTINGS, def_recs)?;
        self.validate_group_records(RENAME_POSTINGS, rename_recs)?;
        self.validate_doc_records(doc_recs)?;

        // group offset/count tiling + block first_record/record_count partition,
        // in writer block order (ref groups, then def, then rename).
        let mut block_cursor = 0usize;
        self.validate_role_groups(
            REF_GROUP_INDEX,
            ref_groups,
            ref_recs,
            REF_POSTINGS,
            &mut block_cursor,
            bc,
        )?;
        self.validate_role_groups(
            DEF_GROUP_INDEX,
            ref_groups,
            def_recs,
            DEF_POSTINGS,
            &mut block_cursor,
            bc,
        )?;
        self.validate_role_groups(
            RENAME_GROUP_INDEX,
            rename_groups,
            rename_recs,
            RENAME_POSTINGS,
            &mut block_cursor,
            bc,
        )?;
        expect(block_cursor == bc, "block_count total")?;

        // doc entries: uri/target refs + postings & interval tiling.
        self.validate_doc_index(dc, ic, ub, doc_recs)?;
        self.validate_meta_and_search(tmc, tmb, smc, smb, src, srb)?;

        // cache per-doc epochs for scan filtering (safe after validation).
        let mut doc_epochs = Vec::with_capacity(dc);
        for d in 0..self.doc_count {
            doc_epochs.push(self.epoch_of(d));
        }
        self.doc_epochs = doc_epochs;
        Ok(self)
    }

    fn check_group_index(&self, idx: usize, count: usize, with_profiles: bool) -> Result<()> {
        let file = self.file(idx);
        let name = format::CHECKSUMMED_FILES[idx];
        let declared = bounded(req_i64(file, 0, name)?, file.len(), "group count")?;
        expect(declared == count, "group index count")?;
        let mut need = 8 + count * format::GROUP_INDEX_ENTRY_SIZE;
        if with_profiles {
            need += count * format::RENAME_PROFILE_ENTRY_SIZE;
        }
        expect(file.len() == need, "group index size")?;
        Ok(())
    }

    fn check_columns(&self, idx: usize, columns: usize) -> Result<usize> {
        let file = self.file(idx);
        let name = format::CHECKSUMMED_FILES[idx];
        let n = bounded(req_i64(file, 0, name)?, file.len(), "record_count")?;
        expect(file.len() == 8 + columns * 4 * n, "postings columns size")?;
        Ok(n)
    }

    /// Symbol dictionary: string refs in-bounds + UTF-8, strictly sorted (no
    /// duplicates), and every group/def ordinal in range.
    fn validate_symbol_dict(&self, sym_blob: usize) -> Result<()> {
        let file = self.file(SYMBOL_INDEX);
        let name = format::SYMBOL_INDEX_FILE;
        let blob_start = 24 + self.symbol_count * 32 + self.target_count * 8;
        let mut prev: Option<&[u8]> = None;
        for i in 0..self.symbol_count {
            let b = 24 + i * 32;
            let s = str_slice(
                file,
                req_i32(file, b, name)?,
                req_i32(file, b + 4, name)?,
                blob_start,
                sym_blob,
                "symbol str ref",
            )?;
            std::str::from_utf8(s).map_err(|_| structural("symbol not utf-8"))?;
            if let Some(p) = prev {
                expect(p < s, "symbols not strictly sorted")?;
            }
            prev = Some(s);
            check_ord_opt(
                req_i32(file, b + 16, name)?,
                self.ref_group_count as usize,
                "ref_group_ord",
            )?;
            check_ord_opt(
                req_i32(file, b + 20, name)?,
                self.rename_group_count as usize,
                "rename_group_ord",
            )?;
            check_ord_opt(
                req_i32(file, b + 24, name)?,
                self.target_count,
                "def_target_ord",
            )?;
        }
        Ok(())
    }

    /// Every group-postings record's `doc_ord`/`target_ord` in range.
    fn validate_group_records(&self, idx: usize, recs: usize) -> Result<()> {
        let file = self.file(idx);
        let name = format::CHECKSUMMED_FILES[idx];
        let doc_col = 8;
        let target_col = 8 + 2 * 4 * recs;
        for r in 0..recs {
            check_ord(
                req_i32(file, doc_col + 4 * r, name)?,
                self.doc_count as usize,
                "group doc_ord",
            )?;
            check_ord(
                req_i32(file, target_col + 4 * r, name)?,
                self.target_count,
                "group target_ord",
            )?;
        }
        Ok(())
    }

    /// Every doc-postings record's `symbol_ord` in range and packed span
    /// non-inverted (unsigned `packed_start <= packed_end`).
    fn validate_doc_records(&self, recs: usize) -> Result<()> {
        let file = self.file(DOC_POSTINGS);
        let name = format::DOC_POSTINGS_FILE;
        let start_col = 8 + 4 * recs;
        let end_col = 8 + 8 * recs;
        for r in 0..recs {
            check_ord(
                req_i32(file, 8 + 4 * r, name)?,
                self.symbol_count,
                "doc symbol_ord",
            )?;
            let ps = req_u32(file, start_col + 4 * r, name)?;
            let pe = req_u32(file, end_col + 4 * r, name)?;
            expect(ps <= pe, "doc postings inverted span")?;
        }
        Ok(())
    }

    /// One role's group index: offsets tile `[0, postings_recs)` in order, and
    /// each non-empty group's blocks partition it into ≤256-record chunks.
    fn validate_role_groups(
        &self,
        gi_idx: usize,
        group_count: usize,
        postings_recs: usize,
        postings_idx: usize,
        block_cursor: &mut usize,
        block_count: usize,
    ) -> Result<()> {
        let gi = self.file(gi_idx);
        let name = format::CHECKSUMMED_FILES[gi_idx];
        let mut acc = 0usize;
        for g in 0..group_count {
            let b = 8 + g * 16;
            let offset = req_i64(gi, b, name)?;
            expect(
                offset >= 0 && offset as u64 == acc as u64,
                "group offset tiling",
            )?;
            let count = req_i32(gi, b + 8, name)?;
            expect(count >= 0, "group count sign")?;
            let count = count as usize;
            expect(acc + count <= postings_recs, "group range")?;
            let block_off = req_i32(gi, b + 12, name)?;
            if count == 0 {
                expect(block_off == -1, "empty group block_index_offset")?;
            } else {
                expect(
                    block_off >= 0 && block_off as usize == *block_cursor,
                    "group block_index_offset",
                )?;
                let num_blocks = count.div_ceil(256);
                for k in 0..num_blocks {
                    expect(*block_cursor + k < block_count, "block index range")?;
                    self.validate_block(
                        *block_cursor + k,
                        acc + k * 256,
                        (count - k * 256).min(256),
                        postings_idx,
                    )?;
                }
                *block_cursor += num_blocks;
            }
            acc += count;
        }
        expect(acc == postings_recs, "group tiling total")?;
        Ok(())
    }

    /// One block: `first_record`/`record_count` match the partition, and *every*
    /// exact aggregate (editable/role counts, doc/epoch min-max, target bitset)
    /// is recomputed from the owning postings records and required to match, so
    /// tampered skip metadata cannot silently drop records from a filtered scan.
    fn validate_block(
        &self,
        block: usize,
        want_first: usize,
        want_count: usize,
        postings_idx: usize,
    ) -> Result<()> {
        let bi = self.file(BLOCK_INDEX);
        let name = format::BLOCK_INDEX_FILE;
        let base = self.block_base(block);
        expect(
            req_i64(bi, base, name)? as u64 == want_first as u64,
            "block first_record",
        )?;
        let rc = req_i32(bi, base + 8, name)?;
        expect(rc >= 0 && rc as usize == want_count, "block record_count")?;

        // Recompute the exact aggregates from the owning postings records
        // [want_first, want_first + want_count). validate_group_records already
        // range-checked every doc_ord/target_ord, so these reads are in-bounds.
        let pf = self.file(postings_idx);
        let recs = read_i64(pf, 0).unwrap();
        let doc_col = 8;
        let epoch_col = 8 + 4 * recs;
        let target_col = 8 + 8 * recs;
        let flags_col = 8 + 20 * recs;
        let mut editable = 0i32;
        let mut ref_role = 0i32;
        let mut def_role = 0i32;
        let mut dmin = i32::MAX;
        let mut dmax = i32::MIN;
        let mut emin = i32::MAX;
        let mut emax = i32::MIN;
        let mut words = vec![0u64; self.words];
        for k in 0..want_count as i64 {
            let r = want_first as i64 + k;
            let doc = read_i32(pf, (doc_col + 4 * r) as usize).unwrap();
            let epoch = read_i32(pf, (epoch_col + 4 * r) as usize).unwrap();
            let target = read_i32(pf, (target_col + 4 * r) as usize).unwrap();
            let flags = read_i32(pf, (flags_col + 4 * r) as usize).unwrap() as u32;
            if occ_flags::has(flags, occ_flags::EDITABLE) {
                editable += 1;
            }
            if occ_flags::has(flags, occ_flags::DEFINITION) {
                def_role += 1;
            } else {
                ref_role += 1;
            }
            dmin = dmin.min(doc);
            dmax = dmax.max(doc);
            emin = emin.min(epoch);
            emax = emax.max(epoch);
            let t = target as usize;
            words[t >> 6] |= 1u64 << (t & 63);
        }
        expect(
            req_i32(bi, base + 12, name)? == editable,
            "block editable_count",
        )?;
        expect(
            req_i32(bi, base + 16, name)? == ref_role,
            "block ref_role_count",
        )?;
        expect(
            req_i32(bi, base + 20, name)? == def_role,
            "block def_role_count",
        )?;
        expect(req_i32(bi, base + 24, name)? == dmin, "block doc_ord_min")?;
        expect(req_i32(bi, base + 28, name)? == dmax, "block doc_ord_max")?;
        expect(req_i32(bi, base + 32, name)? == emin, "block epoch_min")?;
        expect(req_i32(bi, base + 36, name)? == emax, "block epoch_max")?;
        for (w, &word) in words.iter().enumerate() {
            expect(
                req_u64(bi, base + 40 + 8 * w, name)? == word,
                "block target_words",
            )?;
        }
        Ok(())
    }

    /// Doc entries: uri/target refs valid, doc postings tile `[0, doc_recs)`, and
    /// each non-empty doc's interval blocks partition its postings.
    fn validate_doc_index(
        &self,
        doc_count: usize,
        interval_count: usize,
        uri_blob: usize,
        doc_recs: usize,
    ) -> Result<()> {
        let di = self.file(DOC_INDEX);
        let dp = self.file(DOC_POSTINGS);
        let dp_name = format::DOC_POSTINGS_FILE;
        let dp_start_col = 8 + 4 * doc_recs;
        let dp_end_col = 8 + 8 * doc_recs;
        let name = format::DOC_INDEX_FILE;
        let interval_base = 24 + doc_count * 48;
        let uri_blob_start = interval_base + interval_count * 24;
        let mut post_acc = 0usize;
        let mut iv_cursor = 0usize;
        for d in 0..doc_count {
            let b = 24 + d * 48;
            let uri = str_slice(
                di,
                req_i32(di, b, name)?,
                req_i32(di, b + 4, name)?,
                uri_blob_start,
                uri_blob,
                "uri str ref",
            )?;
            std::str::from_utf8(uri).map_err(|_| structural("uri not utf-8"))?;
            check_ord(
                req_i32(di, b + 20, name)?,
                self.target_count,
                "doc target_ord",
            )?;
            let interval_first = req_i32(di, b + 28, name)?;
            let post_off = req_i64(di, b + 32, name)?;
            expect(
                post_off >= 0 && post_off as u64 == post_acc as u64,
                "doc postings tiling",
            )?;
            let post_count = req_i32(di, b + 40, name)?;
            expect(post_count >= 0, "doc postings_count sign")?;
            let post_count = post_count as usize;
            expect(post_acc + post_count <= doc_recs, "doc postings range")?;
            let iv_count = req_i32(di, b + 44, name)?;
            if post_count == 0 {
                expect(
                    interval_first == -1 && iv_count == 0,
                    "empty doc interval ptr",
                )?;
            } else {
                expect(
                    interval_first >= 0 && interval_first as usize == iv_cursor,
                    "doc interval_first",
                )?;
                let want_iv = post_count.div_ceil(256);
                expect(
                    iv_count >= 0 && iv_count as usize == want_iv,
                    "doc interval_count",
                )?;
                for k in 0..want_iv {
                    expect(iv_cursor + k < interval_count, "interval index range")?;
                    let ib = interval_base + (iv_cursor + k) * 24;
                    let iv_offset = post_acc + k * 256;
                    let iv_count = (post_count - k * 256).min(256);
                    expect(
                        req_i64(di, ib + 8, name)? as u64 == iv_offset as u64,
                        "interval offset",
                    )?;
                    expect(
                        req_i32(di, ib + 16, name)? as usize == iv_count,
                        "interval count",
                    )?;
                    // Recompute the pruning metadata from the covered records:
                    // first_line = start line of the first record, last_line =
                    // max end line across the interval. Tampering these could
                    // make symbol_at skip a real covering occurrence.
                    let first_line =
                        Span::unpack_line(req_u32(dp, dp_start_col + 4 * iv_offset, dp_name)?);
                    let mut last_line = 0u32;
                    for j in 0..iv_count {
                        let pe = req_u32(dp, dp_end_col + 4 * (iv_offset + j), dp_name)?;
                        last_line = last_line.max(Span::unpack_line(pe));
                    }
                    expect(
                        req_i32(di, ib, name)? == first_line as i32,
                        "interval first_line",
                    )?;
                    expect(
                        req_i32(di, ib + 4, name)? == last_line as i32,
                        "interval last_line",
                    )?;
                }
                iv_cursor += want_iv;
            }
            post_acc += post_count;
        }
        expect(post_acc == doc_recs, "doc postings total")?;
        expect(iv_cursor == interval_count, "interval total")?;
        Ok(())
    }

    /// Metadata + search string refs in-bounds, `def_doc_ord`/`symbol_ord` in
    /// range, and search rows sorted by `normalized_name`.
    fn validate_meta_and_search(
        &self,
        tmc: usize,
        tmb: usize,
        smc: usize,
        smb: usize,
        src: usize,
        srb: usize,
    ) -> Result<()> {
        let tm = self.file(TARGET_META);
        let tm_name = format::TARGET_META_FILE;
        let tblob = 16 + tmc * 48;
        for i in 0..tmc {
            let base = 16 + i * 48;
            for k in 0..4 {
                str_slice(
                    tm,
                    req_i32(tm, base + k * 8, tm_name)?,
                    req_i32(tm, base + k * 8 + 4, tm_name)?,
                    tblob,
                    tmb,
                    "target-meta str ref",
                )?;
            }
        }
        let sm = self.file(SYMBOL_META);
        let sm_name = format::SYMBOL_META_FILE;
        let sblob = 16 + smc * 48;
        for i in 0..smc {
            let base = 16 + i * 48;
            for k in 0..3 {
                str_slice(
                    sm,
                    req_i32(sm, base + k * 8, sm_name)?,
                    req_i32(sm, base + k * 8 + 4, sm_name)?,
                    sblob,
                    smb,
                    "symbol-meta str ref",
                )?;
            }
            check_ord_opt(
                req_i32(sm, base + 40, sm_name)?,
                self.doc_count as usize,
                "symbol-meta def_doc_ord",
            )?;
        }
        let se = self.file(SEARCH);
        let se_name = format::SEARCH_FILE;
        let seblob = 16 + src * 16;
        let mut prev: Option<&[u8]> = None;
        for i in 0..src {
            let base = 16 + i * 16;
            let nm = str_slice(
                se,
                req_i32(se, base, se_name)?,
                req_i32(se, base + 4, se_name)?,
                seblob,
                srb,
                "search name ref",
            )?;
            if let Some(p) = prev {
                expect(p <= nm, "search rows not sorted")?;
            }
            prev = Some(nm);
            check_ord(
                req_i32(se, base + 8, se_name)?,
                self.symbol_count,
                "search symbol_ord",
            )?;
        }
        Ok(())
    }
}

/// mmap a segment file read-only.
fn map_file(path: &Path) -> Result<Mmap> {
    let file = std::fs::File::open(path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            SegmentError::Truncated {
                file: path.display().to_string(),
            }
        } else {
            SegmentError::Io(e)
        }
    })?;
    // SAFETY: published segments are immutable after their atomic rename; the
    // mapping is only ever read.
    let mmap = unsafe { Mmap::map(&file)? };
    Ok(mmap)
}

fn verify_checksums(checksums: &[u8], mapped: &[Mmap]) -> Result<()> {
    let mismatch = |detail: String| SegmentError::ChecksumListMismatch { detail };
    let truncated = || SegmentError::Truncated {
        file: format::CHECKSUMS_FILE.into(),
    };
    let count = req_i64(checksums, 0, format::CHECKSUMS_FILE)?;
    if count < 0 || count as usize != format::CHECKSUMMED_FILES.len() {
        return Err(mismatch(format!("entry_count {count}")));
    }
    let mut off = 8usize;
    for (i, expected_name) in format::CHECKSUMMED_FILES.iter().enumerate() {
        let name_len = req_i32(checksums, off, format::CHECKSUMS_FILE)?;
        if name_len < 0 {
            return Err(mismatch(format!("entry {i} negative name_len {name_len}")));
        }
        // Checked cursor arithmetic so a malformed length can never overflow.
        let name_start = off.checked_add(4).ok_or_else(truncated)?;
        let name_end = name_start
            .checked_add(name_len as usize)
            .ok_or_else(truncated)?;
        let name = checksums.get(name_start..name_end).ok_or_else(truncated)?;
        if name != expected_name.as_bytes() {
            return Err(mismatch(format!(
                "entry {i} is {:?}, expected {expected_name}",
                String::from_utf8_lossy(name)
            )));
        }
        let stored = req_u64(checksums, name_end, format::CHECKSUMS_FILE)?;
        off = name_end.checked_add(8).ok_or_else(truncated)?;
        if crc32c(&mapped[i][..]) as u64 != stored {
            return Err(SegmentError::ChecksumMismatch {
                file: (*expected_name).into(),
            });
        }
    }
    // No trailing bytes may hide after the expected entries.
    if off != checksums.len() {
        return Err(mismatch(format!(
            "trailing bytes: {} of {}",
            off,
            checksums.len()
        )));
    }
    Ok(())
}

/// Read a `(offset,len)` string ref at `ref_off` into a materialized `String`,
/// where the blob begins at `blob_start`.
fn read_str(file: &[u8], ref_off: usize, blob_start: usize) -> String {
    let off = read_i32(file, ref_off).unwrap_or(0) as usize;
    let len = read_i32(file, ref_off + 4).unwrap_or(0) as usize;
    file.get(blob_start + off..blob_start + off + len)
        .map(|s| String::from_utf8_lossy(s).into_owned())
        .unwrap_or_default()
}

fn structural(detail: &str) -> SegmentError {
    SegmentError::Structural {
        detail: detail.into(),
    }
}

fn expect(cond: bool, detail: &str) -> Result<()> {
    if cond {
        Ok(())
    } else {
        Err(structural(detail))
    }
}

/// Bound a raw count to `[0, max]` so later size arithmetic cannot overflow or
/// go negative. `max` is the owning file length (each record is ≥ 1 byte).
fn bounded(v: i64, max: usize, detail: &str) -> Result<usize> {
    if v < 0 || v as u64 > max as u64 {
        Err(structural(detail))
    } else {
        Ok(v as usize)
    }
}

/// A required ordinal must be in `[0, count)`.
fn check_ord(v: i32, count: usize, detail: &str) -> Result<()> {
    if v >= 0 && (v as usize) < count {
        Ok(())
    } else {
        Err(structural(detail))
    }
}

/// An optional ordinal is `-1` or in `[0, count)`.
fn check_ord_opt(v: i32, count: usize, detail: &str) -> Result<()> {
    if v == -1 || (v >= 0 && (v as usize) < count) {
        Ok(())
    } else {
        Err(structural(detail))
    }
}

/// Validate a `(offset, len)` string ref against its blob and return the bytes.
fn str_slice<'a>(
    file: &'a [u8],
    off: i32,
    len: i32,
    blob_start: usize,
    blob_len: usize,
    detail: &'static str,
) -> Result<&'a [u8]> {
    if off < 0 || len < 0 {
        return Err(structural(detail));
    }
    let (off, len) = (off as usize, len as usize);
    if off + len > blob_len {
        return Err(structural(detail));
    }
    let start = blob_start + off;
    file.get(start..start + len)
        .ok_or_else(|| structural(detail))
}

fn req_i64(b: &[u8], off: usize, file: &str) -> Result<i64> {
    read_i64(b, off).ok_or_else(|| SegmentError::Truncated { file: file.into() })
}
fn req_u64(b: &[u8], off: usize, file: &str) -> Result<u64> {
    read_u64(b, off).ok_or_else(|| SegmentError::Truncated { file: file.into() })
}
fn req_i32(b: &[u8], off: usize, file: &str) -> Result<i32> {
    read_i32(b, off).ok_or_else(|| SegmentError::Truncated { file: file.into() })
}
fn req_u32(b: &[u8], off: usize, file: &str) -> Result<u32> {
    read_u32(b, off).ok_or_else(|| SegmentError::Truncated { file: file.into() })
}
