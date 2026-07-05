//! Round-trip and validation tests for the `ls-store` v1 segment format.
//!
//! Every persisted record shape is written and read back; the scan obligations
//! (target/epoch/editable filters, block skip) are exercised; and every
//! corruption class rejects the whole segment with a typed error.

use std::path::{Path, PathBuf};

use ls_index_model::{occ_flags, sym_props, Span};
use ls_store::{
    DocOcc, GroupOcc, GroupRecord, RenameProfile, SearchRow, SegmentData, SegmentDoc,
    SegmentReader, SegmentSymbol, SymbolMeta, TargetMeta,
};

const CREATED_AT: i64 = 1_700_000_000_000;
const SEGMENT_ID: u64 = 1;

fn ed() -> i32 {
    occ_flags::EDITABLE as i32
}
fn def() -> i32 {
    occ_flags::DEFINITION as i32
}

fn span(sl: u32, sc: u32, el: u32, ec: u32) -> Span {
    Span::new(sl, sc, el, ec)
}
fn gocc(doc_ord: i32, epoch: i32, target: i32, s: Span, flags: i32) -> GroupOcc {
    GroupOcc {
        doc_ord,
        doc_epoch: epoch,
        target_ord: target,
        span: s,
        flags,
    }
}

/// A hand-built corpus exercising: out-of-order symbols (sort remap), an empty
/// def group, an empty doc, a stale-epoch occurrence, editable/non-editable
/// renames, and the metadata + search sections.
fn base_corpus() -> SegmentData {
    let docs = vec![
        SegmentDoc {
            uri: "file:///A.scala".into(),
            doc_id: 100,
            epoch: 5,
            target_ord: 0,
            generated: false,
            readonly: false,
        },
        SegmentDoc {
            uri: "file:///B.scala".into(),
            doc_id: 101,
            epoch: 5,
            target_ord: 1,
            generated: true,
            readonly: false,
        },
        SegmentDoc {
            uri: "file:///C.scala".into(),
            doc_id: 102,
            epoch: 7,
            target_ord: 2,
            generated: false,
            readonly: true,
        },
    ];
    // Caller order is NOT sorted: "b/Foo#", "a/Bar.", "c/Baz#".
    let symbols = vec![
        SegmentSymbol {
            semantic_symbol: "b/Foo#".into(),
            symbol_id: 200,
            ref_group_ord: 0,
            rename_group_ord: 0,
            def_target_ord: 1,
        },
        SegmentSymbol {
            semantic_symbol: "a/Bar.".into(),
            symbol_id: 201,
            ref_group_ord: 1,
            rename_group_ord: -1,
            def_target_ord: 0,
        },
        SegmentSymbol {
            semantic_symbol: "c/Baz#".into(),
            symbol_id: 202,
            ref_group_ord: -1,
            rename_group_ord: -1,
            def_target_ord: 2,
        },
    ];
    let ref_occurrences = vec![
        vec![
            gocc(0, 5, 0, span(10, 4, 10, 7), ed()),
            gocc(0, 5, 0, span(2, 0, 2, 3), 0),
            gocc(1, 5, 1, span(5, 2, 5, 5), ed()),
            gocc(2, 6, 2, span(1, 0, 1, 2), 0), // stale: doc2 epoch is 7
        ],
        vec![gocc(2, 7, 2, span(3, 0, 3, 4), 0)],
    ];
    let def_occurrences = vec![
        vec![gocc(0, 5, 0, span(2, 0, 2, 3), def() | ed())],
        vec![], // empty def group
    ];
    let rename_occurrences = vec![vec![
        gocc(0, 5, 0, span(10, 4, 10, 7), ed()),
        gocc(1, 5, 1, span(5, 2, 5, 5), ed()),
        gocc(0, 5, 0, span(2, 0, 2, 3), 0), // non-editable
    ]];
    let rename_profiles = vec![RenameProfile {
        is_local: false,
        is_external: false,
        has_generated_occurrences: true,
        has_readonly_occurrences: false,
        has_override_family: true,
        has_companion: false,
        editable_occurrence_count: 2,
        unsafe_reason_mask: 0b101,
    }];
    let doc_occurrences = vec![
        vec![
            DocOcc {
                symbol_ord: 0, // caller "b/Foo#"
                span: span(10, 4, 10, 7),
                flags: ed(),
            },
            DocOcc {
                symbol_ord: 1, // caller "a/Bar."
                span: span(2, 0, 2, 3),
                flags: 0,
            },
        ],
        vec![DocOcc {
            symbol_ord: 0, // caller "b/Foo#"
            span: span(5, 2, 5, 5),
            flags: ed(),
        }],
        vec![], // empty doc
    ];
    let target_meta = vec![
        TargetMeta {
            bsp_id: "//a:lib".into(),
            scala_version: "3.3.1".into(),
            sourceroot: "/src/a".into(),
            semanticdb_root: "/meta/a".into(),
            content_hash: 0xAA,
            options_hash: 0xA1,
        },
        TargetMeta {
            bsp_id: "//b:lib".into(),
            scala_version: "3.3.1".into(),
            sourceroot: "/src/b".into(),
            semanticdb_root: "/meta/b".into(),
            content_hash: 0xBB,
            options_hash: 0xB1,
        },
        TargetMeta {
            bsp_id: "//c:lib".into(),
            scala_version: "2.13.12".into(),
            sourceroot: "/src/c".into(),
            semanticdb_root: "/meta/c".into(),
            content_hash: 0xCC,
            options_hash: 0xC1,
        },
    ];
    // symbol_meta parallel to `symbols` (caller order): Foo, Bar, Baz.
    let symbol_meta = vec![
        SymbolMeta {
            display: "Foo".into(),
            owner: "b".into(),
            package_name: "b".into(),
            kind: 3,
            properties: sym_props::CASE | sym_props::FINAL,
            def_packed_start: Span::pack(1, 0) as i32,
            def_packed_end: Span::pack(1, 3) as i32,
            def_doc_ord: 1,
        },
        SymbolMeta {
            display: "Bar".into(),
            owner: "a".into(),
            package_name: "a".into(),
            kind: 5,
            properties: 0,
            def_packed_start: Span::pack(2, 0) as i32,
            def_packed_end: Span::pack(2, 3) as i32,
            def_doc_ord: 0,
        },
        SymbolMeta {
            display: "Baz".into(),
            owner: "c".into(),
            package_name: "c".into(),
            kind: 3,
            properties: sym_props::ABSTRACT,
            def_packed_start: 0,
            def_packed_end: 0,
            def_doc_ord: -1,
        },
    ];
    let search_rows = vec![
        SearchRow {
            normalized_name: "foo".into(),
            symbol_ord: 1,
        },
        SearchRow {
            normalized_name: "bar".into(),
            symbol_ord: 0,
        },
    ];
    SegmentData {
        docs,
        targets: vec![1000, 1001, 1002],
        symbols,
        ref_occurrences,
        def_occurrences,
        rename_occurrences,
        rename_profiles,
        doc_occurrences,
        target_meta,
        symbol_meta,
        search_rows,
    }
}

