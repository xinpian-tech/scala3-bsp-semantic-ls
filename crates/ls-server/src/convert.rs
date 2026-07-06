//! Pure conversions from the index/engine model to the LSP result types, plus
//! the LSP result types themselves. A behavior-preserving port of the Scala
//! `ls.core.LspConvert` object (the diagnostic conversion lives in
//! [`crate::diagnostics`], the other half of `LspConvert`).
//!
//! [`Span`] and the LSP [`Range`] share semantics by construction (zero-based
//! lines, UTF-16 characters, end-exclusive), so the position mapping is direct —
//! no coordinate transform. The result types are hand-rolled over serde (the
//! server carries its LSP wire types by hand rather than pulling an LSP crate),
//! and the two enums serialize to the LSP integer codes lsp4j emits.

use std::collections::BTreeMap;

use serde::{Serialize, Serializer};

use ls_engine::{HighlightKind, WorkspaceEditPlan};
use ls_index_model::{LsError, Span, SymKind};

use crate::protocol::{Position, Range};

/// LSP `Location`: a range within a document identified by its `file://` URI.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Location {
    pub uri: String,
    pub range: Range,
}

/// LSP `DocumentHighlight`: a range plus its read/write [`DocumentHighlightKind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct DocumentHighlight {
    pub range: Range,
    pub kind: DocumentHighlightKind,
}

/// LSP `DocumentHighlightKind`, serialized as its integer code (`Text=1`,
/// `Read=2`, `Write=3`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentHighlightKind {
    Text,
    Read,
    Write,
}

impl DocumentHighlightKind {
    /// The LSP wire code.
    pub fn code(self) -> i32 {
        match self {
            DocumentHighlightKind::Text => 1,
            DocumentHighlightKind::Read => 2,
            DocumentHighlightKind::Write => 3,
        }
    }
}

impl Serialize for DocumentHighlightKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(self.code())
    }
}

/// LSP `SymbolKind`, serialized as its integer code. Only the subset the index
/// model produces is represented; the mapping is [`symbol_kind`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SymbolKind {
    Package,
    Class,
    Method,
    Field,
    Constructor,
    Interface,
    Variable,
    Object,
    Null,
    TypeParameter,
}

impl SymbolKind {
    /// The LSP wire code (the lsp4j `SymbolKind` enum values).
    pub fn code(self) -> i32 {
        match self {
            SymbolKind::Package => 4,
            SymbolKind::Class => 5,
            SymbolKind::Method => 6,
            SymbolKind::Field => 8,
            SymbolKind::Constructor => 9,
            SymbolKind::Interface => 11,
            SymbolKind::Variable => 13,
            SymbolKind::Object => 19,
            SymbolKind::Null => 21,
            SymbolKind::TypeParameter => 26,
        }
    }
}

impl Serialize for SymbolKind {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_i32(self.code())
    }
}

/// LSP `TextEdit`: replace `range` with `new_text`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct TextEdit {
    pub range: Range,
    #[serde(rename = "newText")]
    pub new_text: String,
}

/// LSP `WorkspaceEdit`: `changes` keyed by `file://` URI. Serialized as the LSP
/// `{ [uri]: TextEdit[] }` object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct WorkspaceEdit {
    pub changes: BTreeMap<String, Vec<TextEdit>>,
}

/// Index [`Span`] -> LSP [`Range`]. Direct, since the coordinate systems match.
pub fn range(span: Span) -> Range {
    Range {
        start: Position {
            line: span.start_line,
            character: span.start_char,
        },
        end: Position {
            line: span.end_line,
            character: span.end_char,
        },
    }
}

/// LSP [`Range`] -> index [`Span`], the inverse of [`range`].
pub fn span(range: &Range) -> Span {
    Span::new(
        range.start.line,
        range.start.character,
        range.end.line,
        range.end.character,
    )
}

