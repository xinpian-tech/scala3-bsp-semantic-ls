//! On-disk index-store inspection — its committed `manifest.json`, the active
//! segment's header, and the paired workspace-state. Shared by the `ls dump`
//! subcommand (a full facts dump) and the doctor `Store` section. Opening goes
//! through `ls_store::Store::open_readonly`, which recovers the active generation
//! without creating the root or touching tmp debris, so inspection is strictly
//! read-only: it runs pre-bootstrap with no build server, no presentation
//! compiler, and no embedded JVM, and never disturbs a live server that owns the
//! same store. Replaces the `sqlite3` ad-hoc inspection of the SQLite era.

use std::path::Path;

use ls_store::{Manifest, Snapshot, Store};

/// The store facts as individual lines (no header or indent), shared by `ls dump`
/// and the doctor `Store` section. A committed generation yields the manifest,
/// active-segment, and workspace-state lines; a store with no committed
/// generation yields a single "manifest: none …" line; a store that fails to
/// open yields a single "error: …" line rather than panicking. The read is
/// strictly read-only (`Store::open_readonly`) — it neither creates the store nor
/// removes tmp debris, so it never interferes with a live server's publish.
fn store_fact_lines(root: &Path) -> Vec<String> {
    match Store::open_readonly(root) {
        Ok(store) => match store.current() {
            Some(snapshot) => active_lines(root, &snapshot),
            None => vec!["manifest: none (no committed generation)".to_string()],
        },
        Err(error) => vec![format!("error: {error}")],
    }
}

/// The manifest, active-segment header, and workspace-state fact lines of an
/// opened generation.
fn active_lines(root: &Path, snapshot: &Snapshot) -> Vec<String> {
    let manifest_line = match Manifest::load(root) {
        Ok(Some(m)) => format!(
            "manifest: schema v{}, segment {} (dir {}), state generation {} (checksum {:#010x}), docs {}, symbols {}",
            m.schema_version,
            m.segment_id,
            m.segment_dir,
            m.state_generation,
            m.state_checksum,
            m.doc_count,
            m.symbol_count,
        ),
        // The snapshot was recovered from the manifest, so this is unreachable in
        // practice; report it rather than unwrap so the caller never panics.
        _ => "manifest: <present but unreadable>".to_string(),
    };
    let segment = snapshot.segment();
    let segment_line = format!(
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
    let state_line = format!(
        "workspace-state: generation {}, {} bytes",
        state.generation,
        state.payload.len(),
    );
    vec![manifest_line, segment_line, state_line]
}

/// `ls dump`: the store facts under a `store: <root>` header. Reads the store at
/// `root` strictly read-only (`Store::open_readonly`) — it never creates the root
/// or removes tmp debris, so dumping a store a live server owns is safe.
pub fn dump_report(root: &Path) -> String {
    let mut out = format!("store: {}\n", root.display());
    for line in store_fact_lines(root) {
        out.push_str(&line);
        out.push('\n');
    }
    out
}

/// The typed store facts for the doctor `Store` section: a `(status, facts)`
/// pair. `status` names the store state (`no workspace root` / `no store` /
/// `empty` / `error` / `active`); `facts` are the human-readable fact lines. The
/// read is strictly read-only (`Store::open_readonly`), so it boots no JVM and
/// never disturbs a live server that owns the same store.
pub fn store_facts(workspace_root: Option<&Path>) -> (String, Vec<String>) {
    match workspace_root {
        None => (
            "no workspace root".to_string(),
            vec!["workspace root not set".to_string()],
        ),
        Some(root) => {
            let store_dir = root.join(crate::bootstrap::STORE_DIR);
            if !store_dir.exists() {
                return (
                    "no store".to_string(),
                    vec!["no store at this workspace root".to_string()],
                );
            }
            let facts = store_fact_lines(&store_dir);
            let status = match facts.first() {
                Some(line) if line.starts_with("error:") => "error",
                Some(line) if line.starts_with("manifest: none") => "empty",
                _ => "active",
            };
            (status.to_string(), facts)
        }
    }
}

/// The doctor `Store` section: the store facts indented under a `Store:` heading
/// (the retained `Doctor.section` layout). The store lives at
/// `<workspace_root>/.scala3-bsp-semantic-ls`; a workspace root that was never
/// set yields a "workspace root not set" line, and a root whose store directory
/// does not exist yields a "no store at this workspace root" line without opening
/// anything — so the doctor stays JVM-free and never materializes a store where
/// none exists.
pub fn store_section(workspace_root: Option<&Path>) -> String {
    let lines = match workspace_root {
        None => vec!["workspace root not set".to_string()],
        Some(root) => {
            let store_dir = root.join(crate::bootstrap::STORE_DIR);
            if store_dir.exists() {
                store_fact_lines(&store_dir)
            } else {
                vec!["no store at this workspace root".to_string()]
            }
        }
    };
    let mut out = String::from("Store:\n");
    for line in lines {
        out.push_str("  ");
        out.push_str(&line);
        out.push('\n');
    }
    out
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

    #[test]
    fn reading_a_store_is_strictly_read_only_and_preserves_tmp_debris() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        store.publish(&data(1, 1), b"g1", 0).unwrap();
        drop(store);
        // A live server's in-flight publish leaves these tmp artifacts; the
        // read-only dump must not delete them (`Store::open` would).
        let state_tmp = dir.path().join("workspace-state-99.bin.tmp");
        std::fs::write(&state_tmp, b"in-flight").unwrap();
        let seg_tmp = dir.path().join("tmp-inflight");
        std::fs::create_dir_all(&seg_tmp).unwrap();

        let report = dump_report(dir.path());
        assert!(report.contains("segment: id 1"), "{report}");
        assert!(state_tmp.exists(), "read deleted a state tmp file");
        assert!(seg_tmp.exists(), "read deleted a tmp segment dir");
    }

    #[test]
    fn store_section_without_a_workspace_root_says_so() {
        let section = store_section(None);
        assert!(section.starts_with("Store:\n"), "{section}");
        assert!(section.contains("workspace root not set"), "{section}");
    }

    #[test]
    fn store_section_of_a_workspace_without_a_store_does_not_materialize_one() {
        let ws = tempfile::tempdir().unwrap();
        let section = store_section(Some(ws.path()));
        assert!(section.starts_with("Store:\n"), "{section}");
        assert!(
            section.contains("no store at this workspace root"),
            "{section}"
        );
        // The doctor read must not create the store directory where none exists.
        assert!(!ws.path().join(crate::bootstrap::STORE_DIR).exists());
    }

    #[test]
    fn store_section_renders_facts_for_a_populated_store() {
        let ws = tempfile::tempdir().unwrap();
        let store_dir = ws.path().join(crate::bootstrap::STORE_DIR);
        let store = Store::open(&store_dir).unwrap();
        store.publish(&data(3, 2), b"state-gen-1", 0).unwrap();
        drop(store);

        let section = store_section(Some(ws.path()));
        assert!(section.starts_with("Store:\n"), "{section}");
        assert!(section.contains("  manifest: schema v"), "{section}");
        assert!(section.contains("  segment: id 1"), "{section}");
        assert!(
            section.contains("  workspace-state: generation 1, 11 bytes"),
            "{section}"
        );
    }
}
