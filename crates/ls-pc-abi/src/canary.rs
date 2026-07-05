//! A deterministic checksum over the boundary layout facts. The island
//! recomputes the identical value from its jextract `sizeof()`/`$offset()`
//! accessors at bootstrap; a mismatch means the two sides disagree on the
//! binary layout and registration is refused. The fact list and hashing order
//! below are therefore the cross-language contract: FNV-1a over the `u64`s in
//! exactly this order, each hashed as 8 little-endian bytes.

use std::mem::{offset_of, size_of};

use crate::abi::{
    AbiRange, BlobStr, LocationRecord, LsBuf, LsBytes, LsStr, PcVtable, Position, RustVtable,
};

/// FNV-1a (64-bit) offset basis and prime.
const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// The ordered layout facts hashed into the canary. Sizes of every boundary
/// struct plus the fixed slot offsets of both vtables (the fields the island
/// reads by offset).
const fn facts() -> [u64; 25] {
    [
        // Primitive struct sizes.
        size_of::<LsStr>() as u64,
        size_of::<LsBytes>() as u64,
        size_of::<LsBuf>() as u64,
        size_of::<BlobStr>() as u64,
        size_of::<Position>() as u64,
        size_of::<AbiRange>() as u64,
        size_of::<LocationRecord>() as u64,
        // Rust vtable size + slot offsets.
        size_of::<RustVtable>() as u64,
        offset_of!(RustVtable, abi_version) as u64,
        offset_of!(RustVtable, layout_canary) as u64,
        offset_of!(RustVtable, alloc) as u64,
        offset_of!(RustVtable, free) as u64,
        offset_of!(RustVtable, log) as u64,
        offset_of!(RustVtable, register_pc_vtable) as u64,
        offset_of!(RustVtable, pc_dispatch_loop) as u64,
        offset_of!(RustVtable, symbol_definition) as u64,
        // PC vtable size + the anchoring slot offsets.
        size_of::<PcVtable>() as u64,
        offset_of!(PcVtable, abi_version) as u64,
        offset_of!(PcVtable, register_target) as u64,
        offset_of!(PcVtable, completion) as u64,
        offset_of!(PcVtable, completion_resolve) as u64,
        offset_of!(PcVtable, prepare_rename) as u64,
        offset_of!(PcVtable, plugin_status) as u64,
        offset_of!(PcVtable, shutdown) as u64,
        offset_of!(PcVtable, spawn_dispatch) as u64,
    ]
}

/// Computes the layout canary from the ordered facts.
pub const fn compute_layout_canary() -> u64 {
    let facts = facts();
    let mut hash = FNV_OFFSET;
    let mut i = 0;
    while i < facts.len() {
        let value = facts[i];
        let mut byte = 0;
        while byte < 8 {
            hash ^= (value >> (byte * 8)) & 0xff;
            hash = hash.wrapping_mul(FNV_PRIME);
            byte += 1;
        }
        i += 1;
    }
    hash
}

/// The expected layout canary (a compile-time constant).
pub const LAYOUT_CANARY: u64 = compute_layout_canary();
