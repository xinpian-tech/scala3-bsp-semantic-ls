//! Property tests for the model invariants that unit tests only spot-check:
//! the position packing (round-trip, saturation, order preservation) and the
//! target bitset (membership, cardinality, word round-trip, intersection).

use std::collections::BTreeSet;

use ls_index_model::{Span, TargetBitset};
use proptest::prelude::*;

proptest! {
    /// In-range coordinates survive pack -> unpack unchanged.
    #[test]
    fn pack_round_trips_in_range(
        line in 0u32..(1 << 20),
        character in 0u32..(1 << 12),
    ) {
        let p = Span::pack(line, character);
        prop_assert_eq!(Span::unpack_line(p), line);
        prop_assert_eq!(Span::unpack_char(p), character);
    }

    /// For *any* input, packing is exactly min-saturation on each component and
    /// never bleeds one component into the other.
    #[test]
    fn pack_saturates_and_is_lossless_after_clamp(line in any::<u32>(), character in any::<u32>()) {
        let p = Span::pack(line, character);
        prop_assert_eq!(Span::unpack_line(p), line.min(Span::LINE_MAX));
        prop_assert_eq!(Span::unpack_char(p), character.min(Span::CHAR_MASK));
    }

    /// Within the representable range, the packed `u32` orders identically to
    /// lexicographic `(line, character)` order.
    #[test]
    fn pack_preserves_lexicographic_order(
        l1 in 0u32..(1 << 20),
        c1 in 0u32..(1 << 12),
        l2 in 0u32..(1 << 20),
        c2 in 0u32..(1 << 12),
    ) {
        let lexical = (l1, c1) <= (l2, c2);
        let packed = Span::pack(l1, c1) <= Span::pack(l2, c2);
        prop_assert_eq!(lexical, packed);
    }

    /// A bitset contains exactly the distinct ordinals inserted, reports the
    /// right cardinality, and survives a word-array round-trip.
    #[test]
    fn bitset_contains_exactly_inserted(
        size in 1u32..512,
        raw in prop::collection::vec(0u32..512, 0..64),
    ) {
        let inserted: Vec<u32> = raw.into_iter().filter(|&o| o < size).collect();
        let bs = TargetBitset::of(size, inserted.iter().copied());
        let distinct: BTreeSet<u32> = inserted.iter().copied().collect();

        for o in 0..size {
            prop_assert_eq!(bs.contains(o), distinct.contains(&o));
        }
        prop_assert_eq!(bs.cardinality() as usize, distinct.len());
        prop_assert!(!bs.contains(size));

        let round_tripped = TargetBitset::from_words(size, bs.to_words());
        prop_assert_eq!(bs, round_tripped);
    }

    /// Two bitsets intersect iff their inserted sets share an ordinal.
    #[test]
    fn bitset_intersects_iff_sets_overlap(
        size in 1u32..256,
        a_raw in prop::collection::vec(0u32..256, 0..32),
        b_raw in prop::collection::vec(0u32..256, 0..32),
    ) {
        let a: Vec<u32> = a_raw.into_iter().filter(|&o| o < size).collect();
        let b: Vec<u32> = b_raw.into_iter().filter(|&o| o < size).collect();
        let a_set: BTreeSet<u32> = a.iter().copied().collect();
        let b_set: BTreeSet<u32> = b.iter().copied().collect();

        let bsa = TargetBitset::of(size, a.iter().copied());
        let bsb = TargetBitset::of(size, b.iter().copied());

        let overlap = a_set.intersection(&b_set).next().is_some();
        prop_assert_eq!(bsa.intersects(&bsb), overlap);
        prop_assert_eq!(bsa.intersects_words(&bsb.to_words()), overlap);
    }

    /// `from_words` normalizes arbitrary raw input: membership stays within
    /// `[0, size)`, cardinality agrees with iteration, and `contains` never
    /// panics for any ordinal (the release-safety property).
    #[test]
    fn from_words_normalizes_arbitrary_input(
        size in 0u32..512,
        raw in prop::collection::vec(any::<u64>(), 0..12),
    ) {
        let bs = TargetBitset::from_words(size, raw);
        prop_assert_eq!(bs.cardinality() as usize, bs.iter().count());
        prop_assert!(bs.iter().all(|o| o < size));
        prop_assert!(bs.iter().all(|o| bs.contains(o)));
        for o in [0u32, size.saturating_sub(1), size, size.saturating_add(1), u32::MAX] {
            let _ = bs.contains(o); // must not panic
        }
    }
}
