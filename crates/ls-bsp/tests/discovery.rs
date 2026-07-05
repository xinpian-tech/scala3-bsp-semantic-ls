//! `.bsp/*.json` discovery — port of the Scala `BspDiscoveryTest`.

use std::path::Path;

use ls_bsp::{BspDiscovery, BspDiscoveryResult, BspError};

fn connection_json(name: &str, argv: &[&str]) -> String {
    let argv_json = argv
        .iter()
        .map(|a| format!("\"{a}\""))
        .collect::<Vec<_>>()
        .join(",");
    format!(
        "{{\n  \"name\": \"{name}\",\n  \"argv\": [{argv_json}],\n  \"version\": \"1.4.2\",\n  \"bspVersion\": \"2.1.1\",\n  \"languages\": [\"scala\"]\n}}"
    )
}

fn write(dir: &Path, name: &str, content: &str) {
    std::fs::write(dir.join(name), content).unwrap();
}

#[test]
fn discovers_sorted_by_server_name() {
    let ws = tempfile::tempdir().unwrap();
    let bsp = ws.path().join(".bsp");
    std::fs::create_dir_all(&bsp).unwrap();
    // File names sort the other way around from server names on purpose.
    write(
        &bsp,
        "a-file.json",
        &connection_json("zeta-build", &["zeta", "bsp"]),
    );
    write(
        &bsp,
        "z-file.json",
        &connection_json("alpha-build", &["alpha", "bsp", "--stdio"]),
    );
    write(&bsp, "notes.txt", "not a connection file");

    let result = BspDiscovery::discover(ws.path());
    assert!(result.invalid.is_empty());
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|c| c.details.name.as_str())
            .collect::<Vec<_>>(),
        vec!["alpha-build", "zeta-build"]
    );
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|c| c.path.file_name().unwrap().to_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["z-file.json", "a-file.json"]
    );

    let picked = BspDiscovery::pick(ws.path()).unwrap();
    assert_eq!(picked.details.name, "alpha-build");
    assert_eq!(picked.details.argv, vec!["alpha", "bsp", "--stdio"]);
    assert_eq!(picked.details.bsp_version, "2.1.1");
    assert_eq!(
        BspDiscovery::required(ws.path()).unwrap().details.name,
        "alpha-build"
    );
}

#[test]
fn malformed_and_incomplete_reported_valid_still_wins() {
    let ws = tempfile::tempdir().unwrap();
    let bsp = ws.path().join(".bsp");
    std::fs::create_dir_all(&bsp).unwrap();
    write(&bsp, "good.json", &connection_json("good-build", &["good"]));
    write(&bsp, "broken.json", "{ this is not json");
    write(&bsp, "no-argv.json", r#"{"name":"no-argv","argv":[]}"#);
    write(&bsp, "empty.json", "");

    let result = BspDiscovery::discover(ws.path());
    assert_eq!(
        result
            .candidates
            .iter()
            .map(|c| c.details.name.as_str())
            .collect::<Vec<_>>(),
        vec!["good-build"]
    );
    assert_eq!(result.invalid.len(), 3);
    let invalid_files: std::collections::BTreeSet<String> = result
        .invalid
        .iter()
        .map(|e| match e {
            BspError::InvalidConnectionFile { path, .. } => Path::new(path)
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_string(),
            other => panic!("expected InvalidConnectionFile, got {other:?}"),
        })
        .collect();
    assert_eq!(
        invalid_files,
        ["broken.json", "empty.json", "no-argv.json"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    );
    assert!(result
        .invalid
        .iter()
        .all(|e| e.message().starts_with("invalid BSP connection file")));
    assert_eq!(
        result.preferred().map(|c| c.details.name.as_str()),
        Some("good-build")
    );
}

#[test]
fn workspace_without_bsp_directory() {
    let ws = tempfile::tempdir().unwrap();
    assert_eq!(
        BspDiscovery::discover(ws.path()),
        BspDiscoveryResult {
            candidates: Vec::new(),
            invalid: Vec::new(),
        }
    );
    assert_eq!(BspDiscovery::pick(ws.path()), None);
    let err = BspDiscovery::required(ws.path()).unwrap_err();
    match err {
        BspError::NoConnectionFile { workspace_root } => {
            assert_eq!(workspace_root, ws.path().display().to_string());
        }
        other => panic!("expected NoConnectionFile, got {other:?}"),
    }
}
