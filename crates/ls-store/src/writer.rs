//! Segment writer — builds the fifteen segment files in memory and publishes
//! them durably (fsync each file, fsync tmp dir, atomic rename, fsync
//! `segments/`), a faithful port of `ls.postings.SegmentWriter`.

use std::path::{Path, PathBuf};

use ls_index_model::{occ_flags, Span};

use crate::crc::crc32c;
use crate::data::{RenameProfile, SegmentData};
use crate::error::{Result, SegmentError};
use crate::format::{self, target_word_count, BLOCK_SIZE};
use crate::wire::LeBuf;

/// One `GroupIndexEntry` (16 bytes on disk).
struct GroupIndexEntry {
    offset: i64,
    count: i32,
    block_index_offset: i32,
}

/// One `BlockEntry` (40 + 8W bytes on disk).
struct BlockEntry {
    first_record: i64,
    record_count: i32,
    editable_count: i32,
    ref_role_count: i32,
    def_role_count: i32,
    doc_ord_min: i32,
    doc_ord_max: i32,
    epoch_min: i32,
    epoch_max: i32,
    target_words: Vec<u64>,
}

/// The columnar postings + group index produced for one of the three group
/// postings files.
#[derive(Default)]
struct GroupLayout {
    doc_ord: Vec<i32>,
    doc_epoch: Vec<i32>,
    target_ord: Vec<i32>,
    packed_start: Vec<i32>,
    packed_end: Vec<i32>,
    flags: Vec<i32>,
    entries: Vec<GroupIndexEntry>,
}

/// Writes segments to a postings root.
pub struct SegmentWriter;

impl SegmentWriter {
    /// Build and durably publish a segment for `data`, returning the final
    /// `segments/segment-NNNNNN` directory.
    pub fn write(
        root: &Path,
        segment_id: u64,
        data: &SegmentData,
        created_at_ms: i64,
    ) -> Result<PathBuf> {
        validate(data)?;

        let target_count = data.targets.len();
        let words = target_word_count(target_count);

        // Symbol dictionary sorted by UTF-8 bytes; `caller_to_sorted[c]` is the
        // on-disk ordinal of caller symbol `c`.
        let sorted_indices = sort_symbol_indices(data)?;
        let mut caller_to_sorted = vec![0i32; data.symbols.len()];
        for (sorted_pos, &caller) in sorted_indices.iter().enumerate() {
            caller_to_sorted[caller] = sorted_pos as i32;
        }

        // Group postings + the shared block index (ref, then def, then rename).
        let mut blocks: Vec<BlockEntry> = Vec::new();
        let ref_layout = layout_groups(&data.ref_occurrences, words, &mut blocks);
        let def_layout = layout_groups(&data.def_occurrences, words, &mut blocks);
        let rename_layout = layout_groups(&data.rename_occurrences, words, &mut blocks);

        let ref_postings = serialize_group_postings(&ref_layout);
        let def_postings = serialize_group_postings(&def_layout);
        let rename_postings = serialize_group_postings(&rename_layout);
        let block_index = serialize_block_index(&blocks, words);

        let ref_group_index = serialize_group_index(&ref_layout.entries, None);
        let def_group_index = serialize_group_index(&def_layout.entries, None);
        let rename_group_index =
            serialize_group_index(&rename_layout.entries, Some(&data.rename_profiles));

        let (doc_index, doc_postings, doc_record_count) = build_doc_files(data, &caller_to_sorted);
        let symbol_index = serialize_symbol_index(data, &sorted_indices);
        let target_meta = serialize_target_meta(data);
        let symbol_meta = serialize_symbol_meta(data, &sorted_indices);
        let search = serialize_search(data);

        let occurrence_count = ref_layout.doc_ord.len()
            + def_layout.doc_ord.len()
            + rename_layout.doc_ord.len()
            + doc_record_count;
        let header = serialize_header(
            segment_id,
            created_at_ms,
            data.ref_occurrences.len() as u64,
            data.rename_occurrences.len() as u64,
            data.docs.len() as u64,
            occurrence_count as u64,
        );

        // Canonical checksummed order (matches format::CHECKSUMMED_FILES).
        let files: Vec<(&str, Vec<u8>)> = vec![
            (format::HEADER_FILE, header),
            (format::REF_GROUP_INDEX_FILE, ref_group_index),
            (format::DEF_GROUP_INDEX_FILE, def_group_index),
            (format::RENAME_GROUP_INDEX_FILE, rename_group_index),
            (format::DOC_INDEX_FILE, doc_index),
            (format::SYMBOL_INDEX_FILE, symbol_index),
            (format::REF_POSTINGS_FILE, ref_postings),
            (format::DEF_POSTINGS_FILE, def_postings),
            (format::RENAME_POSTINGS_FILE, rename_postings),
            (format::DOC_POSTINGS_FILE, doc_postings),
            (format::BLOCK_INDEX_FILE, block_index),
            (format::TARGET_META_FILE, target_meta),
            (format::SYMBOL_META_FILE, symbol_meta),
            (format::SEARCH_FILE, search),
        ];
        let checksums = serialize_checksums(&files);

        publish(root, segment_id, &files, &checksums)
    }
}

