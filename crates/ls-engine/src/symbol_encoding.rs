//! Canonical encoding of a [`SymbolKey`] into the single-string symbol
//! dictionary of a postings segment. Global symbols are stored verbatim;
//! SemanticDB local symbols (`local0`, `local1`, ...) are only unique within one
//! document, so they are qualified with the persistent doc id (`local0@42`). The
//! `@` marker cannot appear in a raw SemanticDB local symbol, so the encoding is
//! collision-free and reversible.

use ls_index_model::{DocId, SymbolKey};
use ls_semanticdb::symbols;

const SEP: char = '@';

pub fn encode_key(key: &SymbolKey) -> String {
    match key.local_doc {
        Some(doc) => format!("{}{}{}", key.semantic_symbol, SEP, doc.value()),
        None => key.semantic_symbol.clone(),
    }
}

pub fn encode(semantic_symbol: &str, local_doc_id: Option<u64>) -> String {
    match local_doc_id {
        Some(id) if symbols::is_local(semantic_symbol) => format!("{semantic_symbol}{SEP}{id}"),
        _ => semantic_symbol.to_string(),
    }
}

/// Inverse of [`encode`]: (raw semantic symbol, local doc id when local).
pub fn decode(encoded: &str) -> (String, Option<u64>) {
    if encoded.starts_with("local") {
        if let Some(at) = encoded.find(SEP) {
            if at > 0 {
                if let Ok(doc_id) = encoded[at + 1..].parse::<u64>() {
                    return (encoded[..at].to_string(), Some(doc_id));
                }
            }
        }
    }
    (encoded.to_string(), None)
}

pub fn to_key(encoded: &str) -> SymbolKey {
    let (raw, doc) = decode(encoded);
    match doc {
        Some(id) => SymbolKey::local(raw, DocId::new(id)),
        None => SymbolKey::global(raw),
    }
}
