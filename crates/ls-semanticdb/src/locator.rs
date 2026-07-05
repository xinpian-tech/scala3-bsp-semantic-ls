//! Locates `.semanticdb` files under one targetroot and maps between
//! source-relative paths (SemanticDB `TextDocument.uri` convention: relative to
//! the sourceroot, forward slashes) and their `.semanticdb` files.
//!
//! scalac layout: `<targetroot>/META-INF/semanticdb/<source-rel-path>.semanticdb`.
//! A verbatim port of the Scala `SemanticdbLocator`, using std filesystem APIs.

use std::path::{Component, Path, PathBuf};

use crate::error::{SemanticdbError, SemanticdbResult};

/// The `.semanticdb` file suffix.
pub const SEMANTICDB_SUFFIX: &str = ".semanticdb";

pub struct SemanticdbLocator {
    targetroot: PathBuf,
    semanticdb_root: PathBuf,
}

impl SemanticdbLocator {
    /// A locator for `.semanticdb` output under `targetroot`.
    pub fn new(targetroot: impl Into<PathBuf>) -> Self {
        let targetroot = targetroot.into();
        let semanticdb_root = targetroot.join("META-INF").join("semanticdb");
        SemanticdbLocator {
            targetroot,
            semanticdb_root,
        }
    }

    pub fn targetroot(&self) -> &Path {
        &self.targetroot
    }

    pub fn semanticdb_root(&self) -> &Path {
        &self.semanticdb_root
    }

    /// All `*.semanticdb` files under the targetroot, sorted (by path string) for
    /// determinism. Empty when the root does not exist.
    pub fn list_semanticdb_files(&self) -> Vec<PathBuf> {
        if !self.semanticdb_root.is_dir() {
            return Vec::new();
        }
        let mut out = Vec::new();
        walk(&self.semanticdb_root, &mut out);
        out.retain(|p| {
            p.is_file()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(SEMANTICDB_SUFFIX))
        });
        out.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
        out
    }

    /// Expected `.semanticdb` file for a source-relative path such as
    /// `src/main/scala/a/B.scala`. Rejects empty, absolute, or `..`-escaping
    /// paths with a typed error.
    pub fn semanticdb_file_for(&self, source_relative_path: &str) -> SemanticdbResult<PathBuf> {
        if source_relative_path.is_empty() || source_relative_path.starts_with('/') {
            return Err(SemanticdbError::InvalidPath(format!(
                "source path must be relative: {source_relative_path}"
            )));
        }
        let resolved = lexical_normalize(
            &self
                .semanticdb_root
                .join(format!("{source_relative_path}{SEMANTICDB_SUFFIX}")),
        );
        let root = lexical_normalize(&self.semanticdb_root);
        if !resolved.starts_with(&root) {
            return Err(SemanticdbError::InvalidPath(format!(
                "source path escapes the semanticdb root: {source_relative_path}"
            )));
        }
        Ok(resolved)
    }

    /// Inverse mapping: source-relative path (forward slashes) for a
    /// `.semanticdb` file, or `None` when the file is not under this targetroot
    /// or lacks the suffix.
    pub fn source_relative_path_for(&self, semanticdb_file: &Path) -> Option<String> {
        let abs = lexical_normalize(&absolutize(semanticdb_file));
        let root = lexical_normalize(&absolutize(&self.semanticdb_root));
        if !abs.starts_with(&root) || abs == root {
            return None;
        }
        let rel = abs.strip_prefix(&root).ok()?;
        let rel_str = rel
            .to_string_lossy()
            .replace(std::path::MAIN_SEPARATOR, "/");
        if rel_str.ends_with(SEMANTICDB_SUFFIX) && rel_str.len() > SEMANTICDB_SUFFIX.len() {
            Some(rel_str[..rel_str.len() - SEMANTICDB_SUFFIX.len()].to_string())
        } else {
            None
        }
    }
}

/// Recursively collect every entry under `dir` (files only; directories are
/// descended into).
fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Absolute form of `p` (joined against the current dir when relative), matching
/// Java's `toAbsolutePath` without resolving symlinks.
fn absolutize(p: &Path) -> PathBuf {
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(p)
    }
}

/// Purely lexical path normalization (Java `Path.normalize`): collapse `.`,
/// resolve `..` against a preceding normal component, and keep a leading `..`.
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                let pop = matches!(out.components().next_back(), Some(Component::Normal(_)));
                if pop {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}
