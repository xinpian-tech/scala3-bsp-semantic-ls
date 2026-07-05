//! The subset of LSP wire types the server layer needs, hand-rolled over serde
//! (the repo carries its JSON-RPC by hand rather than pulling an LSP crate, so
//! the offline build stays dependency-light). Field names serialize to the LSP
//! camelCase spelling.

use serde::{Deserialize, Serialize};

/// A zero-based `(line, character)` position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Position {
    pub line: u32,
    pub character: u32,
}

/// A half-open `[start, end)` range.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// The LSP `integer | string` diagnostic code.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DiagnosticCode {
    Integer(i64),
    String(String),
}

/// A single diagnostic, already converted from its BSP carrier to the LSP shape.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Diagnostic {
    pub range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub severity: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub code: Option<DiagnosticCode>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub message: String,
}

/// `textDocument/publishDiagnostics` params: the merged list for one file URI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublishDiagnosticsParams {
    pub uri: String,
    pub diagnostics: Vec<Diagnostic>,
}