/// Validate the writer input: structural parallelism, non-empty URIs, and every
/// ordinal + span in range — so layout can index `caller_to_sorted` /
/// `target_words` without panicking. Malformed input returns `InvalidInput`.
fn validate(data: &SegmentData) -> Result<()> {
    let invalid = |detail: &str| {
        Err(SegmentError::InvalidInput {
            detail: detail.into(),
        })
    };
    if data.def_occurrences.len() != data.ref_occurrences.len() {
        return invalid("def/ref group counts differ");
    }
    if data.rename_profiles.len() != data.rename_occurrences.len() {
        return invalid("rename profile/group counts differ");
    }
    if data.doc_occurrences.len() != data.docs.len() {
        return invalid("doc occurrence/doc counts differ");
    }
    if !data.target_meta.is_empty() && data.target_meta.len() != data.targets.len() {
        return invalid("target_meta must be empty or parallel to targets");
    }
    if !data.symbol_meta.is_empty() && data.symbol_meta.len() != data.symbols.len() {
        return invalid("symbol_meta must be empty or parallel to symbols");
    }

    let n_docs = data.docs.len();
    let n_targets = data.targets.len();
    let n_symbols = data.symbols.len();
    let n_ref = data.ref_occurrences.len();
    let n_rename = data.rename_occurrences.len();

    for doc in &data.docs {
        if doc.uri.is_empty() {
            return invalid("empty doc uri");
        }
        if !ord_in(doc.target_ord, n_targets) {
            return invalid("doc target_ord out of range");
        }
    }
    for groups in [
        &data.ref_occurrences,
        &data.def_occurrences,
        &data.rename_occurrences,
    ] {
        for group in groups {
            for occ in group {
                if !ord_in(occ.doc_ord, n_docs) {
                    return invalid("occurrence doc_ord out of range");
                }
                if !ord_in(occ.target_ord, n_targets) {
                    return invalid("occurrence target_ord out of range");
                }
                if !span_ok(&occ.span) {
                    return invalid("occurrence span out of range");
                }
            }
        }
    }
    for doc_occs in &data.doc_occurrences {
        for occ in doc_occs {
            if !ord_in(occ.symbol_ord, n_symbols) {
                return invalid("doc occurrence symbol_ord out of range");
            }
            if !span_ok(&occ.span) {
                return invalid("doc occurrence span out of range");
            }
        }
    }
    for s in &data.symbols {
        if !ord_opt(s.ref_group_ord, n_ref) {
            return invalid("symbol ref_group_ord out of range");
        }
        if !ord_opt(s.rename_group_ord, n_rename) {
            return invalid("symbol rename_group_ord out of range");
        }
        if !ord_opt(s.def_target_ord, n_targets) {
            return invalid("symbol def_target_ord out of range");
        }
    }
    for row in &data.search_rows {
        if !ord_in(row.symbol_ord, n_symbols) {
            return invalid("search row symbol_ord out of range");
        }
    }
    for m in &data.symbol_meta {
        if !ord_opt(m.def_doc_ord, n_docs) {
            return invalid("symbol_meta def_doc_ord out of range");
        }
    }
    Ok(())
}

