//! The flat `#[repr(C)]` boundary types: the two vtables (Rust callbacks + the
//! 15-op PC surface), the string/list/buffer primitives, and the fixed record
//! structs. Every field is a fixed-width integer, pointer, or another such
//! struct, so a record's byte image equals the concatenation of its little-
//! endian fields with no padding — which is exactly what the flat codec writes
//! and the Java layout mirror reads.

use std::ffi::c_void;

/// Boundary contract version, checked at registration.
pub const ABI_VERSION: u64 = 1;

/// Shared status codes returned by every boundary function (`0` = ok).
pub const STATUS_OK: i32 = 0;
/// A Rust panic (or Java `Throwable`) was caught at the boundary.
pub const STATUS_PANIC: i32 = -1;
/// A required argument was null or otherwise invalid.
pub const STATUS_BAD_ARG: i32 = -2;
/// `abi_version` / layout canary disagreement at registration.
pub const STATUS_ABI_MISMATCH: i32 = -3;
/// A response/request payload failed to decode.
pub const STATUS_DECODE: i32 = -4;
/// A response allocation failed.
pub const STATUS_ALLOC: i32 = -5;
/// An unexpected internal error.
pub const STATUS_INTERNAL: i32 = -6;

// ---------------------------------------------------------------------------
// Primitives.
// ---------------------------------------------------------------------------

/// A borrowed UTF-8 string argument (no NUL), valid only for the call.
#[repr(C)]
pub struct LsStr {
    pub ptr: *const u8,
    pub len: u32,
}

/// A borrowed opaque byte buffer argument (an encoded payload), valid only for
/// the call.
#[repr(C)]
pub struct LsBytes {
    pub ptr: *const u8,
    pub len: u32,
}

/// A Rust-owned response buffer returned to the caller through an out-param.
/// The caller frees it with the Rust vtable `free`.
#[repr(C)]
pub struct LsBuf {
    pub ptr: *mut u8,
    pub len: u32,
}

/// A string inside a response buffer's trailing blob: `offset` is relative to
/// the blob start, `len` is the byte length.
#[repr(C)]
pub struct BlobStr {
    pub offset: u32,
    pub len: u32,
}

/// A zero-based `line`/`character` position (UTF-16 code units, as LSP).
#[repr(C)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// A flattened `[start, end)` range (avoids a nested struct in records).
#[repr(C)]
pub struct AbiRange {
    pub start_line: u32,
    pub start_character: u32,
    pub end_line: u32,
    pub end_character: u32,
}

/// A definition/reference location record: a blob-referenced uri, a range, and
/// a `DefinitionOrigin` ordinal (`0` when the op carries no origin).
#[repr(C)]
pub struct LocationRecord {
    pub uri: BlobStr,
    pub range: AbiRange,
    pub origin: u32,
}

// ---------------------------------------------------------------------------
// Rust vtable (handed to the premain; its address is the agent argument).
// ---------------------------------------------------------------------------

/// Response-buffer allocation: Rust owns all cross-boundary memory. `alloc`
/// returns a buffer of `size` bytes (or null); the consumer later `free`s it.
pub type AllocFn = unsafe extern "C" fn(size: u32) -> *mut u8;
/// Frees an `alloc`-obtained buffer of the given size.
pub type FreeFn = unsafe extern "C" fn(ptr: *mut u8, size: u32);
/// Island → Rust structured logging.
pub type LogFn = unsafe extern "C" fn(level: i32, ptr: *const u8, len: u32);
/// Island → Rust registration of the PC vtable; returns a status code.
pub type RegisterPcVtableFn = unsafe extern "C" fn(pc: *const PcVtable) -> i32;
/// Entry point a Java-loaned thread enters and never returns from.
pub type PcDispatchLoopFn = unsafe extern "C" fn(worker_index: i32);
/// Index-backed cross-file go-to-definition callback: resolves `symbol` (with
/// the requesting buffer `from_uri`) into a locations response written to `out`.
pub type SymbolDefinitionFn =
    unsafe extern "C" fn(symbol: LsStr, from_uri: LsStr, out: *mut LsBuf) -> i32;