fn write(dir: &Path, data: &SegmentData) -> PathBuf {
    ls_store::SegmentWriter::write(dir, SEGMENT_ID, data, CREATED_AT).expect("write segment")
}

fn collect_ref(
    r: &SegmentReader,
    group: u32,
    allowed: Option<&ls_index_model::TargetBitset>,
) -> Vec<GroupRecord> {
    let mut out = Vec::new();
    r.scan_ref_group(group, allowed, &mut |rec| out.push(rec));
    out
}

#[test]
fn round_trip_all_record_shapes() {
    let tmp = tempfile::tempdir().unwrap();
    let data = base_corpus();
    let dir = write(tmp.path(), &data);
    let r = SegmentReader::open(&dir).expect("open");

    // header
    assert_eq!(r.segment_id(), SEGMENT_ID);
    assert_eq!(r.created_at_ms(), CREATED_AT);
    assert_eq!(r.ref_group_count(), 2);
    assert_eq!(r.rename_group_count(), 1);
    assert_eq!(r.doc_count(), 3);
    assert_eq!(r.occurrence_count(), 12);

    // group index (ref/def/rename) + empty def group + block offsets
    assert_eq!(r.ref_group(0).count, 4);
    assert_eq!(r.ref_group(0).offset, 0);
    assert_eq!(r.ref_group(0).block_index_offset, 0);
    assert_eq!(r.ref_group(1).count, 1);
    assert_eq!(r.ref_group(1).offset, 4);
    assert_eq!(r.ref_group(1).block_index_offset, 1);
    assert_eq!(r.def_group(0).count, 1);
    assert_eq!(r.def_group(0).block_index_offset, 2);
    let empty_def = r.def_group(1);
    assert_eq!(empty_def.count, 0);
    assert_eq!(empty_def.block_index_offset, -1);
    assert_eq!(empty_def.offset, 1);
    assert_eq!(r.rename_group(0).count, 3);
    assert_eq!(r.rename_group(0).block_index_offset, 3);

    // rename profile
    let p = r.rename_profile(0);
    assert_eq!(p, data.rename_profiles[0]);

    // group postings: ref sorted by (doc_ord, packed_start, packed_end)
    assert_eq!(
        r.ref_record(0),
        GroupRecord {
            doc_ord: 0,
            doc_epoch: 5,
            target_ord: 0,
            packed_start: Span::pack(2, 0) as i32,
            packed_end: Span::pack(2, 3) as i32,
            flags: 0,
        }
    );
    assert_eq!(r.ref_record(1).packed_start, Span::pack(10, 4) as i32);
    assert_eq!(r.ref_record(2).doc_ord, 1);
    assert_eq!(r.ref_record(3).doc_ord, 2); // the stale one, still present raw
    assert_eq!(r.ref_record(3).doc_epoch, 6);
    assert_eq!(r.ref_record(4).target_ord, 2);
    assert_eq!(r.def_record(0).flags, def() | ed());

    // doc index
    assert_eq!(r.uri_of(0), "file:///A.scala");
    assert_eq!(r.uri_of(2), "file:///C.scala");
    assert_eq!(r.doc_id_of(1), 101);
    assert_eq!(r.epoch_of(2), 7);
    assert_eq!(r.target_ord_of_doc(1), 1);
    assert!(r.doc_generated(1));
    assert!(!r.doc_readonly(1));
    assert!(r.doc_readonly(2));
    let d2 = r.doc_entry(2);
    assert_eq!(d2.postings_count, 0);
    assert_eq!(d2.interval_first, -1);
    let d0 = r.doc_entry(0);
    assert_eq!(d0.postings_count, 2);
    assert_eq!(d0.interval_count, 1);

    // interval entries: doc0 block spans lines 2..10
    let iv = r.interval_entry(d0.interval_first);
    assert_eq!(iv.first_line, 2);
    assert_eq!(iv.last_line, 10);
    assert_eq!(iv.count, 2);

    // doc postings: symbol ords remapped to sorted order, sorted by span
    let rec0 = r.doc_record(d0.postings_offset);
    assert_eq!(rec0.symbol_ord, 0); // "a/Bar." sorts first
    assert_eq!(rec0.packed_start, Span::pack(2, 0) as i32);
    let rec1 = r.doc_record(d0.postings_offset + 1);
    assert_eq!(rec1.symbol_ord, 1); // "b/Foo#"
    assert_eq!(rec1.flags, ed());

    // symbol index (UTF-8 sorted) + binary search + target ids
    assert_eq!(r.symbol_count(), 3);
    assert_eq!(r.semantic_symbol_of(0), "a/Bar.");
    assert_eq!(r.semantic_symbol_of(1), "b/Foo#");
    assert_eq!(r.semantic_symbol_of(2), "c/Baz#");
    let sv = r.symbol_view(0);
    assert_eq!(sv.symbol_id, 201);
    assert_eq!(sv.ref_group_ord, 1);
    assert_eq!(sv.rename_group_ord, -1);
    assert_eq!(sv.def_target_ord, 0);
    assert_eq!(r.find_symbol_ord("b/Foo#"), Some(1));
    assert_eq!(r.find_symbol_ord("a/Bar."), Some(0));
    assert_eq!(r.find_symbol_ord("zzz"), None);
    assert_eq!(r.find_symbol_ord("a/Baq."), None);
    assert_eq!(r.target_count(), 3);
    assert_eq!(r.target_id_of(0), 1000);
    assert_eq!(r.target_id_of(2), 1002);

    // block index
    assert_eq!(r.block_count(), 4);
    assert_eq!(r.block_word_count(), 1);
    assert_eq!(r.block_size(), 256);
    let b0 = r.block_entry(0); // ref group 0
    assert_eq!(b0.first_record, 0);
    assert_eq!(b0.record_count, 4);
    assert_eq!(b0.editable_count, 2);
    assert_eq!(b0.ref_role_count, 4);
    assert_eq!(b0.def_role_count, 0);
    assert_eq!(b0.doc_ord_min, 0);
    assert_eq!(b0.doc_ord_max, 2);
    assert_eq!(b0.epoch_min, 5);
    assert_eq!(b0.epoch_max, 6);
    assert_eq!(b0.target_words, vec![0b111]);
    let b2 = r.block_entry(2); // def group 0
    assert_eq!(b2.def_role_count, 1);
    assert_eq!(b2.ref_role_count, 0);
    assert_eq!(b2.target_words, vec![0b1]);

    // target-meta
    assert_eq!(r.target_meta(0), data.target_meta[0]);
    assert_eq!(r.target_meta(2), data.target_meta[2]);

    // symbol-meta (remapped to sorted order: ord0=Bar, ord1=Foo, ord2=Baz)
    assert_eq!(r.symbol_meta(0), data.symbol_meta[1]);
    assert_eq!(r.symbol_meta(1), data.symbol_meta[0]);
    assert_eq!(r.symbol_meta(2), data.symbol_meta[2]);

    // search.bin: rows sorted by normalized_name, and each row's caller
    // symbol_ord remapped to the sorted on-disk ordinal (caller 0 "b/Foo#" -> 1,
    // caller 1 "a/Bar." -> 0) so it resolves the same symbol as symbol-meta.bin.
    assert_eq!(r.search_row_count(), 2);
    assert_eq!(r.search_row(0), ("bar".to_string(), 1)); // caller 0 -> sorted 1
    assert_eq!(r.search_row(1), ("foo".to_string(), 0)); // caller 1 -> sorted 0
}

