//! Little-endian encode/decode helpers. Every scalar on disk is little-endian
//! and unaligned (docs/index-format.md), so all encoding/decoding goes through
//! here.

/// A growable little-endian byte buffer used to build one segment file.
#[derive(Default)]
pub struct LeBuf {
    bytes: Vec<u8>,
}

impl LeBuf {
    pub fn with_capacity(n: usize) -> Self {
        LeBuf {
            bytes: Vec::with_capacity(n),
        }
    }

    pub fn put_u16(&mut self, v: u16) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    pub fn put_u32(&mut self, v: u32) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    pub fn put_i32(&mut self, v: i32) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    pub fn put_u64(&mut self, v: u64) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    pub fn put_i64(&mut self, v: i64) {
        self.bytes.extend_from_slice(&v.to_le_bytes());
    }

    pub fn put_bytes(&mut self, b: &[u8]) {
        self.bytes.extend_from_slice(b);
    }

    /// Overwrite the little-endian `u64` at `off` (used for the header
    /// self-checksum trailer).
    pub fn set_u64(&mut self, off: usize, v: u64) {
        self.bytes[off..off + 8].copy_from_slice(&v.to_le_bytes());
    }

    pub fn as_slice(&self) -> &[u8] {
        &self.bytes
    }

    pub fn into_vec(self) -> Vec<u8> {
        self.bytes
    }
}

/// Bounds-checked little-endian readers over a mapped file. `None` means the
/// read runs past the end of the slice (a truncated/corrupt file).
#[inline]
pub fn read_u16(b: &[u8], off: usize) -> Option<u16> {
    b.get(off..off + 2)
        .map(|s| u16::from_le_bytes(s.try_into().unwrap()))
}

#[inline]
pub fn read_u32(b: &[u8], off: usize) -> Option<u32> {
    b.get(off..off + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
}

#[inline]
pub fn read_i32(b: &[u8], off: usize) -> Option<i32> {
    b.get(off..off + 4)
        .map(|s| i32::from_le_bytes(s.try_into().unwrap()))
}

#[inline]
pub fn read_u64(b: &[u8], off: usize) -> Option<u64> {
    b.get(off..off + 8)
        .map(|s| u64::from_le_bytes(s.try_into().unwrap()))
}

#[inline]
pub fn read_i64(b: &[u8], off: usize) -> Option<i64> {
    b.get(off..off + 8)
        .map(|s| i64::from_le_bytes(s.try_into().unwrap()))
}
