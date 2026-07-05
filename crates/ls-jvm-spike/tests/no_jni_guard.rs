//! Repository hygiene guard: the M0 boundary must stay FFM-only.
//!
//! Fails if the spike ever grows a `jni` crate dependency, includes a JNI C
//! header, or references a `JNIEnv` — i.e. anything beyond the single
//! `JNI_CreateJavaVM` boot symbol. Comments (which legitimately say "no
//! JNIEnv") are stripped before scanning, so only real code is checked.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <root>/crates/ls-jvm-spike
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("repo root")
        .to_path_buf()
}

/// Drop `//`-style line comments (including `//!`/`///`), block-comment lines
/// (`/* … */`, and continuation lines starting with `*`), so only code remains.
fn strip_comments(src: &str) -> String {
    src.lines()
        .map(|line| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("//") || trimmed.starts_with('*') || trimmed.starts_with("/*") {
                return String::new();
            }
            match line.split_once("//") {
                Some((code, _)) => code.to_string(),
                None => line.to_string(),
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn collect(dir: &Path, exts: &[&str], out: &mut Vec<PathBuf>) {
    if !dir.exists() {
        return;
    }
    for entry in std::fs::read_dir(dir).expect("read_dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect(&path, exts, out);
        } else if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
            if exts.contains(&ext) {
                out.push(path);
            }
        }
    }
}

#[test]
fn no_jni_crate_dependency() {
    // Authoritative: the workspace lockfile must not carry a `jni` crate.
    let lock = std::fs::read_to_string(repo_root().join("Cargo.lock")).expect("Cargo.lock");
    assert!(
        !lock.lines().any(|l| l.trim() == "name = \"jni\""),
        "the `jni` crate must not appear in Cargo.lock"
    );

    // And the spike crate's [dependencies] table must not name `jni`.
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"))
            .expect("Cargo.toml");
    let mut in_deps = false;
    for line in manifest.lines() {
        let t = line.trim();
        if t.starts_with('[') {
            in_deps = t == "[dependencies]";
            continue;
        }
        if in_deps {
            let name = t.split(['=', ' ']).next().unwrap_or("").trim();
            assert_ne!(name, "jni", "the `jni` crate must not be a dependency");
        }
    }
}

#[test]
fn no_jnienv_or_jni_header_in_code() {
    let mut files = Vec::new();
    collect(
        Path::new(env!("CARGO_MANIFEST_DIR")),
        &["rs", "h"],
        &mut files,
    );
    // The Scala island lives outside the crate; scan it when present (it is
    // absent in the crane fileset, which only vendors crates/).
    collect(
        &repo_root().join("modules/ls-pc-host-spike"),
        &["scala", "java"],
        &mut files,
    );

    let forbidden = ["JNIEnv", "jni.h", "jni::", "extern crate jni"];
    for file in files {
        // This guard necessarily names the forbidden tokens in its own code.
        if file.file_name().and_then(|n| n.to_str()) == Some("no_jni_guard.rs") {
            continue;
        }
        let code = strip_comments(&std::fs::read_to_string(&file).expect("read source"));
        for needle in forbidden {
            assert!(
                !code.contains(needle),
                "forbidden JNI token {needle:?} found in {}",
                file.display()
            );
        }
    }
}