#[test]
fn scans_apply_target_epoch_editable_filters() {
    let tmp = tempfile::tempdir().unwrap();
    let data = base_corpus();
    let dir = write(tmp.path(), &data);
    let r = SegmentReader::open(&dir).expect("open");

    // Unfiltered ref scan drops the stale-epoch occurrence (doc2 epoch6 != 7).
    let all = collect_ref(&r, 0, None);
    assert_eq!(all.len(), 3);
    assert!(all.iter().all(|rec| rec.doc_ord != 2));

    // Target filter: only target 1 survives (block not skipped, per-record cut).
    let only_t1 = ls_index_model::TargetBitset::of(3, [1]);
    let filtered = collect_ref(&r, 0, Some(&only_t1));
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].target_ord, 1);

    // Empty filter skips the whole block.
    let none = ls_index_model::TargetBitset::empty(3);
    assert_eq!(collect_ref(&r, 0, Some(&none)).len(), 0);

    // Def scan yields the one definition.
    let mut defs = Vec::new();
    r.scan_def_group(0, &mut |rec| defs.push(rec));
    assert_eq!(defs.len(), 1);

    // Rename scan drops the non-editable occurrence.
    let mut renames = Vec::new();
    r.scan_rename_group(0, &mut |rec| renames.push(rec));
    assert_eq!(renames.len(), 2);
    assert!(renames
        .iter()
        .all(|rec| occ_flags::has(rec.flags as u32, occ_flags::EDITABLE)));

    // Doc scans, with and without the editable filter.
    let mut d0: Vec<_> = Vec::new();
    r.scan_doc(0, false, &mut |rec| d0.push(rec));
    assert_eq!(d0.len(), 2);
    let mut d0_ed: Vec<_> = Vec::new();
    r.scan_doc(0, true, &mut |rec| d0_ed.push(rec));
    assert_eq!(d0_ed.len(), 1);
    let mut d2: Vec<_> = Vec::new();
    r.scan_doc(2, false, &mut |rec| d2.push(rec));
    assert_eq!(d2.len(), 0);
}

