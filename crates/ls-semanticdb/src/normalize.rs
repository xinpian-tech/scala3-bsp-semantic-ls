//! Turns one raw [`SdbDocument`] into the shared [`NormalizedDocument`] model.
//!
//! Rules:
//!   - local symbols become `SymbolKey::local(sym, doc_id)` (SemanticDB locals
//!     are only unique per document); everything else is `SymbolKey::global`;
//!   - owner/package names are derived from the symbol string grammar;
//!   - kind codes map through `SymKind::from_code`; property bits are kept;
//!   - occurrences without a range, with an empty symbol, or with an unknown
//!     role are dropped: they cannot be materialized as exact locations.

use ls_index_model::{
    DocId, NormalizedDocument, Occurrence, Role, Span, SymKind, SymbolInfo, SymbolKey,
};

use crate::model::{sdb_language, sdb_role, SdbDocument};
use crate::symbols;

fn key_of(sym: &str, doc_id: DocId) -> SymbolKey {
    if symbols::is_local(sym) {
        SymbolKey::local(sym, doc_id)
    } else {
        SymbolKey::global(sym)
    }
}

pub fn normalize(doc: &SdbDocument, doc_id: DocId) -> NormalizedDocument {
    let symbols_out = doc
        .symbols
        .iter()
        .map(|s| {
            let display = if !s.display_name.is_empty() {
                s.display_name.clone()
            } else {
                symbols::display_name(&s.symbol).unwrap_or_else(|| s.symbol.clone())
            };
            SymbolInfo {
                key: key_of(&s.symbol, doc_id),
                display_name: display,
                owner_name: symbols::owner_name(&s.symbol),
                package_name: symbols::package_name(&s.symbol),
                kind: SymKind::from_code(s.kind_code),
                properties: s.properties as u32,
                overridden_symbols: s.overridden_symbols.clone(),
            }
        })
        .collect();

    let occurrences_out = doc
        .occurrences
        .iter()
        .filter_map(|o| match (&o.range, role_of(o.role_code)) {
            (Some(r), Some(role)) if !o.symbol.is_empty() => Some(Occurrence::new(
                key_of(&o.symbol, doc_id),
                Span::new(
                    r.start_line as u32,
                    r.start_character as u32,
                    r.end_line as u32,
                    r.end_character as u32,
                ),
                role,
            )),
            _ => None,
        })
        .collect();

    NormalizedDocument {
        uri: doc.uri.clone(),
        md5: doc.md5.clone(),
        schema_version: doc.schema,
        language: sdb_language::name(doc.language_code).to_string(),
        symbols: symbols_out,
        occurrences: occurrences_out,
    }
}

fn role_of(code: i32) -> Option<Role> {
    match code {
        sdb_role::REFERENCE => Some(Role::Reference),
        sdb_role::DEFINITION => Some(Role::Definition),
        _ => None,
    }
}
