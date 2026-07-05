//! Snapshot lifecycle: the [`Store`] facade owns one immutable [`Snapshot`] at a
//! time behind an [`arc_swap::ArcSwapOption`], publishes new generations with the
//! required durability order (segment → workspace-state → manifest → snapshot
//! swap), recovers the active (segment, state) pair on boot, and janitors a
//! superseded generation's files only after its snapshot drops.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use arc_swap::ArcSwapOption;

use crate::data::SegmentData;
use crate::error::{StoreError, StoreResult};
use crate::manifest::{Manifest, MANIFEST_SCHEMA_VERSION, MANIFEST_TMP};
use crate::reader::SegmentReader;
use crate::workspace_state::{self, WorkspaceState};
use crate::writer::SegmentWriter;
use crate::{durable, format};

/// A deterministic crash point for the publish protocol, so the recovery matrix
/// can be exercised without a real `kill -9`. When a failpoint fires, `publish`
/// stops after the corresponding on-disk side effect and does NOT swap the live
/// snapshot — exactly the state a crash at that point would leave behind.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Failpoint {
    /// No injected crash (the normal path).
    #[default]
    None,
    /// Crash after writing the state tmp, before renaming it into place.
    TornStateTmp,
    /// Crash after the state file is durable, before the manifest is committed.
    AfterStateBeforeManifest,
    /// Crash after writing the manifest tmp, before renaming it into place.
    TornManifestTmp,
}

/// An immutable, opened generation: the validated segment + its paired
/// workspace-state, plus the on-disk paths they own.
#[derive(Debug)]
pub struct Snapshot {
    segment: SegmentReader,
    state: WorkspaceState,
    segment_id: u64,
    generation: u64,
    segment_dir: PathBuf,
    state_path: PathBuf,
}

impl Snapshot {
    /// The opened, validated postings segment.
    pub fn segment(&self) -> &SegmentReader {
        &self.segment
    }
    /// The validated workspace-state paired with this segment.
    pub fn state(&self) -> &WorkspaceState {
        &self.state
    }
    /// Active segment id.
    pub fn segment_id(&self) -> u64 {
        self.segment_id
    }
    /// Active workspace-state generation.
    pub fn generation(&self) -> u64 {
        self.generation
    }
}

/// A superseded generation queued for deletion once its snapshot drops.
struct Retired {
    snapshot: Weak<Snapshot>,
    paths: Vec<PathBuf>,
}

/// The storage facade: recovers/holds the active snapshot and publishes new
/// generations durably.
pub struct Store {
    root: PathBuf,
    current: ArcSwapOption<Snapshot>,
    retired: Mutex<Vec<Retired>>,
    publish_lock: Mutex<()>,
}

impl Store {
    /// Open the store at `root`, removing tmp debris and recovering the active
    /// (segment, state) pair named by `manifest.json` (if any). A fresh root
    /// (no manifest) opens empty.
    pub fn open(root: &Path) -> StoreResult<Store> {
        std::fs::create_dir_all(root)?;
        remove_tmp_debris(root)?;
        let store = Store {
            root: root.to_path_buf(),
            current: ArcSwapOption::empty(),
            retired: Mutex::new(Vec::new()),
            publish_lock: Mutex::new(()),
        };
        if let Some(manifest) = Manifest::load(root)? {
            let snap = Arc::new(store.open_snapshot(&manifest)?);
            store.current.store(Some(snap));
        }
        Ok(store)
    }

    /// The store root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The active snapshot, or `None` for a fresh store. The returned `Arc`
    /// keeps its mmap + state alive across a concurrent publish.
    pub fn current(&self) -> Option<Arc<Snapshot>> {
        self.current.load_full()
    }

    /// Alias for [`Store::current`] — clone-and-retain the active snapshot.
    pub fn retain(&self) -> Option<Arc<Snapshot>> {
        self.current.load_full()
    }

    /// Build a new generation from `data` + `state_payload` and publish it
    /// durably (segment → state → manifest → snapshot swap), returning the new
    /// active snapshot.
    pub fn publish(
        &self,
        data: &SegmentData,
        state_payload: &[u8],
        created_at_ms: i64,
    ) -> StoreResult<Arc<Snapshot>> {
        // Failpoint::None never aborts, so the Option is always Some.
        Ok(self
            .publish_with_failpoint(data, state_payload, created_at_ms, Failpoint::None)?
            .expect("no failpoint published a snapshot"))
    }

