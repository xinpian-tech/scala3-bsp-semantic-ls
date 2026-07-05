//! `symbol_at` / `symbol_at_counting` parity tests — the exact
//! smallest-covering-span rule over the doc interval-block index, mirroring the
//! Scala `HandBuiltCorpusTest` symbolAt cases plus a bruteforce probe (vs a
//! naive scan) and an interval-pruning assertion.

use ls_index_model::{occ_flags, Role, Span};
use ls_store::{DocOcc, SegmentData, SegmentDoc, SegmentReader, SegmentSymbol, TargetMeta};

fn def() -> i32 {
    occ_flags::DEFINITION as i32
}

fn dococc(sym: i32, sl: u32, sc: u32, el: u32, ec: u32, flags: i32) -> DocOcc {
    DocOcc {
        symbol_ord: sym,
        span: Span::new(sl, sc, el, ec),
        flags,
    }
}

/// A segment with `docs` documents (each a Vec of doc occurrences) and
/// `n_symbols` pre-sorted symbols (so caller ordinal == on-disk ordinal, no
/// remap: `symbol_at` returns the ordinal placed in `DocOcc`).
fn segment(docs: Vec<Vec<DocOcc>>, n_symbols: usize) -> (tempfile::TempDir, SegmentReader) {
    let symbols = (0..n_symbols)
        .map(|i| SegmentSymbol {
            semantic_symbol: format!("s{i:04}"),
            symbol_id: i as i64,
            ref_group_ord: -1,
            rename_group_ord: -1,
            def_target_ord: -1,
        })
        .collect();
    let seg_docs = (0..docs.len())
        .map(|i| SegmentDoc {
            uri: format!("file:///D{i}.scala"),
            doc_id: i as i64,
            epoch: 1,
            target_ord: 0,
            generated: false,
            readonly: false,
        })
        .collect();
    let data = SegmentData {
        docs: seg_docs,
        targets: vec![1],
        symbols,
        ref_occurrences: vec![],
        def_occurrences: vec![],
        rename_occurrences: vec![],
        rename_profiles: vec![],
        doc_occurrences: docs,
        target_meta: vec![TargetMeta::default()],
        symbol_meta: vec![],
        search_rows: vec![],
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = ls_store::SegmentWriter::write(tmp.path(), 1, &data, 0).expect("write");
    let r = SegmentReader::open(&dir).expect("open");
    (tmp, r)
}

#[test]
fn boundaries_role_and_gaps() {
    let (_t, r) = segment(
        vec![vec![dococc(0, 1, 4, 1, 7, 0), dococc(1, 0, 4, 0, 7, def())]],
        2,
    );
    // Start/end-inclusive containment.
    let hit = r.symbol_at(0, 1, 4).unwrap();
    assert_eq!(hit.symbol_ord, 0);
    assert_eq!(hit.role, Role::Reference);
    assert_eq!(hit.span, Span::new(1, 4, 1, 7));
    assert_eq!(
        r.symbol_at(0, 1, 7).map(|h| h.span),
        Some(Span::new(1, 4, 1, 7))
    );
    assert_eq!(r.symbol_at(0, 1, 3), None);
    assert_eq!(r.symbol_at(0, 1, 8), None);
    // Definition role from flags.
    assert_eq!(r.symbol_at(0, 0, 5).unwrap().role, Role::Definition);
    // Between occurrences / past the end → None.
    assert_eq!(r.symbol_at(0, 2, 5), None);
    assert_eq!(r.symbol_at(0, 100, 0), None);
}

#[test]
fn smallest_covering_span_wins() {
    let (_t, r) = segment(
        vec![vec![dococc(0, 3, 0, 3, 10, 0), dococc(1, 3, 5, 3, 8, 0)]],
        2,
    );
    assert_eq!(r.symbol_at(0, 3, 7).unwrap().symbol_ord, 1); // inner
    assert_eq!(r.symbol_at(0, 3, 2).unwrap().symbol_ord, 0); // outer
}

#[test]
fn equal_size_tie_goes_to_earliest_start() {
    let (_t, r) = segment(
        vec![vec![dococc(0, 4, 0, 4, 10, 0), dococc(1, 4, 5, 4, 15, 0)]],
        2,
    );
    // Both spans are width 10 and cover (4,7); earliest packed_start wins.
    assert_eq!(r.symbol_at(0, 4, 7).unwrap().symbol_ord, 0);
}

#[test]
fn doc_without_postings_returns_none() {
    let (_t, r) = segment(vec![vec![dococc(0, 0, 0, 0, 3, 0)], vec![]], 1);
    assert_eq!(r.symbol_at(1, 0, 0), None);
    let (hit, blocks) = r.symbol_at_counting(1, 0, 0);
    assert!(hit.is_none());
    assert_eq!(blocks, 0);
}

#[test]
fn interval_block_pruning() {
    // 600 single-line occurrences → 3 interval blocks; a mid query scans one.
    let occs: Vec<DocOcc> = (0..600u32).map(|i| dococc(0, i, 0, i, 5, 0)).collect();
    let (_t, r) = segment(vec![occs], 1);
    assert_eq!(r.doc_entry(0).interval_count, 3);
    let (hit, blocks) = r.symbol_at_counting(0, 500, 2);
    assert_eq!(hit.unwrap().span, Span::new(500, 0, 500, 5));
    assert!(blocks <= 1, "expected pruning, scanned {blocks} blocks");
}

/// Naive reference implementation of the symbol-at rule over the raw records
/// (sorted the way the writer sorts doc postings).
fn naive(raw: &[(i32, u32, u32, u32, u32)], line: u32, ch: u32) -> Option<i32> {
    let mut sorted: Vec<_> = raw.to_vec();
    sorted.sort_by_key(|&(_, sl, sc, el, ec)| (Span::pack(sl, sc), Span::pack(el, ec)));
    let q = Span::pack(line, ch);
    let mut best: Option<(i32, u32)> = None; // (sym, size)
    for (s, sl, sc, el, ec) in sorted {
        let ps = Span::pack(sl, sc);
        let pe = Span::pack(el, ec);
        if ps <= q && q <= pe {
            let size = pe - ps;
            if best.is_none_or(|(_, bsize)| size < bsize) {
                best = Some((s, size));
            }
        }
    }
    best.map(|(s, _)| s)
}

#[test]
fn high_line_positions_use_unsigned_packed_order() {
    // Lines >= 524288 make `line << 12` exceed 2^31, so the packed position has
    // its sign bit set. A wide low-start occurrence (packed_start 0) precedes a
    // narrow high-line one on disk; signed comparison would break-early at the
    // first record and miss the real smallest hit.
    let (_t, r) = segment(
        vec![vec![
            dococc(0, 0, 0, 600_000, 5, 0),        // spans down to a high line
            dococc(1, 524_288, 0, 524_288, 10, 0), // narrow, on line 524288
        ]],
        2,
    );
    let hit = r.symbol_at(0, 524_288, 5).expect("covering hit");
    assert_eq!(hit.symbol_ord, 1); // the narrow high-line occurrence wins
    assert_eq!(hit.span, Span::new(524_288, 0, 524_288, 10));
    // The wide occurrence still wins where the narrow one does not cover.
    assert_eq!(r.symbol_at(0, 524_288, 20).unwrap().symbol_ord, 0);
    assert_eq!(r.symbol_at(0, 300_000, 0).unwrap().symbol_ord, 0);
}

#[test]
fn bruteforce_matches_naive_scan() {
    // Overlapping, nested, single-point, and multi-line occurrences.
    let raw: Vec<(i32, u32, u32, u32, u32)> = vec![
        (0, 0, 0, 0, 10),
        (1, 0, 2, 0, 6),
        (2, 0, 4, 0, 4), // single point
        (3, 0, 3, 2, 3), // multi-line
        (4, 1, 0, 1, 20),
        (5, 1, 5, 1, 5),
        (6, 2, 0, 2, 8),
        (7, 2, 2, 2, 6),
        (8, 0, 7, 0, 9),
        (9, 1, 10, 1, 15),
    ];
    let occs = raw
        .iter()
        .map(|&(s, sl, sc, el, ec)| dococc(s, sl, sc, el, ec, if s % 3 == 0 { def() } else { 0 }))
        .collect();
    let (_t, r) = segment(vec![occs], 10);
    for line in 0..3u32 {
        for ch in 0..24u32 {
            let expected = naive(&raw, line, ch);
            let got = r.symbol_at(0, line, ch).map(|h| h.symbol_ord);
            assert_eq!(got, expected, "mismatch at ({line},{ch})");
        }
    }
}