fn ord_in(v: i32, n: usize) -> bool {
    v >= 0 && (v as usize) < n
}

fn ord_opt(v: i32, n: usize) -> bool {
    v == -1 || ord_in(v, n)
}

/// A span's coordinates must fit the packed representation (char < 2^12, line ≤
/// `LINE_MAX`) and be non-inverted (`packed_start <= packed_end`).
fn span_ok(s: &Span) -> bool {
    s.start_char <= Span::CHAR_MASK
        && s.end_char <= Span::CHAR_MASK
        && s.start_line <= Span::LINE_MAX
        && s.end_line <= Span::LINE_MAX
        && Span::pack(s.start_line, s.start_char) <= Span::pack(s.end_line, s.end_char)
}

/// Indices of `data.symbols` sorted by unsigned UTF-8 byte comparison; rejects
/// duplicate symbol strings.
fn sort_symbol_indices(data: &SegmentData) -> Result<Vec<usize>> {
    let mut indices: Vec<usize> = (0..data.symbols.len()).collect();
    indices.sort_by(|&a, &b| {
        data.symbols[a]
            .semantic_symbol
            .as_bytes()
            .cmp(data.symbols[b].semantic_symbol.as_bytes())
    });
    for pair in indices.windows(2) {
        if data.symbols[pair[0]].semantic_symbol == data.symbols[pair[1]].semantic_symbol {
            return Err(SegmentError::InvalidInput {
                detail: format!(
                    "duplicate symbol string {:?}",
                    data.symbols[pair[0]].semantic_symbol
                ),
            });
        }
    }
    Ok(indices)
}

/// Lay out one role's groups into columnar postings, appending skip blocks to
/// the shared block index.
fn layout_groups(
    groups: &[Vec<crate::data::GroupOcc>],
    words: usize,
    blocks: &mut Vec<BlockEntry>,
) -> GroupLayout {
    let mut layout = GroupLayout::default();
    for group in groups {
        let group_start = layout.doc_ord.len() as i64;
        if group.is_empty() {
            layout.entries.push(GroupIndexEntry {
                offset: group_start,
                count: 0,
                block_index_offset: -1,
            });
            continue;
        }
        let block_start = blocks.len() as i32;
        layout.entries.push(GroupIndexEntry {
            offset: group_start,
            count: group.len() as i32,
            block_index_offset: block_start,
        });

        let mut sorted: Vec<&crate::data::GroupOcc> = group.iter().collect();
        sorted.sort_by_key(|o| {
            (
                o.doc_ord,
                Span::pack(o.span.start_line, o.span.start_char),
                Span::pack(o.span.end_line, o.span.end_char),
            )
        });

        for chunk in sorted.chunks(BLOCK_SIZE as usize) {
            let first_record = layout.doc_ord.len() as i64;
            let mut block = BlockEntry {
                first_record,
                record_count: chunk.len() as i32,
                editable_count: 0,
                ref_role_count: 0,
                def_role_count: 0,
                doc_ord_min: i32::MAX,
                doc_ord_max: i32::MIN,
                epoch_min: i32::MAX,
                epoch_max: i32::MIN,
                target_words: vec![0u64; words],
            };
            for occ in chunk {
                layout.doc_ord.push(occ.doc_ord);
                layout.doc_epoch.push(occ.doc_epoch);
                layout.target_ord.push(occ.target_ord);
                layout
                    .packed_start
                    .push(Span::pack(occ.span.start_line, occ.span.start_char) as i32);
                layout
                    .packed_end
                    .push(Span::pack(occ.span.end_line, occ.span.end_char) as i32);
                layout.flags.push(occ.flags);

                let flags = occ.flags as u32;
                if occ_flags::has(flags, occ_flags::EDITABLE) {
                    block.editable_count += 1;
                }
                if occ_flags::has(flags, occ_flags::DEFINITION) {
                    block.def_role_count += 1;
                } else {
                    block.ref_role_count += 1;
                }
                block.doc_ord_min = block.doc_ord_min.min(occ.doc_ord);
                block.doc_ord_max = block.doc_ord_max.max(occ.doc_ord);
                block.epoch_min = block.epoch_min.min(occ.doc_epoch);
                block.epoch_max = block.epoch_max.max(occ.doc_epoch);
                if occ.target_ord >= 0 {
                    let t = occ.target_ord as usize;
                    block.target_words[t >> 6] |= 1u64 << (t & 63);
                }
            }
            blocks.push(block);
        }
    }
    layout
}

