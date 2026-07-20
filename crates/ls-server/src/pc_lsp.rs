//! Bridges the island's decoded payload carriers (`ls_pc_abi::payloads`) to the
//! `lsp-types` protocol shapes for the payload-backed LSP methods
//! (`textDocument/inlayHint`, `textDocument/selectionRange`,
//! `textDocument/foldingRange`).
//!
//! Unlike the hand-rolled serde layer the original 12-method surface uses
//! (`crate::convert`/`crate::pc_convert`), every NEW protocol shape on this
//! edge is an upstream `lsp_types` model — nothing here defines a wire struct.
//! The bridge only maps the flat ABI carriers into those models; the dispatch
//! layer serializes them into the existing `serde_json` response path
//! (`Response::success(id, serde_json::to_value(..))`).

use ls_pc_abi::payloads::{
    FoldingRange as AbiFoldingRange, InlayHint as AbiInlayHint, InlayLabelPart, Pos, Rng,
    TextEdit as AbiTextEdit,
};
use serde_json::Value;

/// ABI position (zero-based UTF-16, as LSP) -> `lsp_types::Position`.
pub fn lsp_position(pos: &Pos) -> lsp_types::Position {
    lsp_types::Position::new(pos.line, pos.character)
}

/// ABI range -> `lsp_types::Range`.
pub fn lsp_range(rng: &Rng) -> lsp_types::Range {
    lsp_types::Range::new(
        lsp_types::Position::new(rng.start_line, rng.start_character),
        lsp_types::Position::new(rng.end_line, rng.end_character),
    )
}

/// `lsp_types::Range` -> the ABI carrier range (both are zero-based UTF-16).
pub fn abi_rng(range: &lsp_types::Range) -> Rng {
    Rng {
        start_line: range.start.line,
        start_character: range.start.character,
        end_line: range.end.line,
        end_character: range.end.character,
    }
}

/// `lsp_types::Position` -> the ABI carrier position.
pub fn abi_pos(position: &lsp_types::Position) -> Pos {
    Pos {
        line: position.line,
        character: position.character,
    }
}

/// One decoded island inlay hint -> the `lsp_types::InlayHint`. The label is
/// always emitted as label PARTS (the island already normalized a plain-string
/// lsp4j label into one part), each carrying its optional target location and
/// string tooltip; `kind` maps the LSP ordinals (1 type, 2 parameter) and
/// anything else — including the island's `0` "no kind" — omits the field.
/// Opaque `data` passes through verbatim as JSON ([`data_json`]).
pub fn inlay_hint(hint: &AbiInlayHint) -> lsp_types::InlayHint {
    lsp_types::InlayHint {
        position: lsp_position(&hint.position),
        label: lsp_types::InlayHintLabel::LabelParts(
            hint.label_parts.iter().map(label_part).collect(),
        ),
        kind: match hint.kind {
            1 => Some(lsp_types::InlayHintKind::TYPE),
            2 => Some(lsp_types::InlayHintKind::PARAMETER),
            _ => None,
        },
        text_edits: hint
            .text_edits
            .as_ref()
            .map(|edits| edits.iter().map(text_edit).collect()),
        tooltip: None,
        padding_left: Some(hint.padding_left),
        padding_right: Some(hint.padding_right),
        data: hint.data.as_deref().map(data_json),
    }
}

fn label_part(part: &InlayLabelPart) -> lsp_types::InlayHintLabelPart {
    lsp_types::InlayHintLabelPart {
        value: part.text.clone(),
        tooltip: part
            .tooltip
            .clone()
            .map(lsp_types::InlayHintLabelPartTooltip::String),
        // A location whose URI does not parse as an `lsp_types::Uri` is dropped
        // rather than emitted malformed (the island only produces `file://`
        // URIs, so this is a defensive boundary, not an expected path).
        location: part.location.as_ref().and_then(|(uri, rng)| {
            uri.parse::<lsp_types::Uri>()
                .ok()
                .map(|uri| lsp_types::Location::new(uri, lsp_range(rng)))
        }),
        command: None,
    }
}

fn text_edit(edit: &AbiTextEdit) -> lsp_types::TextEdit {
    lsp_types::TextEdit::new(lsp_range(&edit.range), edit.new_text.clone())
}

