//! Store lifecycle + recovery matrix: publish protocol ordering, snapshot
//! retention across a concurrent publish, janitor deferral, and the crash /
//! corruption recovery cases (torn tmp files, crash between state and manifest,
//! future-schema / generation / checksum typed refusals). Crashes are simulated
//! with deterministic `Failpoint`s, not a real `kill -9`.

use std::path::{Path, PathBuf};

use ls_store::{Failpoint, SegmentData, SegmentDoc, SegmentSymbol, Store, StoreError, TargetMeta};

/// A minimal but valid segment: `n_docs` empty docs, one target, `n_syms`
/// pre-sorted symbols, no occurrences.
fn data(n_docs: usize, n_syms: usize) -> SegmentData {
    SegmentData {
        docs: (0..n_docs)
            .map(|i| SegmentDoc {
                uri: format!("file:///D{i}.scala"),
                doc_id: i as i64,
                epoch: 1,
                target_ord: 0,
                generated: false,
                readonly: false,
            })
            .collect(),
        targets: vec![1],
        symbols: (0..n_syms)
            .map(|i| SegmentSymbol {
                semantic_symbol: format!("s{i:04}"),
                symbol_id: i as i64,
                ref_group_ord: -1,
                rename_group_ord: -1,
                def_target_ord: -1,
            })
            .collect(),
        ref_occurrences: vec![],
        def_occurrences: vec![],
        rename_occurrences: vec![],
        rename_profiles: vec![],
        doc_occurrences: (0..n_docs).map(|_| vec![]).collect(),
        target_meta: vec![TargetMeta::default()],
        symbol_meta: vec![],
        search_rows: vec![],
    }
}

fn seg_dir(root: &Path, gen: u64) -> PathBuf {
    root.join(format!("segments/segment-{gen:06}"))
}
fn state_file(root: &Path, gen: u64) -> PathBuf {
    root.join(format!("workspace-state-{gen}.bin"))
}

/// No `*.tmp` / `tmp-*` debris remains anywhere under `root`.
fn assert_no_tmp_debris(root: &Path) {
    assert!(
        !root.join("manifest.json.tmp").exists(),
        "manifest tmp remains"
    );
    for entry in std::fs::read_dir(root).unwrap() {
        let name = entry.unwrap().file_name();
        let name = name.to_string_lossy();
        assert!(
            !(name.starts_with("tmp-") || name.ends_with(".tmp")),
            "tmp debris remains: {name}"
        );
    }
}

// ---- positive: publish + recover ----

#[test]
fn publish_then_reopen_preserves_pair() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    assert!(store.current().is_none());

    let snap = store.publish(&data(3, 2), b"state-gen-1", 0).unwrap();
    assert_eq!(snap.generation(), 1);
    assert_eq!(snap.segment_id(), 1);
    assert_eq!(snap.segment().doc_count(), 3);
    assert_eq!(snap.state().payload, b"state-gen-1");
    drop(snap);
    drop(store);

    // Reboot: the manifest's active pair comes back intact.
    let store = Store::open(tmp.path()).unwrap();
    let snap = store.current().expect("recovered snapshot");
    assert_eq!(snap.generation(), 1);
    assert_eq!(snap.segment().doc_count(), 3);
    assert_eq!(snap.state().payload, b"state-gen-1");
}

#[test]
fn second_publish_increments_generation() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    store.publish(&data(1, 1), b"g1", 0).unwrap();
    let snap2 = store.publish(&data(2, 3), b"g2", 0).unwrap();
    assert_eq!(snap2.generation(), 2);
    assert_eq!(snap2.segment().doc_count(), 2);
    assert_eq!(snap2.segment().symbol_count(), 3);

    let store = Store::open(tmp.path()).unwrap();
    let snap = store.current().unwrap();
    assert_eq!(snap.generation(), 2);
    assert_eq!(snap.state().payload, b"g2");
}