/// `(file_uri, span)` -> LSP [`Location`].
pub fn location(file_uri: &str, span: Span) -> Location {
    Location {
        uri: file_uri.to_string(),
        range: range(span),
    }
}

/// Index [`SymKind`] -> LSP [`SymbolKind`], mirroring `LspConvert.symbolKind`.
pub fn symbol_kind(kind: SymKind) -> SymbolKind {
    match kind {
        SymKind::Class => SymbolKind::Class,
        SymKind::Trait | SymKind::Interface => SymbolKind::Interface,
        SymKind::Object | SymKind::PackageObject => SymbolKind::Object,
        SymKind::Method | SymKind::Macro => SymbolKind::Method,
        SymKind::Constructor => SymbolKind::Constructor,
        SymKind::Type => SymbolKind::Class,
        SymKind::TypeParameter => SymbolKind::TypeParameter,
        SymKind::Field => SymbolKind::Field,
        SymKind::Package => SymbolKind::Package,
        SymKind::LocalValue | SymKind::LocalVariable => SymbolKind::Variable,
        SymKind::Parameter | SymKind::SelfParameter => SymbolKind::Variable,
        SymKind::UnknownKind => SymbolKind::Null,
    }
}

/// Engine [`HighlightKind`] -> LSP [`DocumentHighlightKind`].
pub fn highlight_kind(kind: HighlightKind) -> DocumentHighlightKind {
    match kind {
        HighlightKind::Read => DocumentHighlightKind::Read,
        HighlightKind::Write => DocumentHighlightKind::Write,
    }
}