/// The Rust vtable handed to the premain. The island mirrors this layout
/// through jextract-generated FFM bindings; `layout_canary` is recomputed
/// independently and a mismatch refuses registration.
#[repr(C)]
pub struct RustVtable {
    pub abi_version: u64,
    pub layout_canary: u64,
    pub alloc: AllocFn,
    pub free: FreeFn,
    pub log: LogFn,
    pub register_pc_vtable: RegisterPcVtableFn,
    pub pc_dispatch_loop: PcDispatchLoopFn,
    pub symbol_definition: SymbolDefinitionFn,
}

// ---------------------------------------------------------------------------
// PC vtable (15 ops, built by the island as FFM upcall stubs).
// ---------------------------------------------------------------------------

/// A request carrying an encoded payload buffer (register_target/did_open/
/// did_change). Returns a status code.
pub type PcRequestFn = unsafe extern "C" fn(params_ptr: *const u8, params_len: u32) -> i32;
/// A request carrying a single uri (did_close).
pub type PcUriFn = unsafe extern "C" fn(uri: LsStr) -> i32;
/// A position query (completion/hover/signature_help/definition/
/// type_definition/prepare_rename): writes its response payload to `out`.
pub type PcQueryFn =
    unsafe extern "C" fn(uri: LsStr, line: u32, character: u32, out: *mut LsBuf) -> i32;
/// Completion-item resolve: the owning target, the item's symbol, and the
/// encoded item; writes the enriched item to `out`.
pub type PcResolveFn = unsafe extern "C" fn(
    target_id: LsStr,
    symbol: LsStr,
    item_ptr: *const u8,
    item_len: u32,
    out: *mut LsBuf,
) -> i32;
/// A no-argument query that writes a response payload to `out` (plugin_status).
pub type PcStatusOutFn = unsafe extern "C" fn(out: *mut LsBuf) -> i32;
/// A no-argument lifecycle op (restart_instances/shutdown).
pub type PcVoidFn = unsafe extern "C" fn() -> i32;
/// Spawns a fresh loaned dispatch thread for the given generation.
pub type PcSpawnDispatchFn = unsafe extern "C" fn(generation: u32) -> i32;

/// The 15-op PC vtable. Slot order is the cross-language contract.
#[repr(C)]
pub struct PcVtable {
    pub abi_version: u64,
    pub register_target: PcRequestFn,
    pub did_open: PcRequestFn,
    pub did_change: PcRequestFn,
    pub did_close: PcUriFn,
    pub completion: PcQueryFn,
    pub completion_resolve: PcResolveFn,
    pub hover: PcQueryFn,
    pub signature_help: PcQueryFn,
    pub definition: PcQueryFn,
    pub type_definition: PcQueryFn,
    pub prepare_rename: PcQueryFn,
    pub plugin_status: PcStatusOutFn,
    pub restart_instances: PcVoidFn,
    pub shutdown: PcVoidFn,
    pub spawn_dispatch: PcSpawnDispatchFn,
}

// ---------------------------------------------------------------------------
// Layout assertions. The island reads slots at fixed offsets, so these sizes
// and offsets are the binary contract and must not drift.
// ---------------------------------------------------------------------------

const _: () = {
    assert!(std::mem::size_of::<*const c_void>() == 8);
    assert!(std::mem::size_of::<LsStr>() == 16);
    assert!(std::mem::size_of::<LsBytes>() == 16);
    assert!(std::mem::size_of::<LsBuf>() == 16);
    assert!(std::mem::size_of::<BlobStr>() == 8);
    assert!(std::mem::size_of::<Position>() == 8);
    assert!(std::mem::size_of::<AbiRange>() == 16);
    assert!(std::mem::size_of::<LocationRecord>() == 28);
    // RustVtable: two u64 + 6 fn pointers.
    assert!(std::mem::size_of::<RustVtable>() == 64);
    assert!(std::mem::offset_of!(RustVtable, abi_version) == 0);
    assert!(std::mem::offset_of!(RustVtable, layout_canary) == 8);
    assert!(std::mem::offset_of!(RustVtable, alloc) == 16);
    assert!(std::mem::offset_of!(RustVtable, symbol_definition) == 56);
    // PcVtable: one u64 + 15 fn pointers.
    assert!(std::mem::size_of::<PcVtable>() == 128);
    assert!(std::mem::offset_of!(PcVtable, abi_version) == 0);
    assert!(std::mem::offset_of!(PcVtable, register_target) == 8);
    assert!(std::mem::offset_of!(PcVtable, spawn_dispatch) == 120);
};