#[test]
fn open_readonly_recovers_without_creating_or_cleaning() {
    let tmp = tempfile::tempdir().unwrap();
    Store::open(tmp.path())
        .unwrap()
        .publish(&data(2, 1), b"g1", 0)
        .unwrap();
    // Debris a live server's in-flight publish would leave behind.
    let debris = tmp.path().join("tmp-inflight");
    std::fs::create_dir_all(&debris).unwrap();

    // A read-only open recovers the active pair but touches nothing.
    let ro = Store::open_readonly(tmp.path()).unwrap();
    assert_eq!(ro.current().unwrap().generation(), 1);
    assert!(debris.exists(), "open_readonly removed tmp debris");

    // A missing root opens empty without being created.
    let missing = tmp.path().join("does-not-exist");
    assert!(Store::open_readonly(&missing).unwrap().current().is_none());
    assert!(!missing.exists(), "open_readonly created a missing root");
}

// ---- retention + janitor (ArcSwap semantics) ----

#[test]
fn retained_snapshot_survives_publish() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    store.publish(&data(2, 2), b"g1", 0).unwrap();
    let held = store.retain().expect("retain gen1");

    store.publish(&data(4, 5), b"g2", 0).unwrap();

    // The retained snapshot keeps its own mmap + state usable across the publish.
    assert_eq!(held.generation(), 1);
    assert_eq!(held.segment().doc_count(), 2);
    assert_eq!(held.state().payload, b"g1");
    assert_eq!(store.current().unwrap().generation(), 2);
}

#[test]
fn janitor_defers_deletion_until_snapshot_drops() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let store = Store::open(root).unwrap();
    store.publish(&data(1, 1), b"g1", 0).unwrap();
    let held = store.retain().expect("retain gen1");

    // Publish gen2: gen1 is superseded but retained, so its files stay.
    store.publish(&data(1, 1), b"g2", 0).unwrap();
    assert!(
        seg_dir(root, 1).is_dir(),
        "gen1 segment deleted while retained"
    );
    assert!(
        state_file(root, 1).is_file(),
        "gen1 state deleted while retained"
    );
    assert!(seg_dir(root, 2).is_dir());
    assert!(state_file(root, 2).is_file());

    // Release the retained snapshot: the janitor may now delete gen1.
    drop(held);
    store.run_janitor();
    assert!(!seg_dir(root, 1).exists(), "gen1 segment not reclaimed");
    assert!(!state_file(root, 1).exists(), "gen1 state not reclaimed");
    // The active generation is never touched.
    assert!(seg_dir(root, 2).is_dir());
    assert!(state_file(root, 2).is_file());
}

fn committed_segment_dirs(root: &Path) -> usize {
    std::fs::read_dir(root.join("segments"))
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .count()
}

#[test]
fn publishes_auto_reclaim_drained_superseded_generations() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let store = Store::open(root).unwrap();

    // Three publishes, retaining nothing. A publish loads the current snapshot
    // into a local before swapping, so it pins the immediately-prior generation
    // across its OWN publish-tail janitor; that generation is reclaimed by the
    // NEXT publish's janitor. So after gen2, gen1 is still on disk; after gen3,
    // gen1 is auto-reclaimed with NO explicit `run_janitor` and no retained
    // snapshot — the publish-time half of the reclaim obligation (drained
    // superseded generations must not accumulate across re-ingests). The
    // retained-then-released half is covered by
    // `janitor_defers_deletion_until_snapshot_drops`.
    store.publish(&data(1, 1), b"g1", 0).unwrap();
    store.publish(&data(1, 1), b"g2", 0).unwrap();
    assert!(
        seg_dir(root, 1).is_dir(),
        "gen1 is pinned across its immediate successor's janitor pass"
    );
    store.publish(&data(1, 1), b"g3", 0).unwrap();
    assert!(
        !seg_dir(root, 1).exists(),
        "gen1 segment not auto-reclaimed by a later publish"
    );
    assert!(
        !state_file(root, 1).exists(),
        "gen1 state not auto-reclaimed by a later publish"
    );
    assert!(seg_dir(root, 3).is_dir());
    assert!(state_file(root, 3).is_file());

    // Superseded generations do not accumulate: only the active generation and
    // at most the just-superseded (still-pinned) one remain on disk.
    assert!(
        committed_segment_dirs(root) <= 2,
        "drained generations accumulated: {} segment dirs",
        committed_segment_dirs(root)
    );

    // Once the last publish's local snapshot is gone, an explicit janitor sweep
    // reclaims the remainder → exactly one committed segment generation.
    store.run_janitor();
    assert_eq!(
        committed_segment_dirs(root),
        1,
        "expected exactly one committed segment dir after the janitor"
    );
    assert!(seg_dir(root, 3).is_dir());
}