#[test]
fn multi_block_group_chunks_and_skips() {
    let tmp = tempfile::tempdir().unwrap();
    let mut occs = Vec::new();
    for line in 0..300u32 {
        occs.push(gocc(0, 5, 0, span(line, 0, line, 2), 0));
    }
    let data = SegmentData {
        docs: vec![SegmentDoc {
            uri: "file:///Big.scala".into(),
            doc_id: 1,
            epoch: 5,
            target_ord: 0,
            generated: false,
            readonly: false,
        }],
        targets: vec![7],
        symbols: vec![],
        ref_occurrences: vec![occs],
        def_occurrences: vec![vec![]],
        rename_occurrences: vec![],
        rename_profiles: vec![],
        doc_occurrences: vec![vec![]],
        target_meta: vec![TargetMeta::default()],
        symbol_meta: vec![],
        search_rows: vec![],
    };
    let dir = write(tmp.path(), &data);
    let r = SegmentReader::open(&dir).expect("open");

    assert_eq!(r.ref_group(0).count, 300);
    assert_eq!(r.block_count(), 2);
    assert_eq!(r.block_entry(0).record_count, 256);
    assert_eq!(r.block_entry(1).record_count, 44);

    assert_eq!(collect_ref(&r, 0, None).len(), 300);
    // A target filter with no members skips both blocks.
    let none = ls_index_model::TargetBitset::empty(1);
    assert_eq!(collect_ref(&r, 0, Some(&none)).len(), 0);
    // The matching target passes everything.
    let all = ls_index_model::TargetBitset::of(1, [0]);
    assert_eq!(collect_ref(&r, 0, Some(&all)).len(), 300);
}