fn serialize_group_postings(layout: &GroupLayout) -> Vec<u8> {
    let n = layout.doc_ord.len();
    let mut buf = LeBuf::with_capacity(8 + n * 24);
    buf.put_i64(n as i64);
    for col in [
        &layout.doc_ord,
        &layout.doc_epoch,
        &layout.target_ord,
        &layout.packed_start,
        &layout.packed_end,
        &layout.flags,
    ] {
        for &v in col {
            buf.put_i32(v);
        }
    }
    buf.into_vec()
}

fn serialize_group_index(
    entries: &[GroupIndexEntry],
    profiles: Option<&[RenameProfile]>,
) -> Vec<u8> {
    let mut buf = LeBuf::with_capacity(8 + entries.len() * 16);
    buf.put_i64(entries.len() as i64);
    for e in entries {
        buf.put_i64(e.offset);
        buf.put_i32(e.count);
        buf.put_i32(e.block_index_offset);
    }
    if let Some(profiles) = profiles {
        for p in profiles {
            buf.put_i32(profile_flags(p));
            buf.put_i32(p.editable_occurrence_count);
            buf.put_i64(p.unsafe_reason_mask);
        }
    }
    buf.into_vec()
}

fn profile_flags(p: &RenameProfile) -> i32 {
    use format::prof_flags::*;
    let mut f = 0;
    if p.is_local {
        f |= IS_LOCAL;
    }
    if p.is_external {
        f |= IS_EXTERNAL;
    }
    if p.has_generated_occurrences {
        f |= HAS_GENERATED;
    }
    if p.has_readonly_occurrences {
        f |= HAS_READONLY;
    }
    if p.has_override_family {
        f |= HAS_OVERRIDE_FAMILY;
    }
    if p.has_companion {
        f |= HAS_COMPANION;
    }
    f
}

fn serialize_block_index(blocks: &[BlockEntry], words: usize) -> Vec<u8> {
    let mut buf = LeBuf::with_capacity(16 + blocks.len() * (40 + 8 * words));
    buf.put_i64(blocks.len() as i64);
    buf.put_i32(words as i32);
    buf.put_i32(BLOCK_SIZE);
    for b in blocks {
        buf.put_i64(b.first_record);
        buf.put_i32(b.record_count);
        buf.put_i32(b.editable_count);
        buf.put_i32(b.ref_role_count);
        buf.put_i32(b.def_role_count);
        buf.put_i32(b.doc_ord_min);
        buf.put_i32(b.doc_ord_max);
        buf.put_i32(b.epoch_min);
        buf.put_i32(b.epoch_max);
        for &w in &b.target_words {
            buf.put_u64(w);
        }
    }
    buf.into_vec()
}

/// One `DocEntry` (48 bytes on disk) accumulated while laying out doc postings.
struct DocEntryRow {
    uri_offset: i32,
    uri_len: i32,
    doc_id: i64,
    epoch: i32,
    target_ord: i32,
    doc_flags: i32,
    interval_first: i32,
    postings_offset: i64,
    postings_count: i32,
    interval_count: i32,
}

