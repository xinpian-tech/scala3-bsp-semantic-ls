//! Port of the Scala `LocatorSuite`.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use ls_semanticdb::SemanticdbLocator;

/// A self-cleaning temp directory (no external tempdir dependency).
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new() -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("ls-sdb-locator-{}-{}", std::process::id(), n));
        fs::create_dir_all(&path).unwrap();
        TempDir { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn touch(p: &Path, byte: u8) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, [byte]).unwrap();
}

#[test]
fn lists_semanticdb_files_recursively_and_sorted() {
    let tmp = TempDir::new();
    let root = tmp.path();
    let sdb_root = root.join("META-INF/semanticdb");
    let f1 = sdb_root.join("src/main/scala/a/B.scala.semanticdb");
    let f2 = sdb_root.join("src/test/T.scala.semanticdb");
    let junk = sdb_root.join("src/test/notes.txt");
    touch(&f1, 1);
    touch(&f2, 2);
    touch(&junk, 3);

    let locator = SemanticdbLocator::new(root);
    let mut expected = vec![f1, f2];
    expected.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
    assert_eq!(locator.list_semanticdb_files(), expected);
}

#[test]
fn returns_empty_when_semanticdb_root_missing() {
    let tmp = TempDir::new();
    assert!(SemanticdbLocator::new(tmp.path())
        .list_semanticdb_files()
        .is_empty());
}

#[test]
fn maps_source_relative_path_to_semanticdb_file_and_back() {
    let tmp = TempDir::new();
    let root = tmp.path();
    let locator = SemanticdbLocator::new(root);
    let rel = "src/main/scala/a/B.scala";
    let expected = root.join("META-INF/semanticdb/src/main/scala/a/B.scala.semanticdb");
    assert_eq!(locator.semanticdb_file_for(rel).unwrap(), expected);
    assert_eq!(
        locator.source_relative_path_for(&expected).as_deref(),
        Some(rel)
    );
}

#[test]
fn round_trips_every_listed_file() {
    let tmp = TempDir::new();
    let root = tmp.path();
    let f = root.join("META-INF/semanticdb/x/y/Z.scala.semanticdb");
    touch(&f, 0);
    let locator = SemanticdbLocator::new(root);
    for file in locator.list_semanticdb_files() {
        let rel = locator.source_relative_path_for(&file);
        assert_eq!(
            rel.map(|r| locator.semanticdb_file_for(&r).unwrap()),
            Some(file.clone())
        );
    }
}

#[test]
fn rejects_files_outside_root_or_without_suffix_and_bad_source_paths() {
    let tmp = TempDir::new();
    let root = tmp.path();
    let locator = SemanticdbLocator::new(root);
    assert_eq!(
        locator.source_relative_path_for(&root.join("elsewhere/A.scala.semanticdb")),
        None
    );
    assert_eq!(
        locator.source_relative_path_for(&root.join("META-INF/semanticdb/A.scala")),
        None
    );
    assert!(locator.semanticdb_file_for("/absolute/A.scala").is_err());
    assert!(locator.semanticdb_file_for("../../escape.scala").is_err());
}
