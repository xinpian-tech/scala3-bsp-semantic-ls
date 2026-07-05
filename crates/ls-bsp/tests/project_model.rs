//! Pure graph-op tests over a hand-built model (diamond + dangling deps) —
//! port of the Scala `BspProjectModelTest`.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use ls_bsp::{BspProjectModel, BspTarget};

fn target(id: &str, deps: &[&str]) -> BspTarget {
    BspTarget {
        bsp_id: id.to_string(),
        display_name: id.to_string(),
        scala_version: "3.8.4".to_string(),
        scalac_options: vec!["-Xsemanticdb".to_string()],
        class_directory: PathBuf::from(format!("/out/{id}/classes")),
        semanticdb_root: Some(PathBuf::from(format!("/out/{id}/classes"))),
        sourceroot: Some(PathBuf::from("/ws")),
        sources: vec![PathBuf::from(format!("/ws/{id}/src/Main.scala"))],
        direct_deps: deps.iter().map(|s| s.to_string()).collect(),
    }
}

fn set(items: &[&str]) -> BTreeSet<String> {
    items.iter().map(|s| s.to_string()).collect()
}

// diamond: base <- left, base <- right, left <- top, right <- top
fn diamond() -> BspProjectModel {
    BspProjectModel::new(
        vec![
            target("base", &[]),
            target("left", &["base"]),
            target("right", &["base", "external-dep"]),
            target("top", &["left", "right"]),
        ],
        HashMap::from([(
            "file:///ws/base/src/Main.scala".to_string(),
            "base".to_string(),
        )]),
    )
}

#[test]
fn reverse_dependency_closure_walks_diamond_once() {
    let d = diamond();
    assert_eq!(
        d.reverse_dependency_closure("base"),
        set(&["base", "left", "right", "top"])
    );
    assert_eq!(d.reverse_dependency_closure("left"), set(&["left", "top"]));
    assert_eq!(d.reverse_dependency_closure("top"), set(&["top"]));
}

#[test]
fn unknown_ids_yield_empty() {
    let d = diamond();
    assert_eq!(d.reverse_dependency_closure("nope"), BTreeSet::new());
    assert!(d.dependencies_of("nope").is_empty());
    assert!(d.dependents_of("nope").is_empty());
    assert_eq!(d.target_for("nope"), None);
}

#[test]
fn dangling_deps_to_filtered_targets_ignored() {
    let d = diamond();
    assert_eq!(d.dependencies_of("right"), vec!["base".to_string()]);
    assert!(d.dependents_of("external-dep").is_empty());
    assert_eq!(
        d.reverse_dependency_closure("external-dep"),
        BTreeSet::new()
    );
}

#[test]
fn dependents_of_is_sorted_and_deduplicated() {
    let d = diamond();
    assert_eq!(
        d.dependents_of("base"),
        vec!["left".to_string(), "right".to_string()]
    );
    assert_eq!(
        d.dependencies_of("top"),
        vec!["left".to_string(), "right".to_string()]
    );
}

#[test]
fn target_of_uri_resolves_through_map() {
    let d = diamond();
    assert_eq!(
        d.target_of_uri("file:///ws/base/src/Main.scala")
            .map(|t| t.bsp_id.as_str()),
        Some("base")
    );
    assert_eq!(d.target_of_uri("file:///elsewhere.scala"), None);
}

#[test]
fn indexable_partitioning() {
    let mut no_sdb = target("no-sdb", &[]);
    no_sdb.semanticdb_root = None;
    no_sdb.scalac_options = Vec::new();
    let model = BspProjectModel::new(vec![target("ok", &[]), no_sdb], HashMap::new());
    assert_eq!(
        model
            .indexable_targets()
            .iter()
            .map(|t| t.bsp_id.as_str())
            .collect::<Vec<_>>(),
        vec!["ok"]
    );
    assert_eq!(
        model
            .unavailable_targets()
            .iter()
            .map(|t| t.bsp_id.as_str())
            .collect::<Vec<_>>(),
        vec!["no-sdb"]
    );
    assert_eq!(model.unavailable_errors().len(), 1);
}
