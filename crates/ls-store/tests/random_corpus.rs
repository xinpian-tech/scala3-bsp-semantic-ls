//! Seeded-random corpus differential — the `RandomCorpusTest` port. A large
//! pseudo-random corpus (200 docs, 2000 symbols, ~20k occurrences) is written
//! through the real `SegmentWriter` and every reader obligation is checked
//! against an independent brute-force reference computed from the same corpus:
//! group scans (with and without target pruning), definition/rename scans,
//! epoch-stale dropping, per-doc scans, rename-profile round-trip, the symbol
//! and doc dictionaries, and 2000 `symbol_at` probes (uniform misses + targeted
//! boundary hits).
//!
//! The generator is a dependency-free `splitmix64` so the corpus is fully
//! deterministic and reproducible from the fixed seed without a `rand` crate.
//! Symbols are emitted in sorted UTF-8 order, so a caller ordinal equals its
//! on-disk ordinal and no remap is needed to predict `symbol_at` results.

use ls_index_model::{occ_flags, Role, Span, TargetBitset};
use ls_store::{
    DocOcc, GroupOcc, GroupRecord, RenameProfile, SegmentData, SegmentDoc, SegmentReader,
    SegmentSymbol, SegmentWriter,
};

const SEED: u64 = 0xc0ff_ee42;
const N_TARGETS: usize = 8;
const N_DOCS: usize = 200;
const N_SYMBOLS: usize = 2000;
const N_REF_GROUPS: usize = 400;
const N_RENAME_GROUPS: usize = 300;

// ---- dependency-free deterministic RNG (splitmix64) ----

struct Rng {
    state: u64,
}

impl Rng {
    fn new(seed: u64) -> Self {
        Rng { state: seed }
    }
    fn next_u64(&mut self) -> u64 {
        // splitmix64
        self.state = self.state.wrapping_add(0x9e37_79b9_7f4a_7c15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
        z ^ (z >> 31)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next_u64() % n as u64) as usize
    }
    fn chance(&mut self, num: u64, den: u64) -> bool {
        self.next_u64() % den < num
    }
    /// A valid, non-inverted span: line < 1000, char < 160, `pack(start) <= pack(end)`.
    fn span(&mut self) -> Span {
        let sl = (self.below(1000)) as u32;
        let sc = (self.below(150)) as u32;
        if self.chance(1, 3) {
            // multi-line: end line strictly greater keeps the packed order.
            let el = sl + 1 + self.below(3) as u32;
            let ec = self.below(150) as u32;
            Span::new(sl, sc, el, ec)
        } else {
            // same line: end char at or past start keeps the packed order.
            let ec = sc + 1 + self.below(8) as u32;
            Span::new(sl, sc, sl, ec)
        }
    }
}

// ---- corpus ----

struct Corpus {
    data: SegmentData,
    /// Per-doc current epoch, indexed by doc ord (mirror of `docs[d].epoch`).
    doc_epochs: Vec<i32>,
}

fn flags(editable: bool, definition: bool) -> i32 {
    let mut f = 0u32;
    if editable {
        f |= occ_flags::EDITABLE;
    }
    if definition {
        f |= occ_flags::DEFINITION;
    }
    f as i32
}

/// Build a group occurrence: a random doc, a target chosen independently of the
/// doc's own target (so pruning is exercised), and — one time in four — a stale
/// epoch that must be dropped from every scan.
fn group_occ(rng: &mut Rng, doc_epochs: &[i32], editable: bool, definition: bool) -> GroupOcc {
    let doc_ord = rng.below(N_DOCS);
    let fresh = doc_epochs[doc_ord];
    let doc_epoch = if rng.chance(1, 4) {
        fresh + 1 + rng.below(3) as i32 // stale
    } else {
        fresh
    };
    GroupOcc {
        doc_ord: doc_ord as i32,
        doc_epoch,
        target_ord: rng.below(N_TARGETS) as i32,
        span: rng.span(),
        flags: flags(editable, definition),
    }
}

