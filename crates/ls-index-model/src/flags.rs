//! Occurrence roles and per-occurrence bit flags.

/// Occurrence role, mirroring SemanticDB `SymbolOccurrence.Role`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Hash)]
pub enum Role {
    Reference,
    Definition,
}

/// Bit flags stored per occurrence in the postings. Exact facts, never guesses.
///
/// Modelled as a module of `u32` masks plus [`occ_flags::has`], mirroring the
/// Scala `OccFlags` object; a raw `u32` carries the combined flag set.
pub mod occ_flags {
    pub const DEFINITION: u32 = 1 << 0;
    pub const EDITABLE: u32 = 1 << 1;
    pub const GENERATED: u32 = 1 << 2;
    pub const READONLY: u32 = 1 << 3;
    pub const SYNTHETIC: u32 = 1 << 4;

    /// Is `bit` set within `flags`?
    #[inline]
    pub const fn has(flags: u32, bit: u32) -> bool {
        flags & bit != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occ_flags_membership() {
        let flags = occ_flags::DEFINITION | occ_flags::EDITABLE;
        assert!(occ_flags::has(flags, occ_flags::DEFINITION));
        assert!(occ_flags::has(flags, occ_flags::EDITABLE));
        assert!(!occ_flags::has(flags, occ_flags::GENERATED));
        assert!(!occ_flags::has(0, occ_flags::SYNTHETIC));
    }

    #[test]
    fn role_variants_are_distinct() {
        assert_ne!(Role::Reference, Role::Definition);
    }
}
