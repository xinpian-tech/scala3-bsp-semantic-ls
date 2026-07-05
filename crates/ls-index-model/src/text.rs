//! Line/character positions, spans, and the columnar position packing.
//!
//! Coordinates are zero-based and match both SemanticDB `Range` and LSP
//! `Position` semantics (end exclusive at the protocol level; [`Span::contains`]
//! is end-inclusive because it is used for cursor hit-testing).

/// A zero-based line/character position.
///
/// Ordering is lexicographic on `(line, character)`, so `a <= b` reproduces the
/// Scala `Pos.<=`.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Hash)]
pub struct Pos {
    pub line: u32,
    pub character: u32,
}

impl Pos {
    #[inline]
    pub const fn new(line: u32, character: u32) -> Self {
        Pos { line, character }
    }
}

/// A span over zero-based line/character coordinates.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub struct Span {
    pub start_line: u32,
    pub start_char: u32,
    pub end_line: u32,
    pub end_char: u32,
}

impl Span {
    /// Bits reserved for the character component in the packed encoding.
    pub const CHAR_BITS: u32 = 12;
    /// Mask isolating the character component of a packed position.
    pub const CHAR_MASK: u32 = (1 << Self::CHAR_BITS) - 1;
    /// Largest line that survives packing without saturation.
    pub const LINE_MAX: u32 = (1 << 20) - 1;

    #[inline]
    pub const fn new(start_line: u32, start_char: u32, end_line: u32, end_char: u32) -> Self {
        Span {
            start_line,
            start_char,
            end_line,
            end_char,
        }
    }

    /// End-inclusive hit test: is `(line, character)` within `[start, end]`?
    pub fn contains(&self, line: u32, character: u32) -> bool {
        let after_start =
            line > self.start_line || (line == self.start_line && character >= self.start_char);
        let before_end =
            line < self.end_line || (line == self.end_line && character <= self.end_char);
        after_start && before_end
    }

    /// Pack a single position into one `u32` as `(line << 12) | char`, the
    /// columnar postings encoding. Lines above [`Span::LINE_MAX`] and characters
    /// above [`Span::CHAR_MASK`] saturate rather than overflow into neighbours.
    ///
    /// The Scala original returns a signed `Int` with the identical bit pattern
    /// (negative once bit 31 is set, i.e. for lines `>= 524288`); this port
    /// exposes it as `u32`, so packed positions always order correctly under
    /// unsigned comparison — the ordering the columnar postings rely on.
    #[inline]
    pub const fn pack(line: u32, character: u32) -> u32 {
        let l = if line < Self::LINE_MAX {
            line
        } else {
            Self::LINE_MAX
        };
        let c = if character < Self::CHAR_MASK {
            character
        } else {
            Self::CHAR_MASK
        };
        (l << Self::CHAR_BITS) | c
    }

    /// The line component of a packed position.
    #[inline]
    pub const fn unpack_line(packed: u32) -> u32 {
        packed >> Self::CHAR_BITS
    }

    /// The character component of a packed position.
    #[inline]
    pub const fn unpack_char(packed: u32) -> u32 {
        packed & Self::CHAR_MASK
    }
}

/// A resolved location in a workspace source file.
#[derive(Clone, PartialEq, Eq, Debug, Hash)]
pub struct Loc {
    pub uri: String,
    pub span: Span,
}

impl Loc {
    #[inline]
    pub fn new(uri: impl Into<String>, span: Span) -> Self {
        Loc {
            uri: uri.into(),
            span,
        }
    }
}

#[cfg(test)]
mod runtime_contract_suite {
    // Mirrors `RuntimeContractSuite` (packing) from the Scala module.
    use super::*;

    #[test]
    fn pack_round_trips_line_and_character() {
        let p = Span::pack(1234, 56);
        assert_eq!(Span::unpack_line(p), 1234);
        assert_eq!(Span::unpack_char(p), 56);
    }

    #[test]
    fn pack_saturates_characters_beyond_twelve_bits() {
        let p = Span::pack(3, 5000);
        assert_eq!(Span::unpack_line(p), 3);
        assert_eq!(Span::unpack_char(p), Span::CHAR_MASK);
    }

    #[test]
    fn pack_orders_by_line_then_char() {
        assert!(Span::pack(1, 4095) < Span::pack(2, 0));
        assert!(Span::pack(7, 3) < Span::pack(7, 4));
    }

    #[test]
    fn contains_is_end_inclusive_at_boundaries() {
        let s = Span::new(2, 4, 2, 10);
        assert!(s.contains(2, 4));
        assert!(s.contains(2, 10));
        assert!(!s.contains(2, 3));
        assert!(!s.contains(2, 11));
        assert!(!s.contains(1, 5));
        assert!(!s.contains(3, 5));
    }

    #[test]
    fn pos_orders_lexicographically() {
        assert!(Pos::new(1, 9) <= Pos::new(2, 0));
        assert!(Pos::new(3, 4) <= Pos::new(3, 4));
        assert!(Pos::new(3, 5) > Pos::new(3, 4));
    }
}