fn random_corpus(seed: u64) -> Corpus {
    let mut rng = Rng::new(seed);

    // Docs, each with a current epoch in 1..=4.
    let mut docs = Vec::with_capacity(N_DOCS);
    let mut doc_epochs = Vec::with_capacity(N_DOCS);
    for d in 0..N_DOCS {
        let epoch = 1 + rng.below(4) as i32;
        doc_epochs.push(epoch);
        docs.push(SegmentDoc {
            uri: format!("file:///D{d:04}.scala"),
            doc_id: 1000 + d as i64,
            epoch,
            target_ord: rng.below(N_TARGETS) as i32,
            generated: rng.chance(1, 8),
            readonly: rng.chance(1, 8),
        });
    }

    // Symbols in sorted UTF-8 order (caller ordinal == on-disk ordinal).
    let mut symbols = Vec::with_capacity(N_SYMBOLS);
    for s in 0..N_SYMBOLS {
        symbols.push(SegmentSymbol {
            semantic_symbol: format!("ws/pkg/S{s:05}#"),
            symbol_id: 10_000 + s as i64,
            ref_group_ord: (s % N_REF_GROUPS) as i32,
            rename_group_ord: if s % 3 == 0 {
                -1
            } else {
                (s % N_RENAME_GROUPS) as i32
            },
            def_target_ord: if s % 5 == 0 {
                -1
            } else {
                (s % N_TARGETS) as i32
            },
        });
    }

    // Reference groups (~8000 occ) and the parallel definition groups (~2000).
    let mut ref_occurrences = Vec::with_capacity(N_REF_GROUPS);
    let mut def_occurrences = Vec::with_capacity(N_REF_GROUPS);
    for _g in 0..N_REF_GROUPS {
        let n_ref = 5 + rng.below(35);
        let refs = (0..n_ref)
            .map(|_| {
                let editable = rng.chance(1, 2);
                let definition = rng.chance(1, 4);
                group_occ(&mut rng, &doc_epochs, editable, definition)
            })
            .collect();
        ref_occurrences.push(refs);
        let n_def = rng.below(8);
        let defs = (0..n_def)
            .map(|_| {
                let editable = rng.chance(1, 3);
                group_occ(&mut rng, &doc_epochs, editable, true)
            })
            .collect();
        def_occurrences.push(defs);
    }

    // Rename groups (~4000 occ): most occurrences editable, the rest filtered.
    let mut rename_occurrences = Vec::with_capacity(N_RENAME_GROUPS);
    let mut rename_profiles = Vec::with_capacity(N_RENAME_GROUPS);
    for _g in 0..N_RENAME_GROUPS {
        let n = 3 + rng.below(24);
        let occ = (0..n)
            .map(|_| {
                let editable = rng.chance(7, 10);
                let definition = rng.chance(1, 5);
                group_occ(&mut rng, &doc_epochs, editable, definition)
            })
            .collect();
        rename_occurrences.push(occ);
        rename_profiles.push(RenameProfile {
            is_local: rng.chance(1, 2),
            is_external: rng.chance(1, 4),
            has_generated_occurrences: rng.chance(1, 3),
            has_readonly_occurrences: rng.chance(1, 3),
            has_override_family: rng.chance(1, 4),
            has_companion: rng.chance(1, 3),
            editable_occurrence_count: rng.below(20) as i32,
            unsafe_reason_mask: (rng.next_u64() & 0x3f) as i64,
        });
    }

    // Doc postings (~6000 occ): the symbol-at source.
    let mut doc_occurrences = Vec::with_capacity(N_DOCS);
    for _d in 0..N_DOCS {
        let n = rng.below(60);
        let occ = (0..n)
            .map(|_| {
                let symbol_ord = rng.below(N_SYMBOLS) as i32;
                let span = rng.span();
                let editable = rng.chance(1, 2);
                let definition = rng.chance(1, 4);
                DocOcc {
                    symbol_ord,
                    span,
                    flags: flags(editable, definition),
                }
            })
            .collect();
        doc_occurrences.push(occ);
    }

    let data = SegmentData {
        docs,
        targets: (0..N_TARGETS as i64).collect(),
        symbols,
        ref_occurrences,
        def_occurrences,
        rename_occurrences,
        rename_profiles,
        doc_occurrences,
        target_meta: vec![],
        symbol_meta: vec![],
        search_rows: vec![],
    };
    Corpus { data, doc_epochs }
}

// ---- brute-force reference ----

fn to_record(o: &GroupOcc) -> GroupRecord {
    GroupRecord {
        doc_ord: o.doc_ord,
        doc_epoch: o.doc_epoch,
        target_ord: o.target_ord,
        packed_start: Span::pack(o.span.start_line, o.span.start_char) as i32,
        packed_end: Span::pack(o.span.end_line, o.span.end_char) as i32,
        flags: o.flags,
    }
}

