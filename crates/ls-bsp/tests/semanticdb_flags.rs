//! SemanticDB flag extraction — port of the Scala `SemanticdbFlagsTest`.

use std::path::{Path, PathBuf};

use ls_bsp::SemanticdbFlags;

fn opts(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| s.to_string()).collect()
}

fn ws() -> PathBuf {
    PathBuf::from("/workspace")
}

fn class_dir() -> PathBuf {
    PathBuf::from("/workspace/out/classes")
}

#[test]
fn no_flags_disabled_sourceroot_defaults() {
    let cfg = SemanticdbFlags::extract(&opts(&["-deprecation", "-feature"]), &class_dir(), &ws());
    assert!(!cfg.enabled());
    assert_eq!(cfg.semanticdb_root, None);
    assert_eq!(cfg.sourceroot, ws());
}

#[test]
fn xsemanticdb_targetroot_is_class_directory() {
    let cfg = SemanticdbFlags::extract(&opts(&["-Xsemanticdb"]), &class_dir(), &ws());
    assert!(cfg.enabled());
    assert_eq!(cfg.semanticdb_root, Some(class_dir()));
    assert_eq!(cfg.sourceroot, ws());
}

#[test]
fn ysemanticdb_also_enables() {
    let cfg = SemanticdbFlags::extract(&opts(&["-Ysemanticdb"]), &class_dir(), &ws());
    assert_eq!(cfg.semanticdb_root, Some(class_dir()));
}

#[test]
fn colon_form_target_overrides() {
    let cfg = SemanticdbFlags::extract(
        &opts(&["-Xsemanticdb", "-semanticdb-target:/workspace/out/meta"]),
        &class_dir(),
        &ws(),
    );
    assert_eq!(
        cfg.semanticdb_root,
        Some(PathBuf::from("/workspace/out/meta"))
    );
}

#[test]
fn two_token_form_target_overrides() {
    let cfg = SemanticdbFlags::extract(
        &opts(&["-Xsemanticdb", "-semanticdb-target", "/workspace/out/meta2"]),
        &class_dir(),
        &ws(),
    );
    assert_eq!(
        cfg.semanticdb_root,
        Some(PathBuf::from("/workspace/out/meta2"))
    );
}

#[test]
fn target_without_enable_does_not_enable() {
    let cfg = SemanticdbFlags::extract(&opts(&["-semanticdb-target:/x"]), &class_dir(), &ws());
    assert!(!cfg.enabled());
    assert_eq!(cfg.semanticdb_root, None);
}

#[test]
fn sourceroot_colon_and_two_token() {
    let colon = SemanticdbFlags::extract(
        &opts(&["-Xsemanticdb", "-sourceroot:/repo/src"]),
        &class_dir(),
        &ws(),
    );
    assert_eq!(colon.sourceroot, Path::new("/repo/src"));
    let two_token = SemanticdbFlags::extract(
        &opts(&["-Xsemanticdb", "-sourceroot", "/repo/src2"]),
        &class_dir(),
        &ws(),
    );
    assert_eq!(two_token.sourceroot, Path::new("/repo/src2"));
}

#[test]
fn last_occurrence_wins_across_spellings() {
    let cfg = SemanticdbFlags::extract(
        &opts(&[
            "-Xsemanticdb",
            "-semanticdb-target:/first",
            "-semanticdb-target",
            "/second",
            "-sourceroot",
            "/src-first",
            "-sourceroot:/src-second",
        ]),
        &class_dir(),
        &ws(),
    );
    assert_eq!(cfg.semanticdb_root, Some(PathBuf::from("/second")));
    assert_eq!(cfg.sourceroot, Path::new("/src-second"));
}

#[test]
fn relative_paths_resolve_and_normalize() {
    let cfg = SemanticdbFlags::extract(
        &opts(&[
            "-Xsemanticdb",
            "-semanticdb-target:out/../meta",
            "-sourceroot:sub/dir",
        ]),
        &class_dir(),
        &ws(),
    );
    assert_eq!(cfg.semanticdb_root, Some(PathBuf::from("/workspace/meta")));
    assert_eq!(cfg.sourceroot, Path::new("/workspace/sub/dir"));
}

#[test]
fn trailing_two_token_flag_without_value_ignored() {
    let cfg = SemanticdbFlags::extract(
        &opts(&["-Xsemanticdb", "-semanticdb-target"]),
        &class_dir(),
        &ws(),
    );
    assert_eq!(cfg.semanticdb_root, Some(class_dir()));
}

#[test]
fn empty_colon_form_value_ignored() {
    let cfg = SemanticdbFlags::extract(
        &opts(&["-Xsemanticdb", "-semanticdb-target:"]),
        &class_dir(),
        &ws(),
    );
    assert_eq!(cfg.semanticdb_root, Some(class_dir()));
}
