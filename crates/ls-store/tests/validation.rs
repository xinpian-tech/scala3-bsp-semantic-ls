//! Deep open-time validation: a *self-consistent* corrupt segment (bytes
//! mutated **and** `checksums.bin` recomputed) is still rejected with a typed
//! `SegmentError`, and never panics. Plus writer input validation.

use std::path::{Path, PathBuf};

use ls_index_model::Span;
use ls_store::{
    DocOcc, GroupOcc, SegmentData, SegmentDoc, SegmentError, SegmentReader, SegmentSymbol,
    SegmentWriter, TargetMeta,
};

fn gocc(doc_ord: i32, target: i32, s: Span, flags: i32) -> GroupOcc {
    GroupOcc {
        doc_ord,
        doc_epoch: 5,
        target_ord: target,
        span: s,
        flags,
    }
}

/// A small but complete valid segment: 3 docs, 3 targets, 3 symbols (one ref
/// group with one occurrence → one block, doc0 with one doc occurrence).
fn base_data() -> SegmentData {
    let docs = (0..3)
        .map(|i| SegmentDoc {
            uri: format!("file:///D{i}.scala"),
            doc_id: 100 + i as i64,
            epoch: 5,
            target_ord: i,
            generated: false,
            readonly: false,
        })
        .collect();
    let symbols = vec![
        SegmentSymbol {
            semantic_symbol: "b/Foo#".into(),
            symbol_id: 200,
            ref_group_ord: 0,
            rename_group_ord: -1,
            def_target_ord: 1,
        },
        SegmentSymbol {
            semantic_symbol: "a/Bar.".into(),
            symbol_id: 201,
            ref_group_ord: -1,
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
    SegmentData {
        docs,
        targets: vec![1000, 1001, 1002],
        symbols,
        ref_occurrences: vec![vec![gocc(0, 0, Span::new(1, 0, 1, 3), 0)]],
        def_occurrences: vec![vec![]],
        rename_occurrences: vec![],
        rename_profiles: vec![],
        doc_occurrences: vec![
            vec![DocOcc {
                symbol_ord: 0,
                span: Span::new(1, 0, 1, 3),
                flags: 0,
            }],
            vec![],
            vec![],
        ],
        target_meta: (0..3).map(|_| TargetMeta::default()).collect(),
        symbol_meta: vec![],
        search_rows: vec![],
    }
}

fn write_base() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentWriter::write(tmp.path(), 1, &base_data(), 0).expect("write base");
    // Sanity: the untouched segment opens.
    SegmentReader::open(&dir).expect("base opens");
    (tmp, dir)
}

/// Overwrite `filename`'s CRC entry in `checksums.bin`.
fn patch_checksum(dir: &Path, filename: &str, new_crc: u32) {
    let cpath = dir.join("checksums.bin");
    let mut c = std::fs::read(&cpath).unwrap();
    let count = i64::from_le_bytes(c[0..8].try_into().unwrap());
    let mut off = 8usize;
    let mut crc_off = None;
    for _ in 0..count {
        let name_len = i32::from_le_bytes(c[off..off + 4].try_into().unwrap()) as usize;
        off += 4;
        let matches = &c[off..off + name_len] == filename.as_bytes();
        off += name_len;
        if matches {
            crc_off = Some(off);
        }
        off += 8;
    }
    let crc_off = crc_off.expect("file listed in checksums.bin");
    c[crc_off..crc_off + 8].copy_from_slice(&(new_crc as u64).to_le_bytes());
    std::fs::write(&cpath, &c).unwrap();
}

/// Mutate a data file, recompute its CRC in `checksums.bin`, and return the
/// (typed) error `open` raises. Panicking here fails the test — that is the
/// no-panic guarantee under test.
fn recorrupt(dir: &Path, filename: &str, mutate: impl FnOnce(&mut Vec<u8>)) -> SegmentError {
    let path = dir.join(filename);
    let mut bytes = std::fs::read(&path).unwrap();
    mutate(&mut bytes);
    let crc = crc32c::crc32c(&bytes);
    std::fs::write(&path, &bytes).unwrap();
    patch_checksum(dir, filename, crc);
    SegmentReader::open(dir)
        .err()
        .expect("expected self-consistent corruption to be rejected")
}

fn assert_structural(e: SegmentError) {
    assert!(matches!(e, SegmentError::Structural { .. }), "got {e:?}");
}

#[test]
fn oob_group_doc_ord_rejected() {
    let (_t, dir) = write_base();
    // ref-postings.bin: record0 doc_ord (col0 @ offset 8) → 3 (doc_count == 3).
    assert_structural(recorrupt(&dir, "ref-postings.bin", |b| {
        b[8..12].copy_from_slice(&3i32.to_le_bytes())
    }));
}

#[test]
fn oob_symbol_str_offset_rejected() {
    let (_t, dir) = write_base();
    // symbol-index.bin: symbol0 str_offset (@ offset 24) → far past the blob.
    assert_structural(recorrupt(&dir, "symbol-index.bin", |b| {
        b[24..28].copy_from_slice(&0x7fff_ffffi32.to_le_bytes())
    }));
}

#[test]
fn oob_doc_target_ord_rejected() {
    let (_t, dir) = write_base();
    // doc-index.bin: doc0 target_ord (entry @ 24, field @ +20 = 44) → 3.
    assert_structural(recorrupt(&dir, "doc-index.bin", |b| {
        b[44..48].copy_from_slice(&3i32.to_le_bytes())
    }));
}

#[test]
fn unsorted_symbols_rejected() {
    let (_t, dir) = write_base();
    // symbol-index.bin blob starts at 24 + 3*32 + 3*8 = 144; the first sorted
    // symbol is "a/Bar." — bump 'a' to 'z' so it no longer sorts first.
    assert_structural(recorrupt(&dir, "symbol-index.bin", |b| b[144] = b'z'));
}

#[test]
fn broken_block_first_record_rejected() {
    let (_t, dir) = write_base();
    // block-index.bin: block0 first_record (@ offset 16) → 5 (must be 0).
    assert_structural(recorrupt(&dir, "block-index.bin", |b| {
        b[16..24].copy_from_slice(&5i64.to_le_bytes())
    }));
}

#[test]
fn negative_record_count_rejected() {
    let (_t, dir) = write_base();
    // ref-postings.bin: record_count (@ offset 0) → -1.
    assert_structural(recorrupt(&dir, "ref-postings.bin", |b| {
        b[0..8].copy_from_slice(&(-1i64).to_le_bytes())
    }));
}

#[test]
fn zeroed_block_target_words_rejected() {
    let (_t, dir) = write_base();
    // block-index.bin: block0 target_words lane (base 16, field @ +40 = 56) → 0.
    // A filtered scan would otherwise skip the real record.
    assert_structural(recorrupt(&dir, "block-index.bin", |b| {
        b[56..64].copy_from_slice(&0u64.to_le_bytes())
    }));
}

#[test]
fn corrupted_block_editable_count_rejected() {
    let (_t, dir) = write_base();
    // block-index.bin: block0 editable_count (base 16, field @ +12 = 28) → 5
    // (the real record is not editable, so recompute expects 0).
    assert_structural(recorrupt(&dir, "block-index.bin", |b| {
        b[28..32].copy_from_slice(&5i32.to_le_bytes())
    }));
}

#[test]
fn corrupted_interval_first_line_rejected() {
    let (_t, dir) = write_base();
    // doc-index.bin: interval0 first_line (interval_base 24+3*48=168, @ +0).
    assert_structural(recorrupt(&dir, "doc-index.bin", |b| {
        b[168..172].copy_from_slice(&99i32.to_le_bytes())
    }));
}

#[test]
fn corrupted_interval_last_line_rejected() {
    let (_t, dir) = write_base();
    // doc-index.bin: interval0 last_line (@ 168 + 4 = 172) → 0 (real is 1); a
    // too-low last_line would make symbol_at skip the block.
    assert_structural(recorrupt(&dir, "doc-index.bin", |b| {
        b[172..176].copy_from_slice(&0i32.to_le_bytes())
    }));
}

#[test]
fn checksums_negative_name_len_rejected_without_panic() {
    let (_t, dir) = write_base();
    // checksums.bin is not itself checksummed: mutate its first entry's name_len
    // (@ offset 8) to -1. `verify_checksums` must reject, not overflow/panic.
    let cpath = dir.join("checksums.bin");
    let mut c = std::fs::read(&cpath).unwrap();
    c[8..12].copy_from_slice(&(-1i32).to_le_bytes());
    std::fs::write(&cpath, &c).unwrap();
    match SegmentReader::open(&dir) {
        Err(SegmentError::ChecksumListMismatch { .. }) => {}
        Err(other) => panic!("expected ChecksumListMismatch, got {other:?}"),
        Ok(_) => panic!("expected rejection, but the segment opened"),
    }
}

// ---- writer input validation ----

fn expect_invalid(data: &SegmentData) {
    let tmp = tempfile::tempdir().unwrap();
    match SegmentWriter::write(tmp.path(), 1, data, 0) {
        Err(SegmentError::InvalidInput { .. }) => {}
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

#[test]
fn writer_rejects_oob_group_doc_ord() {
    let mut d = base_data();
    d.ref_occurrences[0][0].doc_ord = 9;
    expect_invalid(&d);
}

#[test]
fn writer_rejects_oob_group_target_ord() {
    let mut d = base_data();
    d.ref_occurrences[0][0].target_ord = 9;
    expect_invalid(&d);
}

#[test]
fn writer_rejects_oob_doc_symbol_ord() {
    let mut d = base_data();
    d.doc_occurrences[0][0].symbol_ord = 9;
    expect_invalid(&d);
}

#[test]
fn writer_rejects_oob_symbol_ref_group_ord() {
    let mut d = base_data();
    d.symbols[0].ref_group_ord = 5; // only 1 ref group
    expect_invalid(&d);
}

#[test]
fn writer_rejects_inverted_span() {
    let mut d = base_data();
    d.ref_occurrences[0][0].span = Span::new(5, 0, 1, 0); // start after end
    expect_invalid(&d);
}

#[test]
fn writer_rejects_char_overflow() {
    let mut d = base_data();
    d.ref_occurrences[0][0].span = Span::new(1, 5000, 1, 6000); // char >= 2^12
    expect_invalid(&d);
}