/// The records a scan must yield: the whole group stably sorted the way the
/// writer sorts it — `(doc_ord, pack(start), pack(end))` — then the survivors
/// of the target filter (when `allowed` is `Some`), the epoch filter (always),
/// and the editable filter (rename only).
fn expected_group_scan(
    group: &[GroupOcc],
    doc_epochs: &[i32],
    allowed: Option<&TargetBitset>,
    require_editable: bool,
) -> Vec<GroupRecord> {
    let mut sorted: Vec<&GroupOcc> = group.iter().collect();
    sorted.sort_by_key(|o| {
        (
            o.doc_ord,
            Span::pack(o.span.start_line, o.span.start_char),
            Span::pack(o.span.end_line, o.span.end_char),
        )
    });
    sorted
        .into_iter()
        .filter(|o| {
            if let Some(allowed) = allowed {
                if o.target_ord < 0 || !allowed.contains(o.target_ord as u32) {
                    return false;
                }
            }
            if o.doc_epoch != doc_epochs[o.doc_ord as usize] {
                return false;
            }
            if require_editable && !occ_flags::has(o.flags as u32, occ_flags::EDITABLE) {
                return false;
            }
            true
        })
        .map(to_record)
        .collect()
}

fn actual_group_scan(
    r: &SegmentReader,
    group_ord: u32,
    allowed: Option<&TargetBitset>,
) -> Vec<GroupRecord> {
    let mut out = Vec::new();
    r.scan_ref_group(group_ord, allowed, &mut |rec| out.push(rec));
    out
}

/// The smallest covering occurrence at `(line, character)` over one doc's
/// postings, resolving ties the way `symbol_at` does: smallest packed span,
/// then earliest `(packed_start, packed_end)` in sort order.
fn expected_symbol_at(doc_occs: &[DocOcc], line: u32, character: u32) -> Option<(i32, Span, i32)> {
    let mut sorted: Vec<&DocOcc> = doc_occs.iter().collect();
    sorted.sort_by_key(|o| {
        (
            Span::pack(o.span.start_line, o.span.start_char),
            Span::pack(o.span.end_line, o.span.end_char),
        )
    });
    let q = Span::pack(line, character);
    let mut best: Option<(&DocOcc, u32)> = None;
    for o in sorted {
        let ps = Span::pack(o.span.start_line, o.span.start_char);
        let pe = Span::pack(o.span.end_line, o.span.end_char);
        if ps <= q && q <= pe {
            let size = pe - ps;
            if best.is_none_or(|(_, bsize)| size < bsize) {
                best = Some((o, size));
            }
        }
    }
    best.map(|(o, _)| (o.symbol_ord, o.span, o.flags))
}

fn open(corpus: &Corpus) -> (tempfile::TempDir, SegmentReader) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentWriter::write(tmp.path(), 7, &corpus.data, 1_700_000_000_000).expect("write");
    let r = SegmentReader::open(&dir).expect("open");
    (tmp, r)
}

// ---- tests ----

#[test]
fn corpus_has_the_required_shape() {
    let corpus = random_corpus(SEED);
    let (_t, r) = open(&corpus);
    let d = &corpus.data;
    assert_eq!(d.docs.len(), N_DOCS);
    assert_eq!(d.symbols.len(), N_SYMBOLS);
    let total: u64 = (d.ref_occurrences.iter().map(Vec::len).sum::<usize>()
        + d.def_occurrences.iter().map(Vec::len).sum::<usize>()
        + d.rename_occurrences.iter().map(Vec::len).sum::<usize>()
        + d.doc_occurrences.iter().map(Vec::len).sum::<usize>()) as u64;
    assert_eq!(r.occurrence_count(), total, "occurrence accounting");
    assert!(total > 15_000, "corpus too small: {total}");
    assert_eq!(r.ref_group_count(), N_REF_GROUPS as u32);
    assert_eq!(r.rename_group_count(), N_RENAME_GROUPS as u32);
    assert_eq!(r.doc_count(), N_DOCS as u32);
    assert_eq!(r.symbol_count(), N_SYMBOLS);
    assert_eq!(r.target_count(), N_TARGETS);
}

#[test]
fn every_ref_group_scan_equals_brute_force_without_pruning() {
    let corpus = random_corpus(SEED);
    let (_t, r) = open(&corpus);
    let all = TargetBitset::all(N_TARGETS as u32);
    for (g, group) in corpus.data.ref_occurrences.iter().enumerate() {
        let expected = expected_group_scan(group, &corpus.doc_epochs, Some(&all), false);
        let actual = actual_group_scan(&r, g as u32, Some(&all));
        assert_eq!(actual, expected, "ref group {g}");
    }
}

