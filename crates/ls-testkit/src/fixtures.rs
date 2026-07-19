//! The committed `ls-engine` SemanticDB fixture-corpus geometry — targets
//! `fixture-a`/`fixture-b`/`fixture-c` over the `out-{a,b,c}` targetroots plus
//! the SemanticDB-less `fixture-nosdb` — shared by the fake BSP server and the
//! suites that assert over the corpus.

use std::path::PathBuf;

use serde_json::{json, Value};

use ls_bsp::uri::path_to_uri;

/// The committed fixture corpus root (`crates/ls-engine/tests/fixtures`),
/// canonicalized so URIs derived from it survive the server's URI
/// normalization unchanged (handle-vs-wire key equality in suites).
pub fn fixtures_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../ls-engine/tests/fixtures")
        .canonicalize()
        .expect("canonicalize fixtures root")
}

/// The corpus source tree the targets' `-sourceroot` points at.
pub fn sources_root() -> PathBuf {
    fixtures_root().join("sources")
}

pub fn target_id(name: &str) -> String {
    format!("bsp://workspace/{name}")
}

/// A `file://` URI for a corpus source, workspace-relative to `sources/`.
pub fn source_uri(rel: &str) -> String {
    path_to_uri(&sources_root().join(rel))
}

/// The corpus anchor file most suites query (`class Core` at line 2, col 6).
pub fn core_uri() -> String {
    source_uri("a/src/pkga/Core.scala")
}

/// One BSP `BuildTarget` JSON for a corpus target.
pub fn build_target(name: &str, deps: &[&str]) -> Value {
    json!({
        "id": { "uri": target_id(name) },
        "displayName": name,
        "tags": [],
        "languageIds": ["scala"],
        "dependencies": deps.iter().map(|d| json!({ "uri": target_id(d) })).collect::<Vec<_>>(),
        "capabilities": { "canCompile": true },
        "dataKind": "scala",
        "data": {
            "scalaOrganization": "org.scala-lang",
            "scalaVersion": "3.3.1",
            "scalaBinaryVersion": "3",
            "platform": 1,
            "jars": [],
        },
    })
}

/// Recursively copy a committed corpus subtree (e.g. the `out-a` targetroot)
/// to `dest`, for suites that must WRITE into a targetroot (watched-files
/// reingest) while the committed corpus stays read-only.
pub fn copy_corpus_dir(name: &str, dest: &std::path::Path) {
    copy_dir(&fixtures_root().join(name), dest);
}

fn copy_dir(src: &std::path::Path, dest: &std::path::Path) {
    std::fs::create_dir_all(dest).expect("create corpus copy dir");
    for entry in std::fs::read_dir(src).expect("read corpus dir") {
        let entry = entry.expect("corpus dir entry");
        let target = dest.join(entry.file_name());
        if entry.file_type().expect("corpus entry type").is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).expect("copy corpus file");
        }
    }
}

/// The full advertised corpus: three indexable targets plus the SemanticDB-less one.
pub fn default_targets() -> Vec<Value> {
    vec![
        build_target("fixture-a", &[]),
        build_target("fixture-b", &["fixture-a"]),
        build_target("fixture-c", &[]),
        build_target("fixture-nosdb", &[]),
    ]
}