// ---- negative: crash recovery (torn tmp / crash windows) ----

#[test]
fn torn_state_tmp_recovers_old() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    store.publish(&data(1, 1), b"g1", 0).unwrap();
    // Crash while writing the gen2 state tmp (before its rename).
    let aborted = store
        .publish_with_failpoint(&data(2, 2), b"g2", 0, Failpoint::TornStateTmp)
        .unwrap();
    assert!(aborted.is_none());
    drop(store);

    let store = Store::open(tmp.path()).unwrap();
    let snap = store.current().expect("old pair recovered");
    assert_eq!(snap.generation(), 1);
    assert_eq!(snap.state().payload, b"g1");
    assert_no_tmp_debris(tmp.path());
}

#[test]
fn crash_after_state_before_manifest_recovers_old() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    store.publish(&data(1, 1), b"g1", 0).unwrap();
    // Crash after the gen2 state file is durable, before the manifest commit:
    // the manifest still names gen1, so boot recovers the OLD pair.
    store
        .publish_with_failpoint(&data(2, 2), b"g2", 0, Failpoint::AfterStateBeforeManifest)
        .unwrap();
    drop(store);

    let store = Store::open(tmp.path()).unwrap();
    let snap = store.current().expect("old pair recovered");
    assert_eq!(snap.generation(), 1);
    assert_eq!(snap.segment().doc_count(), 1);
    assert_eq!(snap.state().payload, b"g1");
    assert_no_tmp_debris(tmp.path());
}

#[test]
fn torn_manifest_tmp_recovers_old() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    store.publish(&data(1, 1), b"g1", 0).unwrap();
    // Crash after writing manifest.json.tmp, before the atomic rename.
    store
        .publish_with_failpoint(&data(2, 2), b"g2", 0, Failpoint::TornManifestTmp)
        .unwrap();
    assert!(tmp.path().join("manifest.json.tmp").exists());
    drop(store);

    let store = Store::open(tmp.path()).unwrap();
    let snap = store.current().expect("old pair recovered");
    assert_eq!(snap.generation(), 1);
    assert_eq!(snap.state().payload, b"g1");
    assert_no_tmp_debris(tmp.path());
}

// ---- negative: typed refusals on a corrupt/mismatched state pair ----

// workspace-state header offsets (little-endian): version@4, generation@8,
// payload_checksum@24, header_checksum@28, payload@32.
const VERSION_OFF: usize = 4;
const GEN_OFF: usize = 8;
const PAYLOAD_CRC_OFF: usize = 24;
const HEADER_CRC_OFF: usize = 28;
const PAYLOAD_OFF: usize = 32;

fn rewrite_state_header_crc(b: &mut [u8]) {
    let crc = crc32c::crc32c(&b[..HEADER_CRC_OFF]);
    b[HEADER_CRC_OFF..HEADER_CRC_OFF + 4].copy_from_slice(&crc.to_le_bytes());
}

/// Publish gen1, then mutate its state file and assert reopen fails typed.
fn corrupt_state_and_reopen(mutate: impl FnOnce(&mut Vec<u8>)) -> StoreError {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    store.publish(&data(1, 1), b"payload-1", 0).unwrap();
    drop(store);
    let path = state_file(tmp.path(), 1);
    let mut bytes = std::fs::read(&path).unwrap();
    mutate(&mut bytes);
    std::fs::write(&path, &bytes).unwrap();
    // `Store` is not `Debug`, so match rather than `.expect_err`.
    match Store::open(tmp.path()) {
        Err(e) => e,
        Ok(_) => panic!("corrupt state must be rejected"),
    }
}