#[test]
fn two_word_target_bitset() {
    let tmp = tempfile::tempdir().unwrap();
    let targets: Vec<i64> = (0..70).collect();
    let data = SegmentData {
        docs: vec![SegmentDoc {
            uri: "file:///Wide.scala".into(),
            doc_id: 1,
            epoch: 1,
            target_ord: 65,
            generated: false,
            readonly: false,
        }],
        targets,
        symbols: vec![],
        ref_occurrences: vec![vec![gocc(0, 1, 65, span(0, 0, 0, 1), 0)]],
        def_occurrences: vec![vec![]],
        rename_occurrences: vec![],
        rename_profiles: vec![],
        doc_occurrences: vec![vec![]],
        target_meta: (0..70).map(|_| TargetMeta::default()).collect(),
        symbol_meta: vec![],
        search_rows: vec![],
    };
    let dir = write(tmp.path(), &data);
    let r = SegmentReader::open(&dir).expect("open");

    assert_eq!(r.block_word_count(), 2);
    let b = r.block_entry(0);
    assert_eq!(b.target_words.len(), 2);
    assert_eq!(b.target_words[0], 0);
    assert_eq!(b.target_words[1], 1u64 << (65 - 64));

    let want = ls_index_model::TargetBitset::of(70, [65]);
    assert_eq!(collect_ref(&r, 0, Some(&want)).len(), 1);
    let other = ls_index_model::TargetBitset::of(70, [3]);
    assert_eq!(collect_ref(&r, 0, Some(&other)).len(), 0);
}

// ---- negative / corruption tests ----

fn flip_byte(path: &Path, offset: usize) {
    let mut bytes = std::fs::read(path).unwrap();
    bytes[offset] ^= 0xff;
    std::fs::write(path, bytes).unwrap();
}

fn open_err(dir: &Path) -> ls_store::SegmentError {
    SegmentReader::open(dir).expect_err("expected open to fail")
}

#[test]
fn corrupt_data_file_crc_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write(tmp.path(), &base_corpus());
    // Flip a byte in the middle of ref-postings.bin's column data.
    flip_byte(&dir.join("ref-postings.bin"), 20);
    match open_err(&dir) {
        ls_store::SegmentError::ChecksumMismatch { file } => assert_eq!(file, "ref-postings.bin"),
        other => panic!("expected ChecksumMismatch, got {other:?}"),
    }
}

#[test]
fn corrupt_header_body_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write(tmp.path(), &base_corpus());
    // Flip a byte inside the header's checksummed prefix (segment_id).
    flip_byte(&dir.join("header.bin"), 8);
    assert!(matches!(
        open_err(&dir),
        ls_store::SegmentError::HeaderChecksumMismatch
    ));
}

#[test]
fn bad_magic_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write(tmp.path(), &base_corpus());
    flip_byte(&dir.join("header.bin"), 0);
    assert!(matches!(
        open_err(&dir),
        ls_store::SegmentError::BadMagic { .. }
    ));
}

#[test]
fn bad_version_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write(tmp.path(), &base_corpus());
    // Overwrite version (offset 4) with 0xff — no longer 1.
    flip_byte(&dir.join("header.bin"), 4);
    assert!(matches!(
        open_err(&dir),
        ls_store::SegmentError::BadVersion { .. }
    ));
}

#[test]
fn truncated_file_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write(tmp.path(), &base_corpus());
    // Chop bytes off symbol-index.bin; its CRC will no longer match.
    let p = dir.join("symbol-index.bin");
    let mut bytes = std::fs::read(&p).unwrap();
    bytes.truncate(bytes.len() - 4);
    std::fs::write(&p, bytes).unwrap();
    assert!(matches!(
        open_err(&dir),
        ls_store::SegmentError::ChecksumMismatch { .. }
    ));
}

#[test]
fn checksums_list_tampered_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = write(tmp.path(), &base_corpus());
    // Corrupt the first file name byte in checksums.bin (past entry_count@0 and
    // the first name_len@8): flip a byte of "header.bin".
    flip_byte(&dir.join("checksums.bin"), 12);
    assert!(matches!(
        open_err(&dir),
        ls_store::SegmentError::ChecksumListMismatch { .. }
    ));
}