fn build_doc_files(data: &SegmentData, caller_to_sorted: &[i32]) -> (Vec<u8>, Vec<u8>, usize) {
    let mut symbol_ord: Vec<i32> = Vec::new();
    let mut packed_start: Vec<i32> = Vec::new();
    let mut packed_end: Vec<i32> = Vec::new();
    let mut flags: Vec<i32> = Vec::new();

    // (first_line, last_line, offset, count)
    let mut intervals: Vec<(i32, i32, i64, i32)> = Vec::new();
    let mut docs: Vec<DocEntryRow> = Vec::new();
    let mut uri_blob: Vec<u8> = Vec::new();

    for (doc_ord, doc) in data.docs.iter().enumerate() {
        let uri_offset = uri_blob.len() as i32;
        uri_blob.extend_from_slice(doc.uri.as_bytes());
        let uri_len = doc.uri.len() as i32;

        let mut doc_flags = 0;
        if doc.generated {
            doc_flags |= format::doc_flags::GENERATED;
        }
        if doc.readonly {
            doc_flags |= format::doc_flags::READONLY;
        }

        let postings_offset = symbol_ord.len() as i64;
        let occs = &data.doc_occurrences[doc_ord];
        if occs.is_empty() {
            docs.push(DocEntryRow {
                uri_offset,
                uri_len,
                doc_id: doc.doc_id,
                epoch: doc.epoch,
                target_ord: doc.target_ord,
                doc_flags,
                interval_first: -1,
                postings_offset,
                postings_count: 0,
                interval_count: 0,
            });
            continue;
        }

        let interval_first = intervals.len() as i32;
        let mut sorted: Vec<&crate::data::DocOcc> = occs.iter().collect();
        sorted.sort_by_key(|o| {
            (
                Span::pack(o.span.start_line, o.span.start_char),
                Span::pack(o.span.end_line, o.span.end_char),
            )
        });

        let mut interval_count = 0;
        for chunk in sorted.chunks(BLOCK_SIZE as usize) {
            let offset = symbol_ord.len() as i64;
            let first_line = chunk[0].span.start_line as i32;
            let mut last_line = i32::MIN;
            for occ in chunk {
                symbol_ord.push(caller_to_sorted[occ.symbol_ord as usize]);
                packed_start.push(Span::pack(occ.span.start_line, occ.span.start_char) as i32);
                packed_end.push(Span::pack(occ.span.end_line, occ.span.end_char) as i32);
                flags.push(occ.flags);
                last_line = last_line.max(occ.span.end_line as i32);
            }
            intervals.push((first_line, last_line, offset, chunk.len() as i32));
            interval_count += 1;
        }
        let postings_count = (symbol_ord.len() as i64 - postings_offset) as i32;
        docs.push(DocEntryRow {
            uri_offset,
            uri_len,
            doc_id: doc.doc_id,
            epoch: doc.epoch,
            target_ord: doc.target_ord,
            doc_flags,
            interval_first,
            postings_offset,
            postings_count,
            interval_count,
        });
    }

    // doc-index.bin
    let mut idx =
        LeBuf::with_capacity(24 + docs.len() * 48 + intervals.len() * 24 + uri_blob.len());
    idx.put_i64(docs.len() as i64);
    idx.put_i64(intervals.len() as i64);
    idx.put_i64(uri_blob.len() as i64);
    for d in &docs {
        idx.put_i32(d.uri_offset);
        idx.put_i32(d.uri_len);
        idx.put_i64(d.doc_id);
        idx.put_i32(d.epoch);
        idx.put_i32(d.target_ord);
        idx.put_i32(d.doc_flags);
        idx.put_i32(d.interval_first);
        idx.put_i64(d.postings_offset);
        idx.put_i32(d.postings_count);
        idx.put_i32(d.interval_count);
    }
    for &(fl, ll, off, cnt) in &intervals {
        idx.put_i32(fl);
        idx.put_i32(ll);
        idx.put_i64(off);
        idx.put_i32(cnt);
        idx.put_i32(0); // pad
    }
    idx.put_bytes(&uri_blob);

    // doc-postings.bin
    let n = symbol_ord.len();
    let mut post = LeBuf::with_capacity(8 + n * 16);
    post.put_i64(n as i64);
    for col in [&symbol_ord, &packed_start, &packed_end, &flags] {
        for &v in col {
            post.put_i32(v);
        }
    }

    (idx.into_vec(), post.into_vec(), n)
}

