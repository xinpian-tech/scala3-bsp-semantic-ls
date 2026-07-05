//! The layout canary is deterministic, non-trivial, and sensitive to any drift
//! in a layout fact — the property that lets the island refuse a mismatched
//! Rust vtable at registration.

use ls_pc_abi::{compute_layout_canary, ABI_VERSION, LAYOUT_CANARY};

#[test]
fn canary_is_deterministic_and_nonzero() {
    assert_eq!(compute_layout_canary(), LAYOUT_CANARY);
    assert_eq!(compute_layout_canary(), compute_layout_canary());
    assert_ne!(LAYOUT_CANARY, 0);
}

#[test]
fn abi_version_is_one() {
    assert_eq!(ABI_VERSION, 1);
}

/// Re-implements the canary's FNV-1a-over-facts hashing so we can perturb a
/// single fact and confirm the digest changes. This is the guarantee the real
/// canary relies on: a one-field size/offset change flips the value, so the two
/// sides cannot silently disagree on the layout.
fn fnv_over_facts(facts: &[u64]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for &value in facts {
        let mut byte = 0;
        while byte < 8 {
            hash ^= (value >> (byte * 8)) & 0xff;
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
            byte += 1;
        }
    }
    hash
}

#[test]
fn canary_algorithm_detects_single_fact_drift() {
    // A representative fact vector (primitive sizes + a vtable size).
    let base = [16u64, 16, 16, 8, 8, 16, 28, 64, 128];
    let baseline = fnv_over_facts(&base);
    assert_eq!(fnv_over_facts(&base), baseline);

    for i in 0..base.len() {
        let mut drifted = base;
        drifted[i] += 4; // e.g. a struct grew by one 4-byte field
        assert_ne!(
            fnv_over_facts(&drifted),
            baseline,
            "drift in fact {i} did not change the canary"
        );
    }

    // Reordering two facts must also change the digest (slot order is part of
    // the contract, not just the multiset of sizes).
    let mut swapped = base;
    swapped.swap(7, 8);
    assert_ne!(fnv_over_facts(&swapped), baseline);
}
