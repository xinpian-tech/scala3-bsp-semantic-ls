//! Scala 3 SemanticDB flag extraction.
//!
//!   - `-Xsemanticdb` or `-Ysemanticdb` enables SemanticDB generation.
//!   - `-semanticdb-target:<path>` (or the two-token form
//!     `-semanticdb-target <path>`) overrides the targetroot; otherwise the
//!     targetroot is the class directory.
//!   - `-sourceroot:<path>` (or two-token form) sets the sourceroot; otherwise
//!     the workspace root is the sourceroot.
//!
//! Like scalac, the last occurrence of a flag wins and relative paths are
//! resolved against the workspace root and lexically normalized.

use std::path::{Component, Path, PathBuf};

/// SemanticDB output configuration derived from a target's scalac options.
/// `semanticdb_root` is the targetroot that will contain `META-INF/semanticdb`
/// output; None when the target does not generate SemanticDB at all.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SemanticdbConfig {
    pub semanticdb_root: Option<PathBuf>,
    pub sourceroot: PathBuf,
}

impl SemanticdbConfig {
    pub fn enabled(&self) -> bool {
        self.semanticdb_root.is_some()
    }
}

const ENABLE_FLAGS: [&str; 2] = ["-Xsemanticdb", "-Ysemanticdb"];
const TARGET_FLAG: &str = "-semanticdb-target";
const SOURCEROOT_FLAG: &str = "-sourceroot";

pub struct SemanticdbFlags;

impl SemanticdbFlags {
    pub fn extract(
        options: &[String],
        class_directory: &Path,
        workspace_root: &Path,
    ) -> SemanticdbConfig {
        let enabled = options.iter().any(|o| ENABLE_FLAGS.contains(&o.as_str()));
        let resolve = |value: &str| lexical_normalize(&workspace_root.join(value));
        let targetroot = if enabled {
            Some(
                last_value(options, TARGET_FLAG)
                    .map(|v| resolve(&v))
                    .unwrap_or_else(|| class_directory.to_path_buf()),
            )
        } else {
            None
        };
        let sourceroot = last_value(options, SOURCEROOT_FLAG)
            .map(|v| resolve(&v))
            .unwrap_or_else(|| workspace_root.to_path_buf());
        SemanticdbConfig {
            semanticdb_root: targetroot,
            sourceroot,
        }
    }
}

/// Last-wins scan over both `-flag:value` and `-flag value` spellings.
fn last_value(options: &[String], flag: &str) -> Option<String> {
    let colon_prefix = format!("{flag}:");
    let mut result: Option<String> = None;
    let mut i = 0;
    while i < options.len() {
        let opt = &options[i];
        if let Some(value) = opt.strip_prefix(&colon_prefix) {
            if !value.is_empty() {
                result = Some(value.to_string());
            }
        } else if opt == flag && i + 1 < options.len() {
            // Two-token form consumes the next argument, mirroring scalac.
            result = Some(options[i + 1].clone());
            i += 1;
        }
        i += 1;
    }
    result
}

/// Lexical `..`/`.` normalization, matching `java.nio.file.Path.normalize`.
fn lexical_normalize(p: &Path) -> PathBuf {
    let mut out: Vec<Component> = Vec::new();
    for comp in p.components() {
        match comp {
            Component::CurDir => {}
            Component::ParentDir => {
                if matches!(out.last(), Some(Component::Normal(_))) {
                    out.pop();
                } else {
                    out.push(comp);
                }
            }
            other => out.push(other),
        }
    }
    out.iter().collect()
}