fn serialize_symbol_index(data: &SegmentData, sorted_indices: &[usize]) -> Vec<u8> {
    let mut blob: Vec<u8> = Vec::new();
    let mut entries: Vec<(i32, i32, i64, i32, i32, i32)> = Vec::with_capacity(sorted_indices.len());
    for &caller in sorted_indices {
        let s = &data.symbols[caller];
        let off = blob.len() as i32;
        blob.extend_from_slice(s.semantic_symbol.as_bytes());
        entries.push((
            off,
            s.semantic_symbol.len() as i32,
            s.symbol_id,
            s.ref_group_ord,
            s.rename_group_ord,
            s.def_target_ord,
        ));
    }
    let mut buf =
        LeBuf::with_capacity(24 + entries.len() * 32 + data.targets.len() * 8 + blob.len());
    buf.put_i64(entries.len() as i64);
    buf.put_i64(data.targets.len() as i64);
    buf.put_i64(blob.len() as i64);
    for &(off, len, id, rg, rn, dt) in &entries {
        buf.put_i32(off);
        buf.put_i32(len);
        buf.put_i64(id);
        buf.put_i32(rg);
        buf.put_i32(rn);
        buf.put_i32(dt);
        buf.put_i32(0); // pad
    }
    for &tid in &data.targets {
        buf.put_i64(tid);
    }
    buf.put_bytes(&blob);
    buf.into_vec()
}

fn serialize_target_meta(data: &SegmentData) -> Vec<u8> {
    let count = data.targets.len();
    let mut blob: Vec<u8> = Vec::new();
    let mut entries: Vec<[i32; 8]> = Vec::with_capacity(count);
    let mut hashes: Vec<(i64, i64)> = Vec::with_capacity(count);
    for i in 0..count {
        let m = data.target_meta.get(i).cloned().unwrap_or_default();
        let refs = [
            push_str(&mut blob, &m.bsp_id),
            push_str(&mut blob, &m.scala_version),
            push_str(&mut blob, &m.sourceroot),
            push_str(&mut blob, &m.semanticdb_root),
        ];
        entries.push([
            refs[0].0, refs[0].1, refs[1].0, refs[1].1, refs[2].0, refs[2].1, refs[3].0, refs[3].1,
        ]);
        hashes.push((m.content_hash, m.options_hash));
    }
    let mut buf = LeBuf::with_capacity(16 + count * 48 + blob.len());
    buf.put_i64(count as i64);
    buf.put_i64(blob.len() as i64);
    for (e, (ch, oh)) in entries.iter().zip(&hashes) {
        for &v in e {
            buf.put_i32(v);
        }
        buf.put_i64(*ch);
        buf.put_i64(*oh);
    }
    buf.put_bytes(&blob);
    buf.into_vec()
}

fn serialize_symbol_meta(data: &SegmentData, sorted_indices: &[usize]) -> Vec<u8> {
    let count = sorted_indices.len();
    let mut blob: Vec<u8> = Vec::new();
    let mut buf = LeBuf::with_capacity(16 + count * 48);
    // Two passes: build the entry table over the blob, then assemble.
    let mut rows: Vec<u8> = Vec::with_capacity(count * 48);
    let mut rowbuf = LeBuf::with_capacity(count * 48);
    for &caller in sorted_indices {
        let m = data.symbol_meta.get(caller).cloned().unwrap_or_default();
        let (do_, dl) = push_str(&mut blob, &m.display);
        let (oo, ol) = push_str(&mut blob, &m.owner);
        let (po, pl) = push_str(&mut blob, &m.package_name);
        rowbuf.put_i32(do_);
        rowbuf.put_i32(dl);
        rowbuf.put_i32(oo);
        rowbuf.put_i32(ol);
        rowbuf.put_i32(po);
        rowbuf.put_i32(pl);
        rowbuf.put_i32(m.kind);
        rowbuf.put_u32(m.properties);
        rowbuf.put_i32(m.def_packed_start);
        rowbuf.put_i32(m.def_packed_end);
        rowbuf.put_i32(m.def_doc_ord);
        rowbuf.put_i32(0); // pad
    }
    rows.extend_from_slice(rowbuf.as_slice());
    buf.put_i64(count as i64);
    buf.put_i64(blob.len() as i64);
    buf.put_bytes(&rows);
    buf.put_bytes(&blob);
    buf.into_vec()
}