/// The hint's opaque `data` bytes, passed through verbatim as JSON. The island
/// writes the lsp4j hint's `data` as its canonical gson JSON bytes, so a plain
/// parse restores the exact value the presentation compiler attached (the
/// `CompletionItem.data` idiom — carried, never interpreted). A non-JSON
/// payload (not producible by the real island) degrades to a JSON string of
/// the lossy-UTF-8 bytes so the opaque token still survives the round trip.
fn data_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes)
        .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(bytes).into_owned()))
}

/// One island selection chain (innermost first) -> the LSP linked
/// `SelectionRange` (each node's `parent` is the next-wider range). An empty
/// chain — the island had no enclosing node at the position — degrades to the
/// zero-width range at the queried position itself, preserving the spec's
/// `result[i]` ↔ `positions[i]` correspondence.
pub fn selection_chain(chain: &[Rng], queried: &lsp_types::Position) -> lsp_types::SelectionRange {
    let mut node: Option<lsp_types::SelectionRange> = None;
    for rng in chain.iter().rev() {
        node = Some(lsp_types::SelectionRange {
            range: lsp_range(rng),
            parent: node.map(Box::new),
        });
    }
    node.unwrap_or_else(|| lsp_types::SelectionRange {
        range: lsp_types::Range::new(*queried, *queried),
        parent: None,
    })
}