#[test]
fn every_ref_group_scan_equals_brute_force_with_random_target_pruning() {
    let corpus = random_corpus(SEED);
    let (_t, r) = open(&corpus);
    let mut rng = Rng::new(SEED ^ 0x1111);
    for (g, group) in corpus.data.ref_occurrences.iter().enumerate() {
        let ords: Vec<u32> = (0..N_TARGETS as u32).filter(|_| rng.chance(1, 2)).collect();
        let allowed = TargetBitset::of(N_TARGETS as u32, ords.iter().copied());
        let expected = expected_group_scan(group, &corpus.doc_epochs, Some(&allowed), false);
        let actual = actual_group_scan(&r, g as u32, Some(&allowed));
        assert_eq!(actual, expected, "ref group {g} allowed={ords:?}");
    }
}

#[test]
fn every_definition_group_scan_equals_brute_force() {
    let corpus = random_corpus(SEED);
    let (_t, r) = open(&corpus);
    for (g, group) in corpus.data.def_occurrences.iter().enumerate() {
        let expected = expected_group_scan(group, &corpus.doc_epochs, None, false);
        let mut actual = Vec::new();
        r.scan_def_group(g as u32, &mut |rec| actual.push(rec));
        assert_eq!(actual, expected, "definition group {g}");
    }
}

#[test]
fn every_rename_group_scan_equals_brute_force_editable_and_fresh_only() {
    let corpus = random_corpus(SEED);
    let (_t, r) = open(&corpus);
    for (g, group) in corpus.data.rename_occurrences.iter().enumerate() {
        let expected = expected_group_scan(group, &corpus.doc_epochs, None, true);
        let mut actual = Vec::new();
        r.scan_rename_group(g as u32, &mut |rec| actual.push(rec));
        assert_eq!(actual, expected, "rename group {g}");
    }
}

#[test]
fn epoch_stale_occurrences_are_dropped_from_scans() {
    let corpus = random_corpus(SEED);
    let (_t, r) = open(&corpus);
    let all = TargetBitset::all(N_TARGETS as u32);
    let total: usize = corpus.data.ref_occurrences.iter().map(Vec::len).sum();
    let stale: usize = corpus
        .data
        .ref_occurrences
        .iter()
        .flatten()
        .filter(|o| o.doc_epoch != corpus.doc_epochs[o.doc_ord as usize])
        .count();
    assert!(
        stale > 100,
        "corpus should contain stale records, had {stale}"
    );
    let mut surfaced = 0;
    for g in 0..corpus.data.ref_occurrences.len() {
        r.scan_ref_group(g as u32, Some(&all), &mut |_| surfaced += 1);
    }
    assert_eq!(surfaced, total - stale);
}

#[test]
fn every_doc_scan_equals_brute_force() {
    let corpus = random_corpus(SEED);
    let (_t, r) = open(&corpus);
    for (d, occs) in corpus.data.doc_occurrences.iter().enumerate() {
        for require_editable in [false, true] {
            let mut sorted: Vec<&DocOcc> = occs.iter().collect();
            sorted.sort_by_key(|o| {
                (
                    Span::pack(o.span.start_line, o.span.start_char),
                    Span::pack(o.span.end_line, o.span.end_char),
                )
            });
            let expected: Vec<(i32, i32, i32, i32)> = sorted
                .into_iter()
                .filter(|o| {
                    !require_editable || occ_flags::has(o.flags as u32, occ_flags::EDITABLE)
                })
                .map(|o| {
                    (
                        o.symbol_ord,
                        Span::pack(o.span.start_line, o.span.start_char) as i32,
                        Span::pack(o.span.end_line, o.span.end_char) as i32,
                        o.flags,
                    )
                })
                .collect();
            let mut actual = Vec::new();
            r.scan_doc(d as u32, require_editable, &mut |rec| {
                actual.push((rec.symbol_ord, rec.packed_start, rec.packed_end, rec.flags))
            });
            assert_eq!(actual, expected, "doc {d} editable={require_editable}");
        }
    }
}