fn serialize_search(data: &SegmentData) -> Vec<u8> {
    let mut rows: Vec<&crate::data::SearchRow> = data.search_rows.iter().collect();
    rows.sort_by(|a, b| {
        a.normalized_name
            .as_bytes()
            .cmp(b.normalized_name.as_bytes())
    });
    let mut blob: Vec<u8> = Vec::new();
    let mut rowbuf = LeBuf::with_capacity(rows.len() * 16);
    for r in &rows {
        let (off, len) = push_str(&mut blob, &r.normalized_name);
        rowbuf.put_i32(off);
        rowbuf.put_i32(len);
        rowbuf.put_i32(r.symbol_ord);
        rowbuf.put_i32(0); // pad
    }
    let mut buf = LeBuf::with_capacity(16 + rows.len() * 16 + blob.len());
    buf.put_i64(rows.len() as i64);
    buf.put_i64(blob.len() as i64);
    buf.put_bytes(rowbuf.as_slice());
    buf.put_bytes(&blob);
    buf.into_vec()
}

/// Append `s` to `blob`, returning its `(offset, len)`.
fn push_str(blob: &mut Vec<u8>, s: &str) -> (i32, i32) {
    let off = blob.len() as i32;
    blob.extend_from_slice(s.as_bytes());
    (off, s.len() as i32)
}

fn serialize_header(
    segment_id: u64,
    created_at_ms: i64,
    ref_group_count: u64,
    rename_group_count: u64,
    doc_count: u64,
    occurrence_count: u64,
) -> Vec<u8> {
    let mut buf = LeBuf::with_capacity(format::HEADER_SIZE);
    buf.put_u32(format::MAGIC);
    buf.put_u16(format::VERSION);
    buf.put_u16(0); // flags
    buf.put_u64(segment_id);
    buf.put_i64(created_at_ms);
    buf.put_u64(ref_group_count);
    buf.put_u64(rename_group_count);
    buf.put_u64(doc_count);
    buf.put_u64(occurrence_count);
    buf.put_u64(0); // checksum placeholder
    debug_assert_eq!(buf.as_slice().len(), format::HEADER_SIZE);
    let checksum = crc32c(&buf.as_slice()[..format::HEADER_CHECKSUM_OFFSET]);
    buf.set_u64(format::HEADER_CHECKSUM_OFFSET, checksum as u64);
    buf.into_vec()
}

fn serialize_checksums(files: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut buf = LeBuf::default();
    buf.put_i64(files.len() as i64);
    for (name, bytes) in files {
        buf.put_i32(name.len() as i32);
        buf.put_bytes(name.as_bytes());
        buf.put_u64(crc32c(bytes) as u64);
    }
    buf.into_vec()
}

/// Write all files into `tmp-<id>`, fsync, then atomically rename into
/// `segments/segment-NNNNNN` and fsync `segments/`.
fn publish(
    root: &Path,
    segment_id: u64,
    files: &[(&str, Vec<u8>)],
    checksums: &[u8],
) -> Result<PathBuf> {
    use std::fs;
    use std::io::Write;

    let segments_dir = root.join("segments");
    fs::create_dir_all(&segments_dir)?;
    let tmp_dir = root.join(format!("tmp-{segment_id}"));
    if tmp_dir.exists() {
        fs::remove_dir_all(&tmp_dir)?;
    }
    fs::create_dir_all(&tmp_dir)?;

    let write_file = |name: &str, bytes: &[u8]| -> Result<()> {
        let path = tmp_dir.join(name);
        let mut f = fs::File::create(&path)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        Ok(())
    };
    for (name, bytes) in files {
        write_file(name, bytes)?;
    }
    write_file(format::CHECKSUMS_FILE, checksums)?;
    fsync_dir(&tmp_dir)?;

    let final_dir = segments_dir.join(format::segment_dir_name(segment_id));
    if final_dir.exists() {
        fs::remove_dir_all(&final_dir)?;
    }
    fs::rename(&tmp_dir, &final_dir)?;
    fsync_dir(&segments_dir)?;
    Ok(final_dir)
}

fn fsync_dir(dir: &Path) -> Result<()> {
    std::fs::File::open(dir)?.sync_all()?;
    Ok(())
}
