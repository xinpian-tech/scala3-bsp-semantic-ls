//! `ls dump`: an offline, ad-hoc dump of the on-disk index store — its committed
//! `manifest.json`, the active segment's header, and the paired workspace-state.
//! It reads the store exactly the way boot recovery does (`Store::open`), so it
//! runs pre-bootstrap with no build server, no presentation compiler, and no
//! embedded JVM in the process. Replaces the `sqlite3` ad-hoc inspection of the
//! SQLite era.

use std::fmt::Write as _;
use std::path::Path;

use ls_store::{Manifest, Snapshot, Store};

/// Render the on-disk store at `root` as a human-readable facts dump. A fresh
/// root with no committed generation renders an explicit "no committed
/// generation" line rather than an error; a store that fails to open renders the
/// typed error rather than panicking. Writing to a `String` never fails, so the
/// formatting results are discarded.
pub fn dump_report(root: &Path) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "store: {}", root.display());
    match Store::open(root) {
        Ok(store) => match store.current() {
            Some(snapshot) => render_active(&mut out, root, &snapshot),
            None => {
                let _ = writeln!(out, "manifest: none (no committed generation)");
            }
        },
        Err(error) => {
            let _ = writeln!(out, "error: {error}");
        }
    }
    out
}

/// Render the manifest, active-segment header, and workspace-state facts of an
/// opened generation.
fn render_active(out: &mut String, root: &Path, snapshot: &Snapshot) {
    match Manifest::load(root) {
        Ok(Some(manifest)) => {
            let _ = writeln!(
                out,
                "manifest: schema v{}, segment {} (dir {}), state generation {} (checksum {:#010x}), docs {}, symbols {}",
                manifest.schema_version,
                manifest.segment_id,
                manifest.segment_dir,
                manifest.state_generation,
                manifest.state_checksum,
                manifest.doc_count,
                manifest.symbol_count,
            );
        }
        // The snapshot was recovered from the manifest, so this is unreachable in
        // practice; report it rather than unwrap so `ls dump` never panics.
        _ => {
            let _ = writeln!(out, "manifest: <present but unreadable>");
        }
    }
    let segment = snapshot.segment();
    let _ = writeln!(
        out,
        "segment: id {}, created-at-ms {}, docs {}, symbols {}, occurrences {}, ref-groups {}, rename-groups {}, targets {}, search-rows {}",
        segment.segment_id(),
        segment.created_at_ms(),
        segment.doc_count(),
        segment.symbol_count(),
        segment.occurrence_count(),
        segment.ref_group_count(),
        segment.rename_group_count(),
        segment.target_count(),
        segment.search_row_count(),
    );
    let state = snapshot.state();
    let _ = writeln!(
        out,
        "workspace-state: generation {}, {} bytes",
        state.generation,
        state.payload.len(),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ls_store::{SegmentData, SegmentDoc, SegmentSymbol, Store, TargetMeta};

    /// A minimal but valid segment: `n_docs` empty docs, one target, `n_syms`
    /// pre-sorted symbols, no occurrences (mirrors `ls-store`'s own `store.rs`
    /// test builder).
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

    #[test]
    fn dump_of_a_fresh_store_reports_no_committed_generation() {
        let dir = tempfile::tempdir().unwrap();
        let report = dump_report(dir.path());
        assert!(
            report.contains(&format!("store: {}", dir.path().display())),
            "{report}"
        );
        assert!(report.contains("no committed generation"), "{report}");
    }

    #[test]
    fn dump_of_a_published_generation_reports_manifest_state_and_segment_facts() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        // `state-gen-1` is 11 bytes; docs=3, symbols=2, generation=1.
        store.publish(&data(3, 2), b"state-gen-1", 0).unwrap();
        drop(store);

        let report = dump_report(dir.path());
        assert!(report.contains("manifest: schema v"), "{report}");
        assert!(report.contains("state generation 1"), "{report}");
        assert!(report.contains("docs 3, symbols 2"), "{report}");
        assert!(report.contains("segment: id 1"), "{report}");
        assert!(report.contains("occurrences 0"), "{report}");
        assert!(
            report.contains("workspace-state: generation 1, 11 bytes"),
            "{report}"
        );
    }
}
