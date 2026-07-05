//! Streaming-friendly decoder for the SemanticDB `TextDocuments` root message.
//!
//! Field numbers were derived from the authoritative
//! `scalameta/semanticdb/semanticdb.proto`:
//!
//! ```text
//! message TextDocuments { repeated TextDocument documents = 1; }
//! message TextDocument {
//!   Schema schema = 1;            string uri = 2;   string text = 3;
//!   string md5 = 11;              Language language = 10;
//!   repeated SymbolInformation symbols = 5;
//!   repeated SymbolOccurrence occurrences = 6;
//!   repeated Diagnostic diagnostics = 7;   // skipped
//!   repeated Synthetic synthetics = 12;    // skipped
//! }
//! message SymbolInformation {
//!   string symbol = 1; Kind kind = 3; int32 properties = 4;
//!   string display_name = 5; repeated string overridden_symbols = 19;
//! }
//! message SymbolOccurrence { Range range = 1; string symbol = 2; Role role = 3; }
//! message Range { int32 start_line = 1; int32 start_character = 2;
//!                 int32 end_line = 3;   int32 end_character = 4; }
//! ```
//!
//! `Range` coordinates are plain `int32` (NOT `sint32`), so plain varint
//! decoding without zigzag is the correct treatment.

use std::path::Path;

use crate::error::{SemanticdbError, SemanticdbResult};
use crate::model::{SdbDocument, SdbDocuments, SdbOccurrence, SdbRange, SdbSymbolInfo};
use crate::wire::ProtoReader;

// Wire types used below.
const WIRE_VARINT: u32 = 0;
const WIRE_LEN: u32 = 2;

// Field numbers.
mod f {
    // TextDocuments
    pub const DOCUMENTS: u32 = 1;
    // TextDocument
    pub const TD_SCHEMA: u32 = 1;
    pub const TD_URI: u32 = 2;
    pub const TD_TEXT: u32 = 3;
    pub const TD_SYMBOLS: u32 = 5;
    pub const TD_OCCURRENCES: u32 = 6;
    pub const TD_LANGUAGE: u32 = 10;
    pub const TD_MD5: u32 = 11;
    // SymbolInformation
    pub const SI_SYMBOL: u32 = 1;
    pub const SI_KIND: u32 = 3;
    pub const SI_PROPERTIES: u32 = 4;
    pub const SI_DISPLAY_NAME: u32 = 5;
    pub const SI_OVERRIDDEN_SYMBOLS: u32 = 19;
    // SymbolOccurrence
    pub const SO_RANGE: u32 = 1;
    pub const SO_SYMBOL: u32 = 2;
    pub const SO_ROLE: u32 = 3;
    // Range
    pub const R_START_LINE: u32 = 1;
    pub const R_START_CHARACTER: u32 = 2;
    pub const R_END_LINE: u32 = 3;
    pub const R_END_CHARACTER: u32 = 4;
}

/// Parses the whole payload of one `.semanticdb` file.
pub fn parse_text_documents(bytes: &[u8]) -> SemanticdbResult<SdbDocuments> {
    let mut reader = ProtoReader::new(bytes);
    let mut docs = Vec::new();
    while reader.has_remaining() {
        let tag = reader.read_tag()?;
        let field = tag >> 3;
        let wire = tag & 7;
        if field == f::DOCUMENTS && wire == WIRE_LEN {
            docs.push(parse_document(reader.read_message()?)?);
        } else {
            reader.skip_field(wire, field)?;
        }
    }
    Ok(SdbDocuments { documents: docs })
}

/// Reads and parses a `.semanticdb` file from disk.
pub fn parse_file(path: &Path) -> SemanticdbResult<SdbDocuments> {
    let bytes = std::fs::read(path).map_err(|e| SemanticdbError::Io(e.to_string()))?;
    parse_text_documents(&bytes)
}

