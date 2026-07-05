//! The presentation-compiler C ABI: the flat boundary between the Rust process
//! and the embedded-JVM PC island.
//!
//! The island is driven over a pair of `#[repr(C)]` vtables — a Rust vtable of
//! callbacks (allocation, logging, PC-vtable registration, the dispatch-loop
//! entry, and the index-backed cross-file definition callback) and a 15-op PC
//! vtable (the language ops plus loaned-thread spawn). Both are versioned and
//! guarded by a layout canary the two sides recompute independently.
//!
//! Op payloads that do not fit in scalar arguments are carried as flat
//! little-endian buffers ([`codec`]) whose owned mirrors ([`payloads`]) encode
//! and decode losslessly, preserving the nullable-vs-empty distinctions of
//! today's carriers. All cross-boundary memory is Rust-owned ([`memory`]).
//!
//! No JSON crosses this boundary; the reference C header is generated from these
//! definitions by cbindgen (`scripts/regen-pc-abi-bindings.sh`).

pub mod abi;
pub mod canary;
pub mod codec;
pub mod memory;
pub mod payloads;

pub use abi::{
    AbiRange, BlobStr, LocationRecord, LsBuf, LsBytes, LsStr, PcVtable, Position, RustVtable,
    ABI_VERSION, STATUS_ABI_MISMATCH, STATUS_ALLOC, STATUS_BAD_ARG, STATUS_DECODE, STATUS_INTERNAL,
    STATUS_OK, STATUS_PANIC,
};
pub use canary::{compute_layout_canary, LAYOUT_CANARY};
pub use codec::{abi_len, AbiError, Reader, Writer};
