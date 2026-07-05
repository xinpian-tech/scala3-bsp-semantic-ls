//! Raw SemanticDB messages exactly as decoded from the protobuf wire format,
//! before any normalization. Field subset: only what workspace-symbol /
//! references / rename need. Mirrors the Scala `ls.semanticdb` model.

/// SemanticDB `SymbolOccurrence.Role` codes (semanticdb.proto).
pub mod sdb_role {
    pub const UNKNOWN_ROLE: i32 = 0;
    pub const REFERENCE: i32 = 1;
    pub const DEFINITION: i32 = 2;
}

/// SemanticDB `Language` codes (semanticdb.proto).
pub mod sdb_language {
    pub const UNKNOWN: i32 = 0;
    pub const SCALA: i32 = 1;
    pub const JAVA: i32 = 2;
    pub const PROTOBUF: i32 = 3;

    /// The lowercase language name, or `"unknown"` for any other code.
    pub fn name(code: i32) -> &'static str {
        match code {
            SCALA => "scala",
            JAVA => "java",
            PROTOBUF => "protobuf",
            _ => "unknown",
        }
    }
}

/// SemanticDB `Range`: zero-based, end-exclusive character (LSP convention). All
/// four fields are plain `int32` on the wire (not `sint32`, so no zigzag).
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SdbRange {
    pub start_line: i32,
    pub start_character: i32,
    pub end_line: i32,
    pub end_character: i32,
}

/// SemanticDB `SymbolInformation` subset.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SdbSymbolInfo {
    pub symbol: String,
    pub kind_code: i32,
    pub properties: i32,
    pub display_name: String,
    pub overridden_symbols: Vec<String>,
}

/// SemanticDB `SymbolOccurrence`. `range` is optional on the wire.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SdbOccurrence {
    pub range: Option<SdbRange>,
    pub symbol: String,
    pub role_code: i32,
}

/// SemanticDB `TextDocument` subset. Diagnostics and synthetics payloads are
/// skipped during decoding without materialization.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SdbDocument {
    pub schema: i32,
    pub uri: String,
    pub text: String,
    pub md5: String,
    pub language_code: i32,
    pub symbols: Vec<SdbSymbolInfo>,
    pub occurrences: Vec<SdbOccurrence>,
}

/// SemanticDB `TextDocuments`, the root message of a `.semanticdb` file.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct SdbDocuments {
    pub documents: Vec<SdbDocument>,
}
