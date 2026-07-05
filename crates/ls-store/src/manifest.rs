//! `manifest.json` — the single commit point that names the active (segment,
//! workspace-state) pair. Written with the atomic tmp+fsync+rename+fsync-dir
//! protocol so the file on disk is always a complete previous generation or a
//! complete new one, never torn.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::durable::{atomic_write, fsync_dir};
use crate::error::{StoreError, StoreResult};

/// Highest `manifest.json` schema version this build understands.
pub const MANIFEST_SCHEMA_VERSION: u32 = 1;

pub(crate) const MANIFEST_FILE: &str = "manifest.json";
pub(crate) const MANIFEST_TMP: &str = "manifest.json.tmp";

/// The active-generation record. Carries everything needed to open the active
/// (segment, state) pair deterministically and to prove they are paired.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    /// Manifest schema version.
    pub schema_version: u32,
    /// Active segment id.
    pub segment_id: u64,
    /// Active segment directory, relative to the store root (e.g.
    /// `segments/segment-000001`).
    pub segment_dir: String,
    /// Paired workspace-state generation.
    pub state_generation: u64,
    /// CRC32C of the paired state payload, cross-checked on open.
    pub state_checksum: u32,
    /// Segment `doc_count`, cross-checked against the opened segment.
    pub doc_count: u32,
    /// Segment `symbol_count`, cross-checked against the opened segment.
    pub symbol_count: u32,
}

impl Manifest {
    /// Serialize to pretty JSON bytes.
    pub(crate) fn to_json(&self) -> Vec<u8> {
        // A fixed struct with primitive fields never fails to serialize.
        serde_json::to_vec_pretty(self).expect("manifest serializes")
    }

    /// Durably commit this manifest into `root` (tmp+fsync+rename+fsync-dir).
    #[allow(dead_code)]
    pub(crate) fn commit(&self, root: &Path) -> StoreResult<()> {
        atomic_write(root, MANIFEST_TMP, MANIFEST_FILE, &self.to_json())
    }

    /// Rename an already-written `manifest.json.tmp` into place and fsync `root`.
    /// Split from [`Manifest::commit`] so the publish protocol can inject a
    /// failpoint between the tmp write and the rename.
    pub(crate) fn commit_after_tmp(&self, root: &Path) -> StoreResult<()> {
        std::fs::rename(root.join(MANIFEST_TMP), root.join(MANIFEST_FILE))?;
        fsync_dir(root)?;
        Ok(())
    }

    /// Load `root/manifest.json`. Returns `Ok(None)` when absent (a fresh store),
    /// a typed error when present but unparseable or from a future schema.
    pub(crate) fn load(root: &Path) -> StoreResult<Option<Manifest>> {
        let path = root.join(MANIFEST_FILE);
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.into()),
        };
        let manifest: Manifest =
            serde_json::from_slice(&bytes).map_err(|e| StoreError::ManifestCorrupt {
                detail: format!("parse: {e}"),
            })?;
        if manifest.schema_version > MANIFEST_SCHEMA_VERSION {
            return Err(StoreError::FutureSchema {
                what: "manifest".into(),
                found: manifest.schema_version as u64,
            });
        }
        Ok(Some(manifest))
    }
}
