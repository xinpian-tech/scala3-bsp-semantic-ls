//! `.bsp/<name>.json` connection-file discovery: never throws for per-file
//! problems (malformed or incomplete files are collected as invalid), and picks
//! deterministically by server name (ties broken by file name).

use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::errors::BspError;

/// A BSP connection file's payload (the standard `.bsp/*.json` schema).
#[derive(Clone, Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BspConnectionDetails {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub argv: Vec<String>,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub bsp_version: String,
    #[serde(default)]
    pub languages: Vec<String>,
}

/// One parsed `.bsp/<name>.json` connection file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BspConnectionFile {
    pub path: PathBuf,
    pub details: BspConnectionDetails,
}

/// All connection files found under `<workspace>/.bsp`: valid ones sorted
/// deterministically by server name (ties broken by file name), plus the files
/// that failed to parse or validate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BspDiscoveryResult {
    pub candidates: Vec<BspConnectionFile>,
    pub invalid: Vec<BspError>,
}

impl BspDiscoveryResult {
    /// Deterministic pick: first candidate in name order.
    pub fn preferred(&self) -> Option<&BspConnectionFile> {
        self.candidates.first()
    }
}

pub struct BspDiscovery;

impl BspDiscovery {
    pub fn bsp_dir(workspace_root: &Path) -> PathBuf {
        workspace_root.join(".bsp")
    }

    /// Finds and parses every `.bsp/<name>.json` connection file under the
    /// workspace root. Never fails for per-file problems: malformed or
    /// incomplete files are reported in `BspDiscoveryResult::invalid`.
    pub fn discover(workspace_root: &Path) -> BspDiscoveryResult {
        let dir = Self::bsp_dir(workspace_root);
        if !dir.is_dir() {
            return BspDiscoveryResult {
                candidates: Vec::new(),
                invalid: Vec::new(),
            };
        }

        let mut json_files: Vec<PathBuf> = match std::fs::read_dir(&dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| {
                    p.is_file()
                        && p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.ends_with(".json"))
                })
                .collect(),
            Err(_) => Vec::new(),
        };
        json_files.sort_by_key(|p| file_name_of(p));

        let mut candidates: Vec<BspConnectionFile> = Vec::new();
        let mut invalid: Vec<BspError> = Vec::new();
        for path in json_files {
            match parse_file(&path) {
                Ok(details) => candidates.push(BspConnectionFile { path, details }),
                Err(err) => invalid.push(err),
            }
        }
        candidates.sort_by(|a, b| {
            a.details
                .name
                .cmp(&b.details.name)
                .then_with(|| file_name_of(&a.path).cmp(&file_name_of(&b.path)))
        });
        BspDiscoveryResult {
            candidates,
            invalid,
        }
    }

    /// Deterministic pick over `discover`: candidate with the first name in
    /// lexicographic order, or None when the workspace has no valid file.
    pub fn pick(workspace_root: &Path) -> Option<BspConnectionFile> {
        Self::discover(workspace_root).candidates.into_iter().next()
    }

    /// Like `pick` but fails with a typed error when nothing usable exists.
    pub fn required(workspace_root: &Path) -> Result<BspConnectionFile, BspError> {
        Self::pick(workspace_root).ok_or_else(|| BspError::NoConnectionFile {
            workspace_root: workspace_root.display().to_string(),
        })
    }
}

fn file_name_of(p: &Path) -> String {
    p.file_name()
        .and_then(|n| n.to_str())
        .unwrap_or_default()
        .to_string()
}

fn parse_file(path: &Path) -> Result<BspConnectionDetails, BspError> {
    let bad = |detail: String| BspError::InvalidConnectionFile {
        path: path.display().to_string(),
        detail,
    };
    let text = std::fs::read_to_string(path).map_err(|e| bad(format!("unreadable: {e}")))?;
    let details: BspConnectionDetails =
        serde_json::from_str(&text).map_err(|e| bad(format!("malformed JSON: {e}")))?;
    if details.name.is_empty() {
        Err(bad("missing required field 'name'".to_string()))
    } else if details.argv.is_empty() {
        Err(bad("missing required field 'argv'".to_string()))
    } else {
        Ok(details)
    }
}
