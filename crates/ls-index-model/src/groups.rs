//! The rename-safety profile precomputed per rename group at ingest.
//!
//! Mirrors the Scala `ls.index.RenameProfile`. The reason-bit constants and
//! their messages live in [`crate::unsafe_reason`]; this is the per-group record
//! the rename request path consults.

/// Precomputed at ingest; consulted at rename request time.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RenameProfile {
    pub is_local: bool,
    pub is_external: bool,
    pub has_generated_occurrences: bool,
    pub has_readonly_occurrences: bool,
    pub has_override_family: bool,
    pub has_companion: bool,
    pub editable_occurrence_count: u32,
    /// Reason mask (see [`crate::unsafe_reason`]); `0` means safe to rename.
    pub unsafe_reason_mask: u64,
}

impl RenameProfile {
    /// A rename group is safe exactly when no reason bit is set.
    #[inline]
    pub fn is_safe(&self) -> bool {
        self.unsafe_reason_mask == 0
    }

    /// The all-clear default (`RenameProfile.empty` in Scala).
    pub fn empty() -> Self {
        RenameProfile {
            is_local: false,
            is_external: false,
            has_generated_occurrences: false,
            has_readonly_occurrences: false,
            has_override_family: false,
            has_companion: false,
            editable_occurrence_count: 0,
            unsafe_reason_mask: 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_profile_is_safe() {
        let p = RenameProfile::empty();
        assert!(p.is_safe());
        assert_eq!(p.editable_occurrence_count, 0);
    }

    #[test]
    fn nonzero_mask_is_unsafe() {
        let mut p = RenameProfile::empty();
        p.unsafe_reason_mask = crate::unsafe_reason::EXTERNAL;
        assert!(!p.is_safe());
    }
}