#[test]
fn future_state_schema_rejected() {
    // Bump the version to STATE_VERSION+1; the future-schema check fires before
    // the header checksum, so no recompute is needed.
    let e = corrupt_state_and_reopen(|b| {
        b[VERSION_OFF..VERSION_OFF + 2].copy_from_slice(&2u16.to_le_bytes())
    });
    assert!(matches!(e, StoreError::FutureSchema { .. }), "got {e:?}");
}

#[test]
fn state_generation_mismatch_rejected() {
    // Make the state file internally self-consistent but with generation 2,
    // while the manifest still pairs generation 1 -> PairMismatch.
    let e = corrupt_state_and_reopen(|b| {
        b[GEN_OFF..GEN_OFF + 8].copy_from_slice(&2u64.to_le_bytes());
        rewrite_state_header_crc(b);
    });
    assert!(matches!(e, StoreError::PairMismatch { .. }), "got {e:?}");
}

#[test]
fn state_checksum_mismatch_rejected() {
    // Flip a payload byte and recompute BOTH internal checksums so the file is
    // self-consistent, but its payload checksum no longer matches the manifest's
    // recorded state_checksum -> PairMismatch.
    let e = corrupt_state_and_reopen(|b| {
        b[PAYLOAD_OFF] ^= 0xff;
        let payload_crc = crc32c::crc32c(&b[PAYLOAD_OFF..]);
        b[PAYLOAD_CRC_OFF..PAYLOAD_CRC_OFF + 4].copy_from_slice(&payload_crc.to_le_bytes());
        rewrite_state_header_crc(b);
    });
    assert!(matches!(e, StoreError::PairMismatch { .. }), "got {e:?}");
}

#[test]
fn corrupt_state_header_rejected_without_panic() {
    // A lone payload flip (no checksum recompute) is caught as StateCorrupt.
    let e = corrupt_state_and_reopen(|b| b[PAYLOAD_OFF] ^= 0xff);
    assert!(matches!(e, StoreError::StateCorrupt { .. }), "got {e:?}");
}

// ---- negative: manifest <-> segment pairing ----

/// Replace `from` with `to` in `manifest.json` and return the reopen error.
fn tamper_manifest_and_reopen(root: &Path, from: &str, to: &str) -> StoreError {
    let path = root.join("manifest.json");
    let text = std::fs::read_to_string(&path).unwrap();
    assert!(text.contains(from), "manifest missing {from:?}");
    std::fs::write(&path, text.replace(from, to)).unwrap();
    match Store::open(root) {
        Err(e) => e,
        Ok(_) => panic!("tampered manifest must be rejected"),
    }
}

#[test]
fn manifest_segment_id_mismatch_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    // Two generations with identical doc/symbol counts so the count check can't
    // distinguish them.
    store.publish(&data(1, 1), b"g1", 0).unwrap();
    store.publish(&data(1, 1), b"g2", 0).unwrap();
    drop(store);

    // Point the (gen2) manifest's segment_dir at gen1 while it still claims
    // segment_id 2 / state_generation 2 -> a mixed pair the segment-id check
    // must reject.
    let e = tamper_manifest_and_reopen(
        tmp.path(),
        "segments/segment-000002",
        "segments/segment-000001",
    );
    assert!(matches!(e, StoreError::PairMismatch { .. }), "got {e:?}");
}

#[test]
fn manifest_traversal_segment_dir_rejected() {
    let tmp = tempfile::tempdir().unwrap();
    let store = Store::open(tmp.path()).unwrap();
    store.publish(&data(1, 1), b"g1", 0).unwrap();
    drop(store);

    // A non-canonical / traversal segment_dir must be refused before any open.
    let e =
        tamper_manifest_and_reopen(tmp.path(), "segments/segment-000001", "../../../etc/passwd");
    assert!(matches!(e, StoreError::ManifestCorrupt { .. }), "got {e:?}");
}
