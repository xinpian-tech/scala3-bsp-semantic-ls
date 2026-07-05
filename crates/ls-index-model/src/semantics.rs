//! Normalized SemanticDB shapes — the unit of ingest shared across crates.
//!
//! These mirror the Scala `ls.index` model (`SymbolInfo`, `Occurrence`,
//! `NormalizedDocument`) so the SemanticDB ingest crate and the engine layer
//! agree on one document representation.

use crate::flags::Role;
use crate::symbol::{SymKind, SymbolKey};
use crate::text::Span;

/// Normalized `SymbolInformation` extracted from SemanticDB.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SymbolInfo {
    pub key: SymbolKey,
    pub display_name: String,
    pub owner_name: Option<String>,
    pub package_name: Option<String>,
    pub kind: SymKind,
    /// SemanticDB property bit mask (see [`crate::sym_props`]).
    pub properties: u32,
    pub overridden_symbols: Vec<String>,
}

/// Normalized `SymbolOccurrence` extracted from SemanticDB.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Occurrence {
    pub key: SymbolKey,
    pub span: Span,
    pub role: Role,
    pub synthetic: bool,
}

impl Occurrence {
    /// A non-synthetic occurrence (the common case), matching the Scala default.
    pub fn new(key: SymbolKey, span: Span, role: Role) -> Self {
        Occurrence {
            key,
            span,
            role,
            synthetic: false,
        }
    }
}

/// One normalized SemanticDB `TextDocument`, the unit of ingest.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct NormalizedDocument {
    pub uri: String,
    pub md5: String,
    pub schema_version: i32,
    pub language: String,
    pub symbols: Vec<SymbolInfo>,
    pub occurrences: Vec<Occurrence>,
}