fn parse_document(mut r: ProtoReader<'_>) -> SemanticdbResult<SdbDocument> {
    let mut schema = 0;
    let mut uri = String::new();
    let mut text = String::new();
    let mut md5 = String::new();
    let mut language = 0;
    let mut symbols = Vec::new();
    let mut occurrences = Vec::new();
    while r.has_remaining() {
        let tag = r.read_tag()?;
        let field = tag >> 3;
        let wire = tag & 7;
        match (field, wire) {
            (f::TD_SCHEMA, WIRE_VARINT) => schema = r.read_int32()?,
            (f::TD_URI, WIRE_LEN) => uri = r.read_string()?,
            (f::TD_TEXT, WIRE_LEN) => text = r.read_string()?,
            (f::TD_MD5, WIRE_LEN) => md5 = r.read_string()?,
            (f::TD_LANGUAGE, WIRE_VARINT) => language = r.read_int32()?,
            (f::TD_SYMBOLS, WIRE_LEN) => symbols.push(parse_symbol_info(r.read_message()?)?),
            (f::TD_OCCURRENCES, WIRE_LEN) => occurrences.push(parse_occurrence(r.read_message()?)?),
            // Diagnostics/synthetics and any unknown field are skipped opaquely.
            _ => r.skip_field(wire, field)?,
        }
    }
    Ok(SdbDocument {
        schema,
        uri,
        text,
        md5,
        language_code: language,
        symbols,
        occurrences,
    })
}

fn parse_symbol_info(mut r: ProtoReader<'_>) -> SemanticdbResult<SdbSymbolInfo> {
    let mut symbol = String::new();
    let mut kind = 0;
    let mut properties = 0;
    let mut display_name = String::new();
    let mut overridden = Vec::new();
    while r.has_remaining() {
        let tag = r.read_tag()?;
        let field = tag >> 3;
        let wire = tag & 7;
        match (field, wire) {
            (f::SI_SYMBOL, WIRE_LEN) => symbol = r.read_string()?,
            (f::SI_KIND, WIRE_VARINT) => kind = r.read_int32()?,
            (f::SI_PROPERTIES, WIRE_VARINT) => properties = r.read_int32()?,
            (f::SI_DISPLAY_NAME, WIRE_LEN) => display_name = r.read_string()?,
            (f::SI_OVERRIDDEN_SYMBOLS, WIRE_LEN) => overridden.push(r.read_string()?),
            _ => r.skip_field(wire, field)?,
        }
    }
    Ok(SdbSymbolInfo {
        symbol,
        kind_code: kind,
        properties,
        display_name,
        overridden_symbols: overridden,
    })
}

fn parse_occurrence(mut r: ProtoReader<'_>) -> SemanticdbResult<SdbOccurrence> {
    let mut range = None;
    let mut symbol = String::new();
    let mut role = 0;
    while r.has_remaining() {
        let tag = r.read_tag()?;
        let field = tag >> 3;
        let wire = tag & 7;
        match (field, wire) {
            (f::SO_RANGE, WIRE_LEN) => range = Some(parse_range(r.read_message()?)?),
            (f::SO_SYMBOL, WIRE_LEN) => symbol = r.read_string()?,
            (f::SO_ROLE, WIRE_VARINT) => role = r.read_int32()?,
            _ => r.skip_field(wire, field)?,
        }
    }
    Ok(SdbOccurrence {
        range,
        symbol,
        role_code: role,
    })
}

fn parse_range(mut r: ProtoReader<'_>) -> SemanticdbResult<SdbRange> {
    let mut start_line = 0;
    let mut start_character = 0;
    let mut end_line = 0;
    let mut end_character = 0;
    while r.has_remaining() {
        let tag = r.read_tag()?;
        let field = tag >> 3;
        let wire = tag & 7;
        match (field, wire) {
            (f::R_START_LINE, WIRE_VARINT) => start_line = r.read_int32()?,
            (f::R_START_CHARACTER, WIRE_VARINT) => start_character = r.read_int32()?,
            (f::R_END_LINE, WIRE_VARINT) => end_line = r.read_int32()?,
            (f::R_END_CHARACTER, WIRE_VARINT) => end_character = r.read_int32()?,
            _ => r.skip_field(wire, field)?,
        }
    }
    Ok(SdbRange {
        start_line,
        start_character,
        end_line,
        end_character,
    })
}
