//! Segment reader — mmaps a published segment, validates it whole at open time
//! (magic, version, header self-checksum, every file CRC, structural
//! cross-checks) and exposes typed accessors plus the documented group/doc scan
//! obligations. A port of the read/validate half of `ls.postings.SegmentReader`.

use std::path::Path;

use memmap2::Mmap;

use ls_index_model::{occ_flags, TargetBitset};

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

    // ---- structural validation ----

    fn validate_structure(mut self) -> Result<SegmentReader> {
        // symbol-index counts anchor the target/symbol cross-checks.
        let sym = self.file(SYMBOL_INDEX);
        let symbol_count = req_i64(sym, 0, format::SYMBOL_INDEX_FILE)? as usize;
        let target_count = req_i64(sym, 8, format::SYMBOL_INDEX_FILE)? as usize;
        let sym_blob = req_i64(sym, 16, format::SYMBOL_INDEX_FILE)? as usize;
        expect(
            sym.len() == 24 + symbol_count * 32 + target_count * 8 + sym_blob,
            "symbol-index size",
        )?;
        self.symbol_count = symbol_count;
        self.target_count = target_count;
        self.words = target_word_count(target_count);

        // group indices.
        self.check_group_index(REF_GROUP_INDEX, self.ref_group_count as usize, false)?;
        self.check_group_index(DEF_GROUP_INDEX, self.ref_group_count as usize, false)?;
        self.check_group_index(RENAME_GROUP_INDEX, self.rename_group_count as usize, true)?;

        // group postings (6 columns) + accumulate occurrence count.
        let ref_recs = self.check_columns(REF_POSTINGS, 6)?;
        let def_recs = self.check_columns(DEF_POSTINGS, 6)?;
        let rename_recs = self.check_columns(RENAME_POSTINGS, 6)?;
        let doc_recs = self.check_columns(DOC_POSTINGS, 4)?;
        expect(
            (ref_recs + def_recs + rename_recs + doc_recs) as u64 == self.occurrence_count,
            "occurrence_count sum",
        )?;

        // doc-index.
        let di = self.file(DOC_INDEX);
        let dc = req_i64(di, 0, format::DOC_INDEX_FILE)? as usize;
        let ic = req_i64(di, 8, format::DOC_INDEX_FILE)? as usize;
        let ub = req_i64(di, 16, format::DOC_INDEX_FILE)? as usize;
        expect(dc == self.doc_count as usize, "doc-index doc_count")?;
        expect(di.len() == 24 + dc * 48 + ic * 24 + ub, "doc-index size")?;

        // block-index.
        let bi = self.file(BLOCK_INDEX);
        let bc = req_i64(bi, 0, format::BLOCK_INDEX_FILE)? as usize;
        let wc = req_i32(bi, 8, format::BLOCK_INDEX_FILE)? as usize;
        let bs = req_i32(bi, 12, format::BLOCK_INDEX_FILE)?;
        expect(wc == self.words, "block target_word_count")?;
        expect(bs == format::BLOCK_SIZE, "block_size")?;
        expect(bi.len() == 16 + bc * (40 + 8 * wc), "block-index size")?;

        // extension sections.
        let tm = self.file(TARGET_META);
        let tmc = req_i64(tm, 0, format::TARGET_META_FILE)? as usize;
        let tmb = req_i64(tm, 8, format::TARGET_META_FILE)? as usize;
        expect(tmc == target_count, "target-meta count")?;
        expect(tm.len() == 16 + tmc * 48 + tmb, "target-meta size")?;

        let sm = self.file(SYMBOL_META);
        let smc = req_i64(sm, 0, format::SYMBOL_META_FILE)? as usize;
        let smb = req_i64(sm, 8, format::SYMBOL_META_FILE)? as usize;
        expect(smc == symbol_count, "symbol-meta count")?;
        expect(sm.len() == 16 + smc * 48 + smb, "symbol-meta size")?;

        let se = self.file(SEARCH);
        let src = req_i64(se, 0, format::SEARCH_FILE)? as usize;
        let srb = req_i64(se, 8, format::SEARCH_FILE)? as usize;
        expect(se.len() == 16 + src * 16 + srb, "search size")?;

        // Cache per-doc epochs for scan filtering.
        let mut doc_epochs = Vec::with_capacity(self.doc_count as usize);
        for d in 0..self.doc_count {
            doc_epochs.push(self.epoch_of(d));
        }
        self.doc_epochs = doc_epochs;
        Ok(self)
    }

    fn check_group_index(&self, idx: usize, count: usize, with_profiles: bool) -> Result<()> {
        let file = self.file(idx);
        let name = format::CHECKSUMMED_FILES[idx];
        let declared = req_i64(file, 0, name)? as usize;
        expect(declared == count, "group index count")?;
        let mut need = 8 + count * format::GROUP_INDEX_ENTRY_SIZE;
        if with_profiles {
            need += count * format::RENAME_PROFILE_ENTRY_SIZE;
        }
        expect(file.len() == need, "group index size")?;
        Ok(())
    }

    fn check_columns(&self, idx: usize, columns: usize) -> Result<i64> {
        let file = self.file(idx);
        let name = format::CHECKSUMMED_FILES[idx];
        let n = req_i64(file, 0, name)?;
        expect(
            file.len() == 8 + columns * 4 * n as usize,
            "postings columns size",
        )?;
        Ok(n)
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
    let count = req_i64(checksums, 0, format::CHECKSUMS_FILE)?;
    if count as usize != format::CHECKSUMMED_FILES.len() {
        return Err(SegmentError::ChecksumListMismatch {
            detail: format!("entry_count {count}"),
        });
    }
    let mut off = 8usize;
    for (i, expected_name) in format::CHECKSUMMED_FILES.iter().enumerate() {
        let name_len = req_i32(checksums, off, format::CHECKSUMS_FILE)? as usize;
        off += 4;
        let name = checksums
            .get(off..off + name_len)
            .ok_or_else(|| SegmentError::Truncated {
                file: format::CHECKSUMS_FILE.into(),
            })?;
        if name != expected_name.as_bytes() {
            return Err(SegmentError::ChecksumListMismatch {
                detail: format!(
                    "entry {i} is {:?}, expected {expected_name}",
                    String::from_utf8_lossy(name)
                ),
            });
        }
        off += name_len;
        let stored = req_u64(checksums, off, format::CHECKSUMS_FILE)?;
        off += 8;
        if crc32c(&mapped[i][..]) as u64 != stored {
            return Err(SegmentError::ChecksumMismatch {
                file: (*expected_name).into(),
            });
        }
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

fn expect(cond: bool, detail: &str) -> Result<()> {
    if cond {
        Ok(())
    } else {
        Err(SegmentError::Structural {
            detail: detail.into(),
        })
    }
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
