//! Generational `workspace-state-<generation>.bin` — a binary, versioned,
//! checksummed container over an opaque payload (the cross-generation residue:
//! uri → epoch counter, md5, mtime, flags — defined by the ingest layer later).
//!
//! Header (little-endian, 32 bytes):
//! ```text
//! magic            u32   @0   = STATE_MAGIC
//! version          u16   @4   = STATE_VERSION
//! flags            u16   @6   = 0 (reserved)
//! generation       u64   @8
//! payload_len      u64   @16
//! payload_checksum u32   @24  = crc32c(payload)
//! header_checksum  u32   @28  = crc32c(header[0..28])
//! payload          [u8]  @32
//! ```
//! Published with the same fsync + atomic-rename protocol as segments; the
//! reader validates every field and then cross-checks generation + checksum
//! against `manifest.json`, so a mismatched pair is a typed refusal.

use std::path::Path;

use crate::crc::crc32c;
use crate::durable::{atomic_write, fsync_dir, write_tmp};
use crate::error::{StoreError, StoreResult};
use crate::wire::{read_u16, read_u32, read_u64, LeBuf};

const STATE_MAGIC: u32 = 0x4c53_5754; // "LSWT"
/// Highest workspace-state schema version this build understands.
pub const STATE_VERSION: u16 = 1;
const HEADER_SIZE: usize = 32;
const HEADER_CHECKSUM_OFFSET: usize = 28;

/// A validated workspace-state generation: its generation number and opaque
/// payload bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceState {
    pub generation: u64,
    pub payload: Vec<u8>,
}

pub(crate) fn state_file_name(generation: u64) -> String {
    format!("workspace-state-{generation}.bin")
}

pub(crate) fn state_tmp_name(generation: u64) -> String {
    format!("workspace-state-{generation}.bin.tmp")
}

/// Serialize a `generation` + `payload` into the on-disk container bytes.
pub(crate) fn serialize(generation: u64, payload: &[u8]) -> Vec<u8> {
    let mut buf = LeBuf::with_capacity(HEADER_SIZE + payload.len());
    buf.put_u32(STATE_MAGIC);
    buf.put_u16(STATE_VERSION);
    buf.put_u16(0); // flags
    buf.put_u64(generation);
    buf.put_u64(payload.len() as u64);
    buf.put_u32(crc32c(payload));
    buf.put_u32(crc32c(&buf.as_slice()[..HEADER_CHECKSUM_OFFSET]));
    buf.put_bytes(payload);
    buf.into_vec()
}

/// CRC32C of a payload — the value recorded in the manifest.
pub(crate) fn payload_checksum(payload: &[u8]) -> u32 {
    crc32c(payload)
}

/// Write the state file's tmp (fsync'd, not renamed). Split out so the publish
/// protocol can inject a failpoint before the rename.
pub(crate) fn write_state_tmp(root: &Path, generation: u64, payload: &[u8]) -> StoreResult<()> {
    write_tmp(
        root,
        &state_tmp_name(generation),
        &serialize(generation, payload),
    )
}

/// Atomically rename an already-written state tmp into place and fsync `root`.
pub(crate) fn commit_state_tmp(root: &Path, generation: u64) -> StoreResult<()> {
    std::fs::rename(
        root.join(state_tmp_name(generation)),
        root.join(state_file_name(generation)),
    )?;
    fsync_dir(root)?;
    Ok(())
}

/// Write and durably publish a state generation in one call (tmp+fsync+rename+
/// fsync-dir). Used outside the failpoint-instrumented publish path.
#[allow(dead_code)]
pub(crate) fn publish(root: &Path, generation: u64, payload: &[u8]) -> StoreResult<()> {
    atomic_write(
        root,
        &state_tmp_name(generation),
        &state_file_name(generation),
        &serialize(generation, payload),
    )
}

/// Read and validate `root/workspace-state-<generation>.bin`, requiring its
/// generation and payload checksum to match the manifest's paired values.
pub(crate) fn load(
    root: &Path,
    generation: u64,
    expected_checksum: u32,
) -> StoreResult<WorkspaceState> {
    let corrupt = |detail: String| StoreError::StateCorrupt { detail };
    let bytes = std::fs::read(root.join(state_file_name(generation)))?;
    if bytes.len() < HEADER_SIZE {
        return Err(corrupt(format!("truncated header: {} bytes", bytes.len())));
    }
    if read_u32(&bytes, 0).unwrap() != STATE_MAGIC {
        return Err(corrupt("bad magic".into()));
    }
    let version = read_u16(&bytes, 4).unwrap();
    if version > STATE_VERSION {
        return Err(StoreError::FutureSchema {
            what: "workspace-state".into(),
            found: version as u64,
        });
    }
    if version != STATE_VERSION {
        return Err(corrupt(format!("unsupported version {version}")));
    }
    if read_u16(&bytes, 6).unwrap() != 0 {
        return Err(corrupt("flags nonzero".into()));
    }
    let stored_header_crc = read_u32(&bytes, HEADER_CHECKSUM_OFFSET).unwrap();
    if stored_header_crc != crc32c(&bytes[..HEADER_CHECKSUM_OFFSET]) {
        return Err(corrupt("header checksum mismatch".into()));
    }
    let file_generation = read_u64(&bytes, 8).unwrap();
    let payload_len = read_u64(&bytes, 16).unwrap();
    let stored_payload_crc = read_u32(&bytes, 24).unwrap();
    // usize::try_from guards a 32-bit host; on 64-bit it never fails.
    let payload_len = usize::try_from(payload_len)
        .map_err(|_| corrupt(format!("payload_len {payload_len} too large")))?;
    let end = HEADER_SIZE
        .checked_add(payload_len)
        .ok_or_else(|| corrupt("payload_len overflow".into()))?;
    if bytes.len() != end {
        return Err(corrupt(format!(
            "payload length {} != file body {}",
            payload_len,
            bytes.len() - HEADER_SIZE
        )));
    }
    let payload = bytes[HEADER_SIZE..end].to_vec();
    if crc32c(&payload) != stored_payload_crc {
        return Err(corrupt("payload checksum mismatch".into()));
    }
    // Cross-check against the manifest's paired values.
    if file_generation != generation {
        return Err(StoreError::PairMismatch {
            detail: format!("state generation {file_generation} != manifest {generation}"),
        });
    }
    if stored_payload_crc != expected_checksum {
        return Err(StoreError::PairMismatch {
            detail: "state payload checksum != manifest state_checksum".into(),
        });
    }
    Ok(WorkspaceState {
        generation,
        payload,
    })
}