/// One island folding range -> the `lsp_types::FoldingRange`. The island's
/// `folding_kind` ordinals map 1 → comment, 2 → imports, 3 → region; `0`
/// ("none") and any unknown ordinal omit the `kind` field (the plain code
/// fold). Start/end characters are always present — the provider computes
/// exact spans.
pub fn folding_range(folding: &AbiFoldingRange) -> lsp_types::FoldingRange {
    lsp_types::FoldingRange {
        start_line: folding.range.start_line,
        start_character: Some(folding.range.start_character),
        end_line: folding.range.end_line,
        end_character: Some(folding.range.end_character),
        kind: match folding.kind {
            1 => Some(lsp_types::FoldingRangeKind::Comment),
            2 => Some(lsp_types::FoldingRangeKind::Imports),
            3 => Some(lsp_types::FoldingRangeKind::Region),
            _ => None,
        },
        collapsed_text: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn rng(sl: u32, sc: u32, el: u32, ec: u32) -> Rng {
        Rng {
            start_line: sl,
            start_character: sc,
            end_line: el,
            end_character: ec,
        }
    }

    // The full inlay-hint shape: label parts with location + tooltip, the type
    // kind, padding flags, text edits, and JSON `data` passed through verbatim
    // — pinned as the serialized LSP JSON, so the lsp-types bridge (not a
    // hand-rolled model) is what the wire carries.
    #[test]
    fn inlay_hint_maps_the_full_carrier_to_the_lsp_shape() {
        let hint = AbiInlayHint {
            position: Pos {
                line: 1,
                character: 9,
            },
            label_parts: vec![InlayLabelPart {
                text: ": Int".to_string(),
                location: Some(("file:///ws/A.scala".to_string(), rng(0, 2, 0, 5))),
                tooltip: Some("inferred".to_string()),
            }],
            kind: 1,
            padding_left: true,
            padding_right: false,
            text_edits: Some(vec![AbiTextEdit {
                range: rng(1, 9, 1, 9),
                new_text: ": Int".to_string(),
            }]),
            data: Some(b"{\"symbol\":\"scala/Int#\"}".to_vec()),
        };
        assert_eq!(
            serde_json::to_value(inlay_hint(&hint)).unwrap(),
            json!({
                "position": { "line": 1, "character": 9 },
                "label": [{
                    "value": ": Int",
                    "tooltip": "inferred",
                    "location": {
                        "uri": "file:///ws/A.scala",
                        "range": { "start": { "line": 0, "character": 2 }, "end": { "line": 0, "character": 5 } }
                    }
                }],
                "kind": 1,
                "textEdits": [{
                    "range": { "start": { "line": 1, "character": 9 }, "end": { "line": 1, "character": 9 } },
                    "newText": ": Int"
                }],
                "paddingLeft": true,
                "paddingRight": false,
                "data": { "symbol": "scala/Int#" }
            })
        );
    }

    // Kind 0 is the island's "no kind": the field is omitted, not emitted as 0
    // (and an unknown ordinal folds the same way). Absent optional carriers
    // (location/tooltip/edits/data) are omitted, never null.
    #[test]
    fn inlay_hint_omits_no_kind_and_absent_optionals() {
        let hint = AbiInlayHint {
            position: Pos {
                line: 0,
                character: 4,
            },
            label_parts: vec![InlayLabelPart {
                text: "(using x)".to_string(),
                location: None,
                tooltip: None,
            }],
            kind: 0,
            padding_left: false,
            padding_right: false,
            text_edits: None,
            data: None,
        };
        let value = serde_json::to_value(inlay_hint(&hint)).unwrap();
        assert!(value.get("kind").is_none(), "{value}");
        assert!(value.get("textEdits").is_none(), "{value}");
        assert!(value.get("data").is_none(), "{value}");
        assert_eq!(value["label"], json!([{ "value": "(using x)" }]));
    }

    // Non-JSON data bytes (not producible by the real island, which writes
    // canonical gson JSON) degrade to a JSON string of the bytes — the opaque
    // token still round-trips, never a drop or a panic.
    #[test]
    fn inlay_hint_data_degrades_non_json_bytes_to_a_string() {
        let hint = AbiInlayHint {
            position: Pos::default(),
            label_parts: Vec::new(),
            kind: 0,
            padding_left: false,
            padding_right: false,
            text_edits: None,
            data: Some(b"fake/Symbol#".to_vec()),
        };
        let value = serde_json::to_value(inlay_hint(&hint)).unwrap();
        assert_eq!(value["data"], json!("fake/Symbol#"));
    }

    // A label-part location whose URI does not parse is dropped (defensive
    // boundary); the part itself survives with its text.
    #[test]
    fn inlay_hint_drops_an_unparseable_label_location() {
        let hint = AbiInlayHint {
            position: Pos::default(),
            label_parts: vec![InlayLabelPart {
                text: "T".to_string(),
                location: Some(("not a uri".to_string(), rng(0, 0, 0, 1))),
                tooltip: None,
            }],
            kind: 2,
            padding_left: false,
            padding_right: false,
            text_edits: None,
            data: None,
        };
        let value = serde_json::to_value(inlay_hint(&hint)).unwrap();
        assert_eq!(value["kind"], 2);
        assert_eq!(value["label"], json!([{ "value": "T" }]));
    }

    // The innermost-first island chain becomes the LSP linked structure: the
    // head is the innermost range and each `parent` widens by one link.
    #[test]
    fn selection_chain_links_innermost_first_into_parents() {
        let chain = [rng(1, 10, 1, 11), rng(1, 2, 1, 13), rng(0, 0, 2, 1)];
        let queried = lsp_types::Position::new(1, 10);
        assert_eq!(
            serde_json::to_value(selection_chain(&chain, &queried)).unwrap(),
            json!({
                "range": { "start": { "line": 1, "character": 10 }, "end": { "line": 1, "character": 11 } },
                "parent": {
                    "range": { "start": { "line": 1, "character": 2 }, "end": { "line": 1, "character": 13 } },
                    "parent": {
                        "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 2, "character": 1 } }
                    }
                }
            })
        );
    }

    // An empty chain keeps the spec's index correspondence: the zero-width
    // range at the queried position, with no parent.
    #[test]
    fn selection_chain_degrades_an_empty_chain_to_the_queried_position() {
        let queried = lsp_types::Position::new(3, 7);
        assert_eq!(
            serde_json::to_value(selection_chain(&[], &queried)).unwrap(),
            json!({
                "range": { "start": { "line": 3, "character": 7 }, "end": { "line": 3, "character": 7 } }
            })
        );
    }

    // Folding kinds map the boundary ordinals to the LSP kind strings; 0
    // ("none") and unknown ordinals omit the field.
    #[test]
    fn folding_range_maps_each_kind_ordinal() {
        let of = |kind: i32| {
            serde_json::to_value(folding_range(&AbiFoldingRange {
                range: rng(0, 5, 3, 1),
                kind,
            }))
            .unwrap()
        };
        assert_eq!(
            of(2),
            json!({
                "startLine": 0,
                "startCharacter": 5,
                "endLine": 3,
                "endCharacter": 1,
                "kind": "imports"
            })
        );
        assert_eq!(of(1)["kind"], "comment");
        assert_eq!(of(3)["kind"], "region");
        assert!(of(0).get("kind").is_none());
        assert!(of(99).get("kind").is_none());
    }
}
