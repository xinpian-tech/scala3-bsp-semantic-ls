//! Loads the [`BspProjectModel`] from a live [`BspSession`]:
//! workspace/buildTargets filtered to Scala 3 targets, then buildTarget/sources
//! (directories expanded by walking `*.scala` files) and
//! buildTarget/scalacOptions, assembled with SemanticDB config extraction.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::errors::BspError;
use crate::model::{BspProjectModel, BspTarget};
use crate::protocol::{ScalacOptionsItem, SourceItem, SOURCE_ITEM_DIRECTORY};
use crate::semanticdb::SemanticdbFlags;
use crate::session::BspSession;
use crate::uri::{path_to_uri, uri_to_path};

pub struct ProjectModelLoader;

impl ProjectModelLoader {
    pub fn load(session: &BspSession) -> Result<BspProjectModel, BspError> {
        let workspace_root = session.workspace_root.clone();

        // Keep Scala 3 targets only, in a deterministic bspId order.
        let mut scala3: Vec<(crate::protocol::BuildTarget, String)> = session
            .workspace_build_targets()?
            .into_iter()
            .filter_map(|t| {
                if !t.language_ids.iter().any(|l| l == "scala") {
                    return None;
                }
                parse_scala_version(&t.data)
                    .filter(|v| is_scala3(v))
                    .map(|v| (t, v))
            })
            .collect();
        scala3.sort_by(|a, b| a.0.id.uri.cmp(&b.0.id.uri));

        if scala3.is_empty() {
            return Ok(BspProjectModel::new(Vec::new(), HashMap::new()));
        }

        let ids: Vec<String> = scala3.iter().map(|(t, _)| t.id.uri.clone()).collect();
        let sources_by_target: HashMap<String, Vec<SourceItem>> = session
            .build_target_sources(&ids)?
            .into_iter()
            .map(|item| (item.target.uri, item.sources))
            .collect();
        let options_by_target: HashMap<String, ScalacOptionsItem> = session
            .build_target_scalac_options(&ids)?
            .into_iter()
            .map(|item| (item.target.uri.clone(), item))
            .collect();

        let mut targets = Vec::with_capacity(scala3.len());
        for (build_target, scala_version) in &scala3 {
            let bsp_id = build_target.id.uri.clone();
            let options_item =
                options_by_target
                    .get(&bsp_id)
                    .ok_or_else(|| BspError::InvalidResponse {
                        method: "buildTarget/scalacOptions".to_string(),
                        detail: format!("missing item for target {bsp_id}"),
                    })?;
            if options_item.class_directory.is_empty() {
                return Err(BspError::InvalidResponse {
                    method: "buildTarget/scalacOptions".to_string(),
                    detail: format!("missing classDirectory for {bsp_id}"),
                });
            }
            let class_directory = uri_to_path(&options_item.class_directory).map_err(|e| {
                BspError::InvalidResponse {
                    method: "buildTarget/scalacOptions".to_string(),
                    detail: format!("bad file uri '{}': {e}", options_item.class_directory),
                }
            })?;
            let options = options_item.options.clone();
            let semanticdb = SemanticdbFlags::extract(&options, &class_directory, &workspace_root);
            let sources = expand_sources(
                sources_by_target
                    .get(&bsp_id)
                    .map(Vec::as_slice)
                    .unwrap_or(&[]),
            )?;
            let mut deps: Vec<String> = build_target
                .dependencies
                .iter()
                .map(|d| d.uri.clone())
                .collect();
            deps.sort();
            deps.dedup();
            let display_name = build_target
                .display_name
                .clone()
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| bsp_id.clone());
            targets.push(BspTarget {
                bsp_id,
                display_name,
                scala_version: scala_version.clone(),
                scalac_options: options,
                class_directory,
                semanticdb_root: semanticdb.semanticdb_root,
                sourceroot: Some(semanticdb.sourceroot),
                sources,
                direct_deps: deps,
            });
        }

        // First target in bspId order wins for shared sources: deterministic.
        let mut uri_to_target: HashMap<String, String> = HashMap::new();
        for target in &targets {
            for source in &target.sources {
                uri_to_target
                    .entry(path_to_uri(source))
                    .or_insert_with(|| target.bsp_id.clone());
            }
        }

        Ok(BspProjectModel::new(targets, uri_to_target))
    }
}

fn is_scala3(version: &str) -> bool {
    version == "3" || version.starts_with("3.")
}

/// `BuildTarget.data` survives the jsonrpc round-trip as raw JSON; a parse only
/// counts when it carries a `scalaVersion`, so unrelated data kinds do not slip
/// through.
fn parse_scala_version(data: &Option<Value>) -> Option<String> {
    data.as_ref()?
        .get("scalaVersion")?
        .as_str()
        .map(str::to_string)
}

/// FILE items are kept when they are Scala sources; DIRECTORY items are expanded
/// by walking every `*.scala` file under them. Result is deduplicated and
/// sorted for determinism.
fn expand_sources(items: &[SourceItem]) -> Result<Vec<PathBuf>, BspError> {
    let mut out: Vec<PathBuf> = Vec::new();
    for item in items {
        let path = uri_to_path(&item.uri).map_err(|e| BspError::InvalidResponse {
            method: "buildTarget/sources".to_string(),
            detail: format!("bad file uri '{}': {e}", item.uri),
        })?;
        if item.kind == SOURCE_ITEM_DIRECTORY {
            if path.is_dir() {
                collect_scala_files(&path, &mut out);
            }
        } else if is_scala_file(&path) {
            out.push(path);
        }
    }
    out.sort_by(|a, b| a.to_string_lossy().cmp(&b.to_string_lossy()));
    out.dedup();
    Ok(out)
}

fn collect_scala_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_scala_files(&path, out);
        } else if path.is_file() && is_scala_file(&path) {
            out.push(path);
        }
    }
}

fn is_scala_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.ends_with(".scala"))
}
