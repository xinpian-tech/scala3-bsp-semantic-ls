//! CRC32C (Castagnoli), matching `java.util.zip.CRC32C` used by the Scala
//! `ls.postings` writer so a Rust-written segment validates identically. Stored
//! on disk as a `uint32` zero-extended into an `int64` field.

/// CRC32C of a byte slice.
#[inline]
pub fn crc32c(bytes: &[u8]) -> u32 {
    crc32c::crc32c(bytes)
}
