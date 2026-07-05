//! BSP wire types (core protocol plus the Scala extension). Only the fields the
//! client sends or reads are modeled; every optional field defaults so partial
//! server payloads never fail to deserialize. camelCase matches the spec, so a
//! real build server and the in-process fake speak the same JSON.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// BSP protocol version this client advertises.
pub const PROTOCOL_VERSION: &str = "2.1.0";

/// `StatusCode.OK`.
pub const STATUS_OK: i32 = 1;

/// `SourceItemKind.FILE`.
pub const SOURCE_ITEM_FILE: i32 = 1;
/// `SourceItemKind.DIRECTORY`.
pub const SOURCE_ITEM_DIRECTORY: i32 = 2;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildTargetIdentifier {
    pub uri: String,
}

// --- build/initialize ---

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeBuildParams {
    pub display_name: String,
    pub version: String,
    pub bsp_version: String,
    pub root_uri: String,
    pub capabilities: BuildClientCapabilities,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildClientCapabilities {
    pub language_ids: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeBuildResult {
    #[serde(default)]
    pub display_name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub bsp_version: String,
    pub capabilities: BuildServerCapabilities,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildServerCapabilities {
    #[serde(default)]
    pub compile_provider: Option<CompileProvider>,
    #[serde(default)]
    pub inverse_sources_provider: Option<bool>,
    #[serde(default)]
    pub dependency_sources_provider: Option<bool>,
    #[serde(default)]
    pub output_paths_provider: Option<bool>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileProvider {
    #[serde(default)]
    pub language_ids: Vec<String>,
}

// --- workspace/buildTargets ---

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WorkspaceBuildTargetsResult {
    #[serde(default)]
    pub targets: Vec<BuildTarget>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildTarget {
    pub id: BuildTargetIdentifier,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub language_ids: Vec<String>,
    #[serde(default)]
    pub dependencies: Vec<BuildTargetIdentifier>,
    #[serde(default)]
    pub data_kind: Option<String>,
    #[serde(default)]
    pub data: Option<Value>,
}

// --- buildTarget/sources ---

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourcesParams {
    pub targets: Vec<BuildTargetIdentifier>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourcesResult {
    #[serde(default)]
    pub items: Vec<SourcesItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourcesItem {
    pub target: BuildTargetIdentifier,
    #[serde(default)]
    pub sources: Vec<SourceItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceItem {
    pub uri: String,
    pub kind: i32,
    #[serde(default)]
    pub generated: bool,
}

// --- buildTarget/scalacOptions ---

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScalacOptionsParams {
    pub targets: Vec<BuildTargetIdentifier>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScalacOptionsResult {
    #[serde(default)]
    pub items: Vec<ScalacOptionsItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScalacOptionsItem {
    pub target: BuildTargetIdentifier,
    #[serde(default)]
    pub options: Vec<String>,
    #[serde(default)]
    pub classpath: Vec<String>,
    #[serde(default)]
    pub class_directory: String,
}

// --- buildTarget/compile ---

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileParams {
    pub targets: Vec<BuildTargetIdentifier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub origin_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CompileResult {
    pub status_code: Option<i32>,
    #[serde(default)]
    pub origin_id: Option<String>,
}

// --- buildTarget/inverseSources ---

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InverseSourcesParams {
    pub text_document: TextDocumentIdentifier,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TextDocumentIdentifier {
    pub uri: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InverseSourcesResult {
    #[serde(default)]
    pub targets: Vec<BuildTargetIdentifier>,
}

// --- buildTarget/dependencySources ---

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DependencySourcesParams {
    pub targets: Vec<BuildTargetIdentifier>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DependencySourcesResult {
    #[serde(default)]
    pub items: Vec<DependencySourcesItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DependencySourcesItem {
    pub target: BuildTargetIdentifier,
    #[serde(default)]
    pub sources: Vec<String>,
}

// --- buildTarget/outputPaths ---

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputPathsParams {
    pub targets: Vec<BuildTargetIdentifier>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputPathsResult {
    #[serde(default)]
    pub items: Vec<OutputPathsItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OutputPathsItem {
    pub target: BuildTargetIdentifier,
    #[serde(default)]
    pub output_paths: Vec<OutputPathItem>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OutputPathItem {
    pub uri: String,
    pub kind: i32,
}

// --- server -> client notifications ---

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PublishDiagnosticsParams {
    pub text_document: TextDocumentIdentifier,
    #[serde(default)]
    pub build_target: Option<BuildTargetIdentifier>,
    #[serde(default)]
    pub diagnostics: Vec<Diagnostic>,
    #[serde(default)]
    pub reset: bool,
    #[serde(default)]
    pub origin_id: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Diagnostic {
    #[serde(default)]
    pub range: Option<Range>,
    #[serde(default)]
    pub severity: Option<i32>,
    #[serde(default)]
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Position {
    pub line: i32,
    pub character: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LogMessageParams {
    #[serde(rename = "type", default)]
    pub message_type: i32,
    #[serde(default)]
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ShowMessageParams {
    #[serde(rename = "type", default)]
    pub message_type: i32,
    #[serde(default)]
    pub message: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DidChangeBuildTarget {
    #[serde(default)]
    pub changes: Vec<BuildTargetEvent>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct BuildTargetEvent {
    pub target: BuildTargetIdentifier,
}