#[test]
fn all_rename_profiles_round_trip() {
    let corpus = random_corpus(SEED);
    let (_t, r) = open(&corpus);
    for (g, profile) in corpus.data.rename_profiles.iter().enumerate() {
        assert_eq!(&r.rename_profile(g as u32), profile, "rename profile {g}");
    }
}

#[test]
fn symbol_dictionary_is_complete_and_consistent() {
    let corpus = random_corpus(SEED);
    let (_t, r) = open(&corpus);
    for (c, sym) in corpus.data.symbols.iter().enumerate() {
        // Symbols were emitted in sorted order, so caller ordinal == on-disk ordinal.
        let ord = r
            .find_symbol_ord(&sym.semantic_symbol)
            .unwrap_or_else(|| panic!("missing {}", sym.semantic_symbol));
        assert_eq!(ord as usize, c, "{}", sym.semantic_symbol);
        assert_eq!(r.semantic_symbol_of(ord), sym.semantic_symbol);
        let view = r.symbol_view(ord);
        assert_eq!(view.symbol_id, sym.symbol_id);
        assert_eq!(view.ref_group_ord, sym.ref_group_ord);
        assert_eq!(view.rename_group_ord, sym.rename_group_ord);
        assert_eq!(view.def_target_ord, sym.def_target_ord);
    }
    assert_eq!(r.find_symbol_ord("ws/pkg/DoesNotExist#"), None);
}

#[test]
fn doc_dictionary_is_complete() {
    let corpus = random_corpus(SEED);
    let (_t, r) = open(&corpus);
    for (d, doc) in corpus.data.docs.iter().enumerate() {
        let ord = d as u32;
        assert_eq!(r.uri_of(ord), doc.uri);
        assert_eq!(r.doc_id_of(ord), doc.doc_id);
        assert_eq!(r.epoch_of(ord), doc.epoch);
        assert_eq!(r.target_ord_of_doc(ord), doc.target_ord);
        assert_eq!(r.doc_generated(ord), doc.generated);
        assert_eq!(r.doc_readonly(ord), doc.readonly);
    }
}

#[test]
fn random_symbol_at_probes_match_brute_force() {
    let corpus = random_corpus(SEED);
    let (_t, r) = open(&corpus);
    let mut rng = Rng::new(SEED ^ 0x2222);

    let probe = |r: &SegmentReader, d: usize, line: u32, ch: u32, hits: &mut usize| {
        let actual = r.symbol_at(d as u32, line, ch);
        let expected = expected_symbol_at(&corpus.data.doc_occurrences[d], line, ch);
        assert_eq!(actual.is_some(), expected.is_some(), "doc {d} @{line}:{ch}");
        if let (Some(a), Some((sym, span, flags))) = (&actual, expected) {
            *hits += 1;
            assert_eq!(a.symbol_ord, sym, "doc {d} @{line}:{ch}");
            assert_eq!(a.span, span, "doc {d} @{line}:{ch}");
            assert_eq!(a.flags, flags, "doc {d} @{line}:{ch}");
            let want_role = if occ_flags::has(flags as u32, occ_flags::DEFINITION) {
                Role::Definition
            } else {
                Role::Reference
            };
            assert_eq!(a.role, want_role, "doc {d} @{line}:{ch}");
        }
    };

    let mut hits = 0;
    // Uniform probes: mostly misses, must still agree with brute force.
    for _ in 0..1000 {
        let d = rng.below(N_DOCS);
        probe(
            &r,
            d,
            rng.below(1010) as u32,
            rng.below(170) as u32,
            &mut hits,
        );
    }
    // Targeted probes at occurrence boundaries: mostly hits.
    for _ in 0..1000 {
        let d = (0..)
            .map(|_| rng.below(N_DOCS))
            .find(|&d| !corpus.data.doc_occurrences[d].is_empty())
            .unwrap();
        let occs = &corpus.data.doc_occurrences[d];
        let o = &occs[rng.below(occs.len())];
        let s = o.span;
        match rng.below(5) {
            0 => probe(&r, d, s.start_line, s.start_char, &mut hits),
            1 => probe(&r, d, s.end_line, s.end_char, &mut hits),
            2 => probe(&r, d, s.start_line, s.start_char + 1, &mut hits),
            3 => probe(&r, d, s.end_line, s.end_char + 1, &mut hits),
            _ => probe(
                &r,
                d,
                s.start_line,
                s.start_char.saturating_sub(1),
                &mut hits,
            ),
        }
    }
    assert!(hits > 300, "probe set too weak: only {hits} hits");
}
