//! The Rust side of the single-sided memory rule: every cross-boundary buffer
//! is allocated and freed here. [`abi_alloc`] / [`abi_free`] back the Rust
//! vtable's `alloc`/`free` slots; the island calls `alloc` to obtain a response
//! buffer, writes the encoded payload, and returns it, and the Rust consumer
//! `free`s it once decoded. A live-allocation counter lets tests prove no buffer
//! is leaked across many round-trips.

use std::alloc::{alloc, dealloc, Layout};
use std::sync::atomic::{AtomicI64, Ordering};

use crate::abi::LsBuf;

/// Buffers are 8-byte aligned so the island can place any record without an
/// under-aligned access.
const ALIGN: usize = 8;

static LIVE_ALLOCATIONS: AtomicI64 = AtomicI64::new(0);

/// The number of buffers currently outstanding (`alloc`ed but not yet `free`d).
/// Zero at rest; a leak leaves it positive.
pub fn live_allocations() -> i64 {
    LIVE_ALLOCATIONS.load(Ordering::SeqCst)
}

fn layout_for(size: u32) -> Option<Layout> {
    Layout::from_size_align(size as usize, ALIGN).ok()
}

/// Allocates a response buffer of `size` bytes, or returns null (size `0` or an
/// invalid layout). Counted as one live allocation.
///
/// # Safety
/// The returned pointer must be freed exactly once with [`abi_free`] using the
/// same `size`.
pub unsafe extern "C" fn abi_alloc(size: u32) -> *mut u8 {
    match layout_for(size) {
        Some(layout) if layout.size() > 0 => {
            // SAFETY: `layout` has a non-zero size.
            let ptr = unsafe { alloc(layout) };
            if !ptr.is_null() {
                LIVE_ALLOCATIONS.fetch_add(1, Ordering::SeqCst);
            }
            ptr
        }
        _ => std::ptr::null_mut(),
    }
}

/// Frees a buffer obtained from [`abi_alloc`]. A null pointer or zero size is a
/// no-op.
///
/// # Safety
/// `ptr` must have come from [`abi_alloc`] with the same `size` and must not
/// have been freed already.
pub unsafe extern "C" fn abi_free(ptr: *mut u8, size: u32) {
    if ptr.is_null() {
        return;
    }
    if let Some(layout) = layout_for(size) {
        if layout.size() > 0 {
            // SAFETY: `ptr` came from `abi_alloc` with this same layout.
            unsafe { dealloc(ptr, layout) };
            LIVE_ALLOCATIONS.fetch_sub(1, Ordering::SeqCst);
        }
    }
}

/// Copies an encoded payload into a freshly `abi_alloc`ed buffer and reports it
/// through `out`. This is what a boundary op does with an `encode()` result:
/// measure once, allocate once, copy, hand ownership to the caller. Returns
/// `false` (leaving `out` empty) if allocation fails.
///
/// # Safety
/// `out` must be a valid, writable `LsBuf` pointer.
pub unsafe fn write_response(payload: &[u8], out: *mut LsBuf) -> bool {
    let len = payload.len() as u32;
    // SAFETY: caller guarantees `out` is writable.
    unsafe {
        (*out).ptr = std::ptr::null_mut();
        (*out).len = 0;
    }
    if payload.is_empty() {
        return true;
    }
    // SAFETY: `abi_alloc`'s contract.
    let buf = unsafe { abi_alloc(len) };
    if buf.is_null() {
        return false;
    }
    // SAFETY: `buf` points to `len` freshly allocated bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(payload.as_ptr(), buf, payload.len());
        (*out).ptr = buf;
        (*out).len = len;
    }
    true
}