    /// Like [`Store::publish`], but stops at `failpoint` (returning `Ok(None)`)
    /// to simulate a crash mid-protocol. Test/fault-injection surface for the
    /// recovery matrix.
    pub fn publish_with_failpoint(
        &self,
        data: &SegmentData,
        state_payload: &[u8],
        created_at_ms: i64,
        failpoint: Failpoint,
    ) -> StoreResult<Option<Arc<Snapshot>>> {
        let _guard = self.publish_lock.lock().unwrap();
        let prev = self.current.load_full();
        let next = prev.as_ref().map_or(1, |s| s.generation + 1);

        // 1. Segment: fully published (fsync files + tmp dir, atomic rename,
        //    fsync segments/) by the existing writer.
        SegmentWriter::write(&self.root, next, data, created_at_ms)?;

        // 2. workspace-state-<next>.bin: tmp + fsync, [failpoint], rename + fsync-dir.
        workspace_state::write_state_tmp(&self.root, next, state_payload)?;
        if failpoint == Failpoint::TornStateTmp {
            return Ok(None);
        }
        workspace_state::commit_state_tmp(&self.root, next)?;
        if failpoint == Failpoint::AfterStateBeforeManifest {
            return Ok(None);
        }

        // 3. manifest.json: the single commit point. tmp + fsync, [failpoint],
        //    rename + fsync-dir.
        let manifest = Manifest {
            schema_version: MANIFEST_SCHEMA_VERSION,
            segment_id: next,
            segment_dir: format!("segments/{}", format::segment_dir_name(next)),
            state_generation: next,
            state_checksum: workspace_state::payload_checksum(state_payload),
            doc_count: data.docs.len() as u32,
            symbol_count: data.symbols.len() as u32,
        };
        durable::write_tmp(&self.root, MANIFEST_TMP, &manifest.to_json())?;
        if failpoint == Failpoint::TornManifestTmp {
            return Ok(None);
        }
        manifest.commit_after_tmp(&self.root)?;

        // 4. Open + swap the snapshot only after the manifest is committed.
        let snap = Arc::new(self.open_snapshot(&manifest)?);
        let old = self.current.swap(Some(snap.clone()));
        if let Some(old) = old {
            self.retire(old);
        }
        self.run_janitor();
        Ok(Some(snap))
    }

    /// Delete a superseded generation's files, but only for retired snapshots
    /// that have fully dropped. Called after each publish; also callable
    /// explicitly once a retained snapshot is released.
    pub fn run_janitor(&self) {
        let mut retired = self.retired.lock().unwrap();
        retired.retain(|r| {
            if r.snapshot.strong_count() != 0 {
                return true; // still held by a live snapshot; keep queued
            }
            for p in &r.paths {
                let _ = if p.is_dir() {
                    std::fs::remove_dir_all(p)
                } else {
                    std::fs::remove_file(p)
                };
            }
            false
        });
    }

    /// Open the (segment, state) pair named by `manifest` and cross-check they
    /// are paired (segment id, generation, checksum, record counts).
    fn open_snapshot(&self, manifest: &Manifest) -> StoreResult<Snapshot> {
        // The manifest must name a safe relative `segments/segment-<digits>`
        // path — never an absolute path or one escaping the store root.
        if !is_canonical_segment_dir(&manifest.segment_dir) {
            return Err(StoreError::ManifestCorrupt {
                detail: format!("non-canonical segment_dir {:?}", manifest.segment_dir),
            });
        }
        let segment_dir = self.root.join(&manifest.segment_dir);
        let segment = SegmentReader::open(&segment_dir)?;
        // Prove the opened segment IS the manifest's segment. Without this a
        // rewritten manifest could point segment_dir at another generation with
        // the same counts and serve a mixed (segment, state) pair.
        if segment.segment_id() != manifest.segment_id {
            return Err(StoreError::PairMismatch {
                detail: format!(
                    "segment header id {} != manifest segment_id {}",
                    segment.segment_id(),
                    manifest.segment_id
                ),
            });
        }
        if segment.doc_count() != manifest.doc_count
            || segment.symbol_count() as u32 != manifest.symbol_count
        {
            return Err(StoreError::PairMismatch {
                detail: "manifest doc/symbol counts disagree with segment".into(),
            });
        }
        let state = workspace_state::load(
            &self.root,
            manifest.state_generation,
            manifest.state_checksum,
        )?;
        let state_path = self
            .root
            .join(workspace_state::state_file_name(manifest.state_generation));
        Ok(Snapshot {
            segment,
            state,
            segment_id: manifest.segment_id,
            generation: manifest.state_generation,
            segment_dir,
            state_path,
        })
    }

    /// Queue an old snapshot's files for deletion once the snapshot drops.
    fn retire(&self, old: Arc<Snapshot>) {
        let paths = vec![old.segment_dir.clone(), old.state_path.clone()];
        let snapshot = Arc::downgrade(&old);
        drop(old); // release our strong ref so the janitor can observe drop
        self.retired
            .lock()
            .unwrap()
            .push(Retired { snapshot, paths });
    }
}

/// Accept only a relative `segments/segment-<digits>` path — no absolute paths,
/// no `..` traversal, no arbitrary names — so a corrupt manifest cannot make the
/// store read outside its own segments directory.
fn is_canonical_segment_dir(dir: &str) -> bool {
    match dir.strip_prefix("segments/segment-") {
        Some(rest) => !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

/// Remove tmp debris left by a crash mid-publish: `manifest.json.tmp`, any
/// `workspace-state-*.bin.tmp`, and the segment writer's `tmp-*` staging dirs.
fn remove_tmp_debris(root: &Path) -> StoreResult<()> {
    let _ = std::fs::remove_file(root.join(MANIFEST_TMP));
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("tmp-") {
            let _ = std::fs::remove_dir_all(entry.path());
        } else if name.starts_with("workspace-state-") && name.ends_with(".bin.tmp") {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    Ok(())
}
