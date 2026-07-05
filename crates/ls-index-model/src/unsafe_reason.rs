//! Reasons a rename group is rejected.
//!
//! Stored as a `u64` bitmask so the request path is a single integer test;
//! expanded to human-readable messages only on rejection via [`explain`].
//! Bit assignments and messages mirror the Scala `UnsafeReason` object exactly.

pub const EXTERNAL: u64 = 1 << 0;
pub const GENERATED_OCCURRENCE: u64 = 1 << 1;
pub const READONLY_OCCURRENCE: u64 = 1 << 2;
pub const OVERRIDE_FAMILY: u64 = 1 << 3;
pub const SYNTHETIC_ONLY: u64 = 1 << 4;
pub const PC_ONLY: u64 = 1 << 5;
pub const SHARED_SOURCE_DISAGREEMENT: u64 = 1 << 6;
pub const UNSUPPORTED_SYMBOL_FAMILY: u64 = 1 << 7;
pub const DEPENDENCY_SOURCE: u64 = 1 << 8;
pub const OPAQUE_TYPE: u64 = 1 << 9;

/// Expand a reason mask into one static message per set bit, in bit order. An
/// empty mask yields an empty list (the symbol is safe to rename). The messages
/// are compile-time constants; callers needing owned strings (e.g. to build
/// [`LsError::RenameRejected`](crate::LsError::RenameRejected)) collect via
/// `.map(str::to_string)`.
pub fn explain(mask: u64) -> Vec<&'static str> {
    let mut msgs = Vec::new();
    if mask & EXTERNAL != 0 {
        msgs.push("symbol is defined outside the workspace");
    }
    if mask & GENERATED_OCCURRENCE != 0 {
        msgs.push("symbol has occurrences in generated sources");
    }
    if mask & READONLY_OCCURRENCE != 0 {
        msgs.push("symbol has occurrences in readonly sources");
    }
    if mask & OVERRIDE_FAMILY != 0 {
        msgs.push("symbol participates in an override family that cannot be renamed safely");
    }
    if mask & SYNTHETIC_ONLY != 0 {
        msgs.push("symbol only has synthetic occurrences");
    }
    if mask & PC_ONLY != 0 {
        msgs.push("symbol is provided by a PC-only plugin and is not present in fresh SemanticDB");
    }
    if mask & SHARED_SOURCE_DISAGREEMENT != 0 {
        msgs.push("targets sharing this source disagree on the rename group");
    }
    if mask & UNSUPPORTED_SYMBOL_FAMILY != 0 {
        msgs.push("symbol family (e.g. apply/unapply, exported symbol) is not safely renameable");
    }
    if mask & DEPENDENCY_SOURCE != 0 {
        msgs.push("symbol has occurrences in dependency sources");
    }
    if mask & OPAQUE_TYPE != 0 {
        msgs.push("opaque type rename is not supported (conservative policy)");
    }
    msgs
}

#[cfg(test)]
mod target_graph_suite {
    // Mirrors the `UnsafeReason.explain` cases from `TargetGraphSuite`.
    use super::*;

    #[test]
    fn explain_lists_every_set_bit() {
        let mask = EXTERNAL | OVERRIDE_FAMILY;
        let msgs = explain(mask);
        assert_eq!(msgs.len(), 2);
        assert!(msgs.iter().any(|m| m.contains("outside the workspace")));
        assert!(msgs.iter().any(|m| m.contains("override family")));
    }

    #[test]
    fn explain_empty_mask_is_empty() {
        assert!(explain(0).is_empty());
    }

    #[test]
    fn reason_bits_are_disjoint() {
        let all = [
            EXTERNAL,
            GENERATED_OCCURRENCE,
            READONLY_OCCURRENCE,
            OVERRIDE_FAMILY,
            SYNTHETIC_ONLY,
            PC_ONLY,
            SHARED_SOURCE_DISAGREEMENT,
            UNSUPPORTED_SYMBOL_FAMILY,
            DEPENDENCY_SOURCE,
            OPAQUE_TYPE,
        ];
        let mut union = 0u64;
        for bit in all {
            assert_eq!(bit.count_ones(), 1);
            assert_eq!(union & bit, 0, "reason bits must not overlap");
            union |= bit;
        }
        // The full mask explains to exactly one message per reason.
        assert_eq!(explain(union).len(), all.len());
    }
}
