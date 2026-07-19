//! The flat `#[repr(C)]` boundary types: the two vtables (Rust callbacks + the
//! 22-op PC surface), the string/list/buffer primitives, and the fixed record
//! structs. Every field is a fixed-width integer, pointer, or another such
//! struct, so a record's byte image equals the concatenation of its little-
//! endian fields with no padding — which is exactly what the flat codec writes
//! and the Java layout mirror reads.

use std::ffi::c_void;

/// Boundary contract version, checked at registration. Version 2 adds the seven
/// payload-query PC ops (`inlay_hints`/`semantic_tokens`/`selection_range`/
/// `code_action`/`auto_imports`/`pc_diagnostics`/`folding_range`) and the
/// `definition_source_toplevels` Rust-vtable callback.
pub const ABI_VERSION: u64 = 2;

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
/// The op crossed the boundary but its island-side provider has not landed yet
/// (the transport-stub answer of a freshly added op). A distinct nonzero status
/// so the Rust side surfaces it as a typed backend error (degrading to the
/// query's empty fallback), never a panic; the provider task replaces the stub
/// and retires this answer per op.
pub const STATUS_NOT_YET: i32 = -7;

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
/// Index-backed workspace method search callback (the PC `SymbolSearch.
/// searchMethods` seam): resolves `query` (with the requesting build target
/// `bsp_target_id`) into a method-hits response written to `out`.
pub type SearchMethodsFn =
    unsafe extern "C" fn(query: LsStr, bsp_target_id: LsStr, out: *mut LsBuf) -> i32;
/// Index-backed toplevel-symbol callback (the PC `SymbolSearch.
/// definitionSourceToplevels` seam): resolves the SemanticDB `symbol` (with the
/// defining `source_uri`) into a toplevels response written to `out`.
pub type DefinitionSourceToplevelsFn =
    unsafe extern "C" fn(symbol: LsStr, source_uri: LsStr, out: *mut LsBuf) -> i32;

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
    pub search_methods: SearchMethodsFn,
    pub definition_source_toplevels: DefinitionSourceToplevelsFn,
}

// ---------------------------------------------------------------------------
// PC vtable (22 ops, built by the island as FFM upcall stubs).
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
/// A payload-in/payload-out query (inlay_hints/semantic_tokens/selection_range/
/// code_action/auto_imports/pc_diagnostics/folding_range): the request crosses
/// as an encoded payload buffer (the `register_target`-inbound shape) and the
/// response payload is written to `out` (the `plugin_status`-outbound shape).
/// Defined once and reused for every payload-query slot.
pub type PcPayloadQueryFn =
    unsafe extern "C" fn(params_ptr: *const u8, params_len: u32, out: *mut LsBuf) -> i32;

/// The 22-op PC vtable. Slot order is the cross-language contract.
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
    pub inlay_hints: PcPayloadQueryFn,
    pub semantic_tokens: PcPayloadQueryFn,
    pub selection_range: PcPayloadQueryFn,
    pub code_action: PcPayloadQueryFn,
    pub auto_imports: PcPayloadQueryFn,
    pub pc_diagnostics: PcPayloadQueryFn,
    pub folding_range: PcPayloadQueryFn,
}

// ---------------------------------------------------------------------------
// Layout assertions. The island reads slots at fixed offsets, so these sizes
// and offsets are the binary contract and must not drift.
// ---------------------------------------------------------------------------

const _: () = {
    use std::mem::{offset_of, size_of};

    assert!(size_of::<*const c_void>() == 8);

    // Primitive struct sizes + every field offset.
    assert!(size_of::<LsStr>() == 16);
    assert!(offset_of!(LsStr, ptr) == 0);
    assert!(offset_of!(LsStr, len) == 8);
    assert!(size_of::<LsBytes>() == 16);
    assert!(offset_of!(LsBytes, ptr) == 0);
    assert!(offset_of!(LsBytes, len) == 8);
    assert!(size_of::<LsBuf>() == 16);
    assert!(offset_of!(LsBuf, ptr) == 0);
    assert!(offset_of!(LsBuf, len) == 8);
    assert!(size_of::<BlobStr>() == 8);
    assert!(offset_of!(BlobStr, offset) == 0);
    assert!(offset_of!(BlobStr, len) == 4);
    assert!(size_of::<Position>() == 8);
    assert!(offset_of!(Position, line) == 0);
    assert!(offset_of!(Position, character) == 4);
    assert!(size_of::<AbiRange>() == 16);
    assert!(offset_of!(AbiRange, start_line) == 0);
    assert!(offset_of!(AbiRange, start_character) == 4);
    assert!(offset_of!(AbiRange, end_line) == 8);
    assert!(offset_of!(AbiRange, end_character) == 12);
    assert!(size_of::<LocationRecord>() == 28);
    assert!(offset_of!(LocationRecord, uri) == 0);
    assert!(offset_of!(LocationRecord, range) == 8);
    assert!(offset_of!(LocationRecord, origin) == 24);

    // RustVtable: two u64 + 8 fn pointers; assert every slot offset.
    assert!(size_of::<RustVtable>() == 80);
    assert!(offset_of!(RustVtable, abi_version) == 0);
    assert!(offset_of!(RustVtable, layout_canary) == 8);
    assert!(offset_of!(RustVtable, alloc) == 16);
    assert!(offset_of!(RustVtable, free) == 24);
    assert!(offset_of!(RustVtable, log) == 32);
    assert!(offset_of!(RustVtable, register_pc_vtable) == 40);
    assert!(offset_of!(RustVtable, pc_dispatch_loop) == 48);
    assert!(offset_of!(RustVtable, symbol_definition) == 56);
    assert!(offset_of!(RustVtable, search_methods) == 64);
    assert!(offset_of!(RustVtable, definition_source_toplevels) == 72);

    // PcVtable: one u64 + 22 fn pointers; assert every slot offset.
    assert!(size_of::<PcVtable>() == 184);
    assert!(offset_of!(PcVtable, abi_version) == 0);
    assert!(offset_of!(PcVtable, register_target) == 8);
    assert!(offset_of!(PcVtable, did_open) == 16);
    assert!(offset_of!(PcVtable, did_change) == 24);
    assert!(offset_of!(PcVtable, did_close) == 32);
    assert!(offset_of!(PcVtable, completion) == 40);
    assert!(offset_of!(PcVtable, completion_resolve) == 48);
    assert!(offset_of!(PcVtable, hover) == 56);
    assert!(offset_of!(PcVtable, signature_help) == 64);
    assert!(offset_of!(PcVtable, definition) == 72);
    assert!(offset_of!(PcVtable, type_definition) == 80);
    assert!(offset_of!(PcVtable, prepare_rename) == 88);
    assert!(offset_of!(PcVtable, plugin_status) == 96);
    assert!(offset_of!(PcVtable, restart_instances) == 104);
    assert!(offset_of!(PcVtable, shutdown) == 112);
    assert!(offset_of!(PcVtable, spawn_dispatch) == 120);
    assert!(offset_of!(PcVtable, inlay_hints) == 128);
    assert!(offset_of!(PcVtable, semantic_tokens) == 136);
    assert!(offset_of!(PcVtable, selection_range) == 144);
    assert!(offset_of!(PcVtable, code_action) == 152);
    assert!(offset_of!(PcVtable, auto_imports) == 160);
    assert!(offset_of!(PcVtable, pc_diagnostics) == 168);
    assert!(offset_of!(PcVtable, folding_range) == 176);
};
