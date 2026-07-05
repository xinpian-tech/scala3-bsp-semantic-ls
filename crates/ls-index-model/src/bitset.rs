//! Exact target-membership sets over snapshot target ordinals.

/// A dense bitset over target ordinals (an exact membership set, not a
/// probabilistic filter). Used for target-graph pruning and block skip.
///
/// Words are little-endian `u64` lanes; bit `o` lives at
/// `words[o >> 6] & (1 << (o & 63))`.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TargetBitset {
    words: Vec<u64>,
    size: u32,
}

impl TargetBitset {
    /// An all-zero bitset sized for `size` ordinals.
    pub fn empty(size: u32) -> Self {
        let words = vec![0u64; word_count(size)];
        TargetBitset { words, size }
    }

    /// A bitset with the given ordinals set. Panics if any ordinal is `>= size`.
    pub fn of(size: u32, ords: impl IntoIterator<Item = u32>) -> Self {
        // Match the Scala `of`, which keeps at least one word.
        let words_len = word_count(size).max(1);
        let mut words = vec![0u64; words_len];
        for o in ords {
            assert!(o < size, "target ordinal {o} out of range [0,{size})");
            words[(o >> 6) as usize] |= 1u64 << (o & 63);
        }
        TargetBitset { words, size }
    }

    /// Wrap a raw word array as a bitset of `size` ordinals.
    pub fn from_words(size: u32, words: Vec<u64>) -> Self {
        TargetBitset { words, size }
    }

    /// A bitset with every ordinal in `[0, size)` set.
    pub fn all(size: u32) -> Self {
        TargetBitset::of(size, 0..size)
    }

    /// The number of ordinals this bitset is sized for.
    #[inline]
    pub fn size(&self) -> u32 {
        self.size
    }

    /// Is `target_ord` a member? Ordinals `>= size` are never members.
    #[inline]
    pub fn contains(&self, target_ord: u32) -> bool {
        target_ord < self.size
            && (self.words[(target_ord >> 6) as usize] & (1u64 << (target_ord & 63))) != 0
    }

    /// Do these two bitsets share any set bit?
    pub fn intersects(&self, other: &TargetBitset) -> bool {
        self.intersects_words(&other.words)
    }

    /// Intersect against a raw word array (e.g. a block's target bitset read
    /// straight out of an mmap segment).
    pub fn intersects_words(&self, other_words: &[u64]) -> bool {
        let n = self.words.len().min(other_words.len());
        self.words[..n]
            .iter()
            .zip(&other_words[..n])
            .any(|(a, b)| a & b != 0)
    }

    /// A copy of the backing word array.
    pub fn to_words(&self) -> Vec<u64> {
        self.words.clone()
    }

    /// The number of set bits.
    pub fn cardinality(&self) -> u32 {
        self.words.iter().map(|w| w.count_ones()).sum()
    }
}

/// Words needed to hold `size` bits (`ceil(size / 64)`).
#[inline]
fn word_count(size: u32) -> usize {
    ((size as usize) + 63) >> 6
}

#[cfg(test)]
mod target_graph_suite {
    // Mirrors the `TargetBitset` cases from `TargetGraphSuite`.
    use super::*;

    #[test]
    fn membership_and_bounds() {
        let bs = TargetBitset::of(130, [0, 64, 129]);
        assert!(bs.contains(0));
        assert!(bs.contains(64));
        assert!(bs.contains(129));
        assert!(!bs.contains(1));
        assert!(!bs.contains(130));
        assert!(!bs.contains(u32::MAX));
        assert_eq!(bs.cardinality(), 3);
    }

    #[test]
    fn intersects_and_intersects_words() {
        let a = TargetBitset::of(128, [3, 70]);
        let b = TargetBitset::of(128, [70]);
        let c = TargetBitset::of(128, [4]);
        assert!(a.intersects(&b));
        assert!(!a.intersects(&c));
        assert!(a.intersects_words(&b.to_words()));
        assert!(!a.intersects_words(&c.to_words()));
    }

    #[test]
    fn all_and_empty() {
        let all = TargetBitset::all(65);
        assert_eq!(all.cardinality(), 65);
        assert!(all.contains(64));
        let empty = TargetBitset::empty(65);
        assert_eq!(empty.cardinality(), 0);
        assert!(!empty.contains(0));
    }

    #[test]
    #[should_panic(expected = "out of range")]
    fn of_rejects_out_of_range_ordinal() {
        let _ = TargetBitset::of(4, [4]);
    }
}