/// [`WorkspaceEditPlan`] (SemanticDB URIs) -> LSP [`WorkspaceEdit`] keyed by
/// `file://` URI. A SemanticDB URI that does not resolve to a file fails the
/// whole conversion with [`LsError::NotIndexed`] — a rename must never silently
/// drop edits (mirroring `LspConvert.workspaceEdit`).
pub fn workspace_edit(
    plan: &WorkspaceEditPlan,
    to_file_uri: impl Fn(&str) -> Option<String>,
) -> Result<WorkspaceEdit, LsError> {
    let mut changes = BTreeMap::new();
    // `plan.edits` is a `BTreeMap`, so this iterates in SemanticDB-URI order,
    // matching the Scala `plan.edits.toVector.sortBy(_._1)`.
    for (sdb_uri, edits) in &plan.edits {
        let file_uri = to_file_uri(sdb_uri).ok_or_else(|| LsError::NotIndexed {
            uri: sdb_uri.clone(),
        })?;
        let list = edits
            .iter()
            .map(|e| TextEdit {
                range: range(e.span),
                new_text: e.new_text.clone(),
            })
            .collect();
        changes.insert(file_uri, list);
    }
    Ok(WorkspaceEdit { changes })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ls_engine::TextEditSpan;
    use serde_json::json;

    #[test]
    fn span_maps_to_range_directly_and_back() {
        let s = Span::new(1, 2, 3, 4);
        let r = range(s);
        assert_eq!(
            serde_json::to_value(r).unwrap(),
            json!({ "start": { "line": 1, "character": 2 }, "end": { "line": 3, "character": 4 } })
        );
        assert_eq!(span(&r), s);
    }

    #[test]
    fn location_serializes_to_uri_and_range() {
        let loc = location("file:///ws/a.scala", Span::new(0, 0, 0, 3));
        assert_eq!(
            serde_json::to_value(loc).unwrap(),
            json!({
                "uri": "file:///ws/a.scala",
                "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 3 } }
            })
        );
    }

    #[test]
    fn highlight_kind_maps_read_and_write_to_lsp_codes() {
        assert_eq!(
            highlight_kind(HighlightKind::Read),
            DocumentHighlightKind::Read
        );
        assert_eq!(
            highlight_kind(HighlightKind::Write),
            DocumentHighlightKind::Write
        );
        assert_eq!(
            serde_json::to_value(DocumentHighlightKind::Read).unwrap(),
            json!(2)
        );
        assert_eq!(
            serde_json::to_value(DocumentHighlightKind::Write).unwrap(),
            json!(3)
        );
    }

    #[test]
    fn document_highlight_serializes_range_and_kind() {
        let h = DocumentHighlight {
            range: range(Span::new(2, 4, 2, 8)),
            kind: DocumentHighlightKind::Write,
        };
        assert_eq!(
            serde_json::to_value(h).unwrap(),
            json!({
                "range": { "start": { "line": 2, "character": 4 }, "end": { "line": 2, "character": 8 } },
                "kind": 3
            })
        );
    }

    // Every SymKind arm maps as LspConvert.symbolKind does, including the
    // fold-together cases (Trait/Interface, Object/PackageObject, Method/Macro,
    // LocalValue/LocalVariable, Parameter/SelfParameter) and Type -> Class.
    #[test]
    fn symbol_kind_maps_every_arm_to_the_lsp_code() {
        let cases = [
            (SymKind::Class, 5),
            (SymKind::Trait, 11),
            (SymKind::Interface, 11),
            (SymKind::Object, 19),
            (SymKind::PackageObject, 19),
            (SymKind::Method, 6),
            (SymKind::Macro, 6),
            (SymKind::Constructor, 9),
            (SymKind::Type, 5),
            (SymKind::TypeParameter, 26),
            (SymKind::Field, 8),
            (SymKind::Package, 4),
            (SymKind::LocalValue, 13),
            (SymKind::LocalVariable, 13),
            (SymKind::Parameter, 13),
            (SymKind::SelfParameter, 13),
            (SymKind::UnknownKind, 21),
        ];
        for (kind, code) in cases {
            assert_eq!(symbol_kind(kind).code(), code, "{kind:?}");
            assert_eq!(
                serde_json::to_value(symbol_kind(kind)).unwrap(),
                json!(code),
                "{kind:?}"
            );
        }
    }

    #[test]
    fn workspace_edit_keys_by_file_uri_in_sorted_order() {
        let mut edits = BTreeMap::new();
        edits.insert(
            "b/B.scala".to_string(),
            vec![TextEditSpan {
                span: Span::new(0, 0, 0, 1),
                new_text: "Y".to_string(),
            }],
        );
        edits.insert(
            "a/A.scala".to_string(),
            vec![TextEditSpan {
                span: Span::new(1, 2, 1, 3),
                new_text: "X".to_string(),
            }],
        );
        let plan = WorkspaceEditPlan {
            edits,
            occurrence_count: 2,
        };
        let edit = workspace_edit(&plan, |sdb| Some(format!("file:///root/{sdb}"))).unwrap();
        assert_eq!(
            serde_json::to_value(edit).unwrap(),
            json!({
                "changes": {
                    "file:///root/a/A.scala": [
                        { "range": { "start": { "line": 1, "character": 2 }, "end": { "line": 1, "character": 3 } }, "newText": "X" }
                    ],
                    "file:///root/b/B.scala": [
                        { "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 1 } }, "newText": "Y" }
                    ]
                }
            })
        );
    }

    // An edited URI that cannot be resolved to a file fails the whole
    // conversion, so a rename never silently drops edits.
    #[test]
    fn workspace_edit_fails_when_an_edited_uri_is_unresolvable() {
        let mut edits = BTreeMap::new();
        edits.insert(
            "gone/G.scala".to_string(),
            vec![TextEditSpan {
                span: Span::new(0, 0, 0, 1),
                new_text: "Z".to_string(),
            }],
        );
        let plan = WorkspaceEditPlan {
            edits,
            occurrence_count: 1,
        };
        let err = workspace_edit(&plan, |_| None).unwrap_err();
        assert!(matches!(err, LsError::NotIndexed { uri } if uri == "gone/G.scala"));
    }
}
