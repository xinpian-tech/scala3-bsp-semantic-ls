//! Bridges the island's decoded payload carriers (`ls_pc_abi::payloads`) to the
//! `lsp-types` protocol shapes for the payload-backed LSP methods
//! (`textDocument/inlayHint`, `textDocument/selectionRange`,
//! `textDocument/foldingRange`, `textDocument/semanticTokens/full` + `/range`).
//!
//! Unlike the hand-rolled serde layer the original 12-method surface uses
//! (`crate::convert`/`crate::pc_convert`), every NEW protocol shape on this
//! edge is an upstream `lsp_types` model — nothing here defines a wire struct.
//! The bridge only maps the flat ABI carriers into those models; the dispatch
//! layer serializes them into the existing `serde_json` response path
//! (`Response::success(id, serde_json::to_value(..))`).
//!
//! It also owns the semantic-tokens [`legend`] contract and the offset→delta
//! encoder ([`semantic_tokens`] / [`semantic_tokens_range`]): the island's
//! nodes carry `[start, end)` UTF-16 offsets into the OPEN BUFFER TEXT plus
//! type/modifier ints that index the PC-vendored legend, and the encoder turns
//! them into the LSP `SemanticTokens` delta stream.

use line_index::{LineIndex, WideEncoding};
use ls_pc_abi::payloads::{
    FoldingRange as AbiFoldingRange, InlayHint as AbiInlayHint, InlayLabelPart, Pos, Rng,
    SemanticNode, TextEdit as AbiTextEdit,
};
use serde_json::Value;

/// The semantic-tokens legend contract: the island's `Node.tokenType()` /
/// `tokenModifier()` ints are INDICES into the PC-vendored
/// `scala.meta.internal.pc.SemanticTokens.TokenTypes` / `TokenModifiers` lists
/// (scala3-presentation-compiler 3.8.4, vendored in the island's PC jar), so
/// the legend the server advertises must be EXACTLY those lists, in order.
/// Pinned here as Rust constants; the island-side munit suite
/// `modules/ls-pc/test/src/ls/pc/SemanticTokensLegendSuite.scala` pins the same
/// lists (and the same golden anchors) against the vendored object itself, so
/// legend drift on either side of the boundary breaks a build.
pub mod legend {
    /// `scala.meta.internal.pc.SemanticTokens.TokenTypes`, verbatim and in
    /// order (23 entries — the LSP 3.17 standard token types minus none).
    pub const TOKEN_TYPES: [&str; 23] = [
        "namespace",
        "type",
        "class",
        "enum",
        "interface",
        "struct",
        "typeParameter",
        "parameter",
        "variable",
        "property",
        "enumMember",
        "event",
        "function",
        "method",
        "macro",
        "keyword",
        "modifier",
        "comment",
        "string",
        "number",
        "regexp",
        "operator",
        "decorator",
    ];

    /// `scala.meta.internal.pc.SemanticTokens.TokenModifiers`, verbatim and in
    /// order (10 entries).
    pub const TOKEN_MODIFIERS: [&str; 10] = [
        "declaration",
        "definition",
        "readonly",
        "static",
        "deprecated",
        "abstract",
        "async",
        "modification",
        "documentation",
        "defaultLibrary",
    ];

    /// Golden anchor, pinned on BOTH sides of the boundary: `"method"` is
    /// token-type index 13.
    pub const METHOD_TYPE_INDEX: usize = 13;

    /// Golden anchor, pinned on BOTH sides of the boundary: `"declaration"` is
    /// token-modifier bit 0.
    pub const DECLARATION_MODIFIER_INDEX: usize = 0;
}

/// The UTF-16 line-start table of a buffer: `starts[i]` is the UTF-16 offset
/// of line `i`'s first unit, with one trailing sentinel at the total UTF-16
/// length. Built over `line-index`'s line split (so `\n`/`\r\n` terminator
/// handling is the same as the document store's), it maps the island's global
/// UTF-16 node offsets to `(line, UTF-16 column)` by binary search.
struct Utf16Lines {
    starts: Vec<u32>,
}

impl Utf16Lines {
    fn new(text: &str) -> Utf16Lines {
        let index = LineIndex::new(text);
        let mut starts = vec![0u32];
        let mut total = 0u32;
        let mut line = 0u32;
        while let Some(range) = index.line(line) {
            let slice = &text[usize::from(range.start())..usize::from(range.end())];
            total += WideEncoding::Utf16.measure(slice) as u32;
            starts.push(total);
            line += 1;
        }
        Utf16Lines { starts }
    }

    fn total(&self) -> u32 {
        *self.starts.last().expect("at least the sentinel")
    }

    /// `(line, UTF-16 column)` of a global UTF-16 offset, clamped into the
    /// text (an out-of-range offset lands at the end of the last line).
    fn position(&self, offset: u32) -> (u32, u32) {
        let offset = offset.min(self.total());
        let line = match self.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
        // The total-length sentinel (and an offset exactly at it) belongs to
        // the last real line.
        .min(self.starts.len().saturating_sub(2));
        (line as u32, offset - self.starts[line])
    }
}

/// One positioned token: absolute `(line, UTF-16 start column)`, UTF-16
/// length, and the legend type/modifier ints.
struct PositionedToken {
    line: u32,
    start: u32,
    length: u32,
    token_type: u32,
    modifiers: u32,
}

/// Positions the island's offset nodes against the open buffer text, dropping
/// the nodes the LSP stream cannot (or must not) carry:
/// - `token_type < 0`: the dotty provider's "no classification" sentinel
///   (`makeNode`'s `else -1` branch) — the node names a symbol the legend has
///   no type for, so it is dropped exactly as Metals' encoder drops it;
/// - `start >= end`: a zero-width node highlights nothing;
/// - a node whose end lands on a later line: dotty nodes are symbol-name spans
///   and are single-line by construction (pinned by test); a multi-line node
///   would need the multiline-token client capability, so a hypothetical one
///   is dropped rather than emitted corrupt.
fn positioned(nodes: &[SemanticNode], lines: &Utf16Lines) -> Vec<PositionedToken> {
    let mut tokens: Vec<PositionedToken> = nodes
        .iter()
        .filter(|node| node.token_type >= 0 && node.start < node.end)
        .filter_map(|node| {
            let (line, start) = lines.position(node.start);
            let (end_line, end_col) = lines.position(node.end);
            if end_line != line {
                return None;
            }
            Some(PositionedToken {
                line,
                start,
                length: end_col - start,
                token_type: node.token_type as u32,
                modifiers: u32::try_from(node.token_modifier).unwrap_or(0),
            })
        })
        .collect();
    // The provider already sorts by (start, end); re-sort defensively — the
    // delta encoding is only valid over a position-sorted stream.
    tokens.sort_by_key(|t| (t.line, t.start, t.length));
    tokens
}

/// Delta-encodes positioned tokens into the LSP `SemanticTokens` data stream
/// (`deltaLine`/`deltaStart`/`length`/`tokenType`/`tokenModifiers`, five words
/// per token). No `resultId` here: the dispatch layer stamps one onto the
/// `/full` (and `/full/delta`) responses when it caches the stream — `/range`
/// responses stay id-less (a range slice is never a delta base).
fn delta_encode(tokens: &[PositionedToken]) -> lsp_types::SemanticTokens {
    let mut data = Vec::with_capacity(tokens.len());
    let (mut prev_line, mut prev_start) = (0u32, 0u32);
    for token in tokens {
        let delta_line = token.line - prev_line;
        let delta_start = if delta_line == 0 {
            token.start - prev_start
        } else {
            token.start
        };
        data.push(lsp_types::SemanticToken {
            delta_line,
            delta_start,
            length: token.length,
            token_type: token.token_type,
            token_modifiers_bitset: token.modifiers,
        });
        prev_line = token.line;
        prev_start = token.start;
    }
    lsp_types::SemanticTokens {
        result_id: None,
        data,
    }
}

/// The island's offset nodes for the whole buffer -> the LSP `SemanticTokens`
/// delta stream, positioned against `text` (the OPEN BUFFER TEXT the island
/// mirrored — offsets and columns are UTF-16 units, the advertised encoding).
pub fn semantic_tokens(nodes: &[SemanticNode], text: &str) -> lsp_types::SemanticTokens {
    let lines = Utf16Lines::new(text);
    delta_encode(&positioned(nodes, &lines))
}

/// The `/range` variant: the node list is sliced server-side to the tokens
/// overlapping `range` (a token strictly before or after the range is
/// dropped; one straddling a range edge is kept whole) BEFORE the delta
/// encoding, so the first emitted token's deltas are absolute from the
/// document origin, as the spec requires of every `SemanticTokens` stream.
pub fn semantic_tokens_range(
    nodes: &[SemanticNode],
    text: &str,
    range: &lsp_types::Range,
) -> lsp_types::SemanticTokens {
    let lines = Utf16Lines::new(text);
    let from = (range.start.line, range.start.character);
    let to = (range.end.line, range.end.character);
    let mut tokens = positioned(nodes, &lines);
    tokens.retain(|token| {
        (token.line, token.start) < to && (token.line, token.start + token.length) > from
    });
    delta_encode(&tokens)
}

/// The `full/delta` edit list between two encoded token streams: the single
/// minimal splice of the standard LSP delta algorithm (the same prefix/suffix
/// diff rust-analyzer's `semantic_tokens::diff_tokens` computes). The common
/// prefix, then the common suffix of the remainders, are counted in whole
/// five-word tokens — so the spliced `data` stays token-aligned — while the
/// wire `start`/`deleteCount` are in raw u32 units of the encoded stream (×5),
/// as the spec requires. Identical streams answer the empty edit list; a pure
/// deletion carries an empty `data` (never a missing one, so the client-side
/// splice arithmetic stays uniform).
pub fn semantic_tokens_edits(
    prev: &[lsp_types::SemanticToken],
    next: &[lsp_types::SemanticToken],
) -> Vec<lsp_types::SemanticTokensEdit> {
    let prefix = prev
        .iter()
        .zip(next.iter())
        .take_while(|(p, n)| p == n)
        .count();
    let (prev_rest, next_rest) = (&prev[prefix..], &next[prefix..]);
    // Over the remainders only, so an element common to both prefix and suffix
    // is never double-counted (the splice bounds cannot overlap).
    let suffix = prev_rest
        .iter()
        .rev()
        .zip(next_rest.iter().rev())
        .take_while(|(p, n)| p == n)
        .count();
    let deleted = prev_rest.len() - suffix;
    let inserted = &next_rest[..next_rest.len() - suffix];
    if deleted == 0 && inserted.is_empty() {
        return Vec::new();
    }
    vec![lsp_types::SemanticTokensEdit {
        start: 5 * prefix as u32,
        delete_count: 5 * deleted as u32,
        data: Some(inserted.to_vec()),
    }]
}

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

/// Island text edits for one buffer -> an inline `lsp_types::WorkspaceEdit`
/// (`changes: { uri: edits }`). The code-action assembly attaches this edit
/// directly on each literal action — there is no `workspace/executeCommand`
/// round trip and no `codeAction/resolve` — so a returned action is complete
/// the moment the client receives it. A URI that does not parse as an
/// `lsp_types::Uri` yields `None` (the action is dropped rather than emitted
/// with a malformed target; the server only feeds `file://` URIs here, so this
/// is a defensive boundary).
///
/// The edits are sorted by (start, end) before emission: the LSP array-order
/// convention allows a zero-width insert and a replacement at the SAME start
/// only with the insert first, and the island's extract-method op produces
/// exactly that tied-start pair (the new-method insert at the selection's own
/// statement plus the selection replacement), in provider order the spec does
/// not accept.
// The `changes` key is the upstream `lsp_types::Uri` (a `fluent-uri` with an
// internal meta cache Cell); the map is built once and serialized, never
// mutated through its keys, so clippy's mutable-key lint is a false positive
// here.
#[allow(clippy::mutable_key_type)]
pub fn workspace_edit(uri: &str, edits: &[AbiTextEdit]) -> Option<lsp_types::WorkspaceEdit> {
    let uri: lsp_types::Uri = uri.parse().ok()?;
    let mut sorted: Vec<lsp_types::TextEdit> = edits.iter().map(text_edit).collect();
    sorted.sort_by_key(|edit| {
        (
            edit.range.start.line,
            edit.range.start.character,
            edit.range.end.line,
            edit.range.end.character,
        )
    });
    let mut changes = std::collections::HashMap::new();
    changes.insert(uri, sorted);
    Some(lsp_types::WorkspaceEdit {
        changes: Some(changes),
        document_changes: None,
        change_annotations: None,
    })
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

    // --- the semantic-tokens legend contract ---------------------------------

    // The legend is EXACTLY the PC-vendored
    // `scala.meta.internal.pc.SemanticTokens` lists (scala3-presentation-
    // compiler 3.8.4), pinned verbatim; the island-side
    // SemanticTokensLegendSuite pins the same lists from the vendored object,
    // so drift breaks both sides.
    #[test]
    fn the_legend_is_the_pc_vendored_token_lists() {
        assert_eq!(
            legend::TOKEN_TYPES,
            [
                "namespace",
                "type",
                "class",
                "enum",
                "interface",
                "struct",
                "typeParameter",
                "parameter",
                "variable",
                "property",
                "enumMember",
                "event",
                "function",
                "method",
                "macro",
                "keyword",
                "modifier",
                "comment",
                "string",
                "number",
                "regexp",
                "operator",
                "decorator",
            ]
        );
        assert_eq!(
            legend::TOKEN_MODIFIERS,
            [
                "declaration",
                "definition",
                "readonly",
                "static",
                "deprecated",
                "abstract",
                "async",
                "modification",
                "documentation",
                "defaultLibrary",
            ]
        );
    }

    // The golden anchors, shared verbatim with the island-side parity suite.
    #[test]
    fn the_legend_golden_anchors_hold() {
        assert_eq!(legend::TOKEN_TYPES[legend::METHOD_TYPE_INDEX], "method");
        assert_eq!(
            legend::TOKEN_MODIFIERS[legend::DECLARATION_MODIFIER_INDEX],
            "declaration"
        );
    }

    // --- the offset -> delta encoder -----------------------------------------

    fn node(start: u32, end: u32, token_type: i32, token_modifier: i32) -> SemanticNode {
        SemanticNode {
            start,
            end,
            token_type,
            token_modifier,
        }
    }

    /// The raw five-words-per-token integer stream of an encoded result (the
    /// exact array the wire carries).
    fn data_of(tokens: &lsp_types::SemanticTokens) -> Vec<u32> {
        serde_json::to_value(tokens).unwrap()["data"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u32)
            .collect()
    }

    // Multi-token, multi-line delta encoding over plain ASCII: absolute first
    // token, same-line column delta, cross-line line delta with an absolute
    // column restart. The pure encoder emits no resultId — the dispatch layer
    // stamps one when it caches a `/full` stream.
    #[test]
    fn semantic_tokens_delta_encode_across_lines() {
        //          0123456789012345
        let text = "class A:\n  def m = 1\n";
        // "class"=[0,5) keyword; "A"=[6,7) class+definition(bit1);
        // "m"=[15,16) method+definition.
        let tokens = semantic_tokens(
            &[node(0, 5, 15, 0), node(6, 7, 2, 2), node(15, 16, 13, 2)],
            text,
        );
        assert_eq!(
            data_of(&tokens),
            vec![
                0, 0, 5, 15, 0, // line 0 col 0 "class"
                0, 6, 1, 2, 2, // same line, +6 cols, "A"
                1, 6, 1, 13, 2, // next line, absolute col 6, "m"
            ]
        );
        let value = serde_json::to_value(&tokens).unwrap();
        assert!(value.get("resultId").is_none(), "{value}");
    }

    // Astral characters occupy TWO UTF-16 units: the island's offsets already
    // count them as two, and the encoder's columns/lengths must stay in the
    // same UTF-16 space (the advertised `positionEncoding`).
    #[test]
    fn semantic_tokens_columns_count_utf16_units_around_astral_chars() {
        // "𐐀" (U+10400) is 2 UTF-16 units: `val` starts the line, the name
        // "a𐐀b" spans units [6, 10) on line 1.
        let text = "// 😀\nval a\u{10400}b = 1\n";
        let tokens = semantic_tokens(&[node(10, 14, 8, 4)], text);
        assert_eq!(data_of(&tokens), vec![1, 4, 4, 8, 4]);
    }

    // The drop rules: a `-1` unclassified node (dotty's `makeNode` fallthrough),
    // a zero-width node, and a hypothetical multi-line node are all dropped —
    // never emitted corrupt. Dotty nodes are symbol-NAME spans, single-line by
    // construction; this pins the defensive boundary for anything else.
    #[test]
    fn semantic_tokens_drop_unclassified_empty_and_multiline_nodes() {
        let text = "val a = 1\nval b = 2\n";
        let tokens = semantic_tokens(
            &[
                node(4, 5, -1, 0), // unclassified: dropped
                node(4, 4, 8, 0),  // zero-width: dropped
                node(4, 15, 8, 0), // spans the line break: dropped
                node(14, 15, 8, 4),
            ],
            text,
        );
        assert_eq!(data_of(&tokens), vec![1, 4, 1, 8, 4]);
    }

    // The encoder re-sorts defensively: an out-of-order node list (the ABI does
    // not guarantee provider order) still yields a valid, monotone delta stream.
    #[test]
    fn semantic_tokens_sort_nodes_before_encoding() {
        let text = "a b\n";
        let tokens = semantic_tokens(&[node(2, 3, 8, 0), node(0, 1, 8, 2)], text);
        assert_eq!(data_of(&tokens), vec![0, 0, 1, 8, 2, 0, 2, 1, 8, 0]);
    }

    // Offsets past the buffer end clamp to the last line's end instead of
    // panicking (a stale node against a raced edit is truncated, not fatal).
    #[test]
    fn semantic_tokens_clamp_offsets_past_the_buffer_end() {
        let text = "ab";
        let tokens = semantic_tokens(&[node(1, 99, 8, 0)], text);
        assert_eq!(data_of(&tokens), vec![0, 1, 1, 8, 0]);
    }

    // CRLF line breaks: the terminator is part of the line's UTF-16 width for
    // offset accounting, and columns stay relative to each line start.
    #[test]
    fn semantic_tokens_position_across_crlf_terminators() {
        let text = "ab\r\ncd\r\n";
        let tokens = semantic_tokens(&[node(0, 2, 8, 0), node(4, 6, 8, 0)], text);
        assert_eq!(data_of(&tokens), vec![0, 0, 2, 8, 0, 1, 0, 2, 8, 0]);
    }

    // `/range` slices server-side BEFORE encoding: tokens strictly outside the
    // range drop, one straddling an edge is kept whole, and the first kept
    // token's deltas are absolute from the document origin.
    #[test]
    fn semantic_tokens_range_slices_overlapping_tokens_and_restarts_deltas() {
        let text = "aa bb\ncc dd\nee ff\n";
        let nodes = [
            node(0, 2, 8, 0),   // line 0 "aa"
            node(3, 5, 8, 0),   // line 0 "bb" — straddles the range start
            node(6, 8, 13, 0),  // line 1 "cc"
            node(12, 14, 8, 0), // line 2 "ee" — starts exactly at range end
        ];
        let range = lsp_types::Range::new(
            lsp_types::Position::new(0, 4),
            lsp_types::Position::new(2, 0),
        );
        let tokens = semantic_tokens_range(&nodes, text, &range);
        assert_eq!(
            data_of(&tokens),
            vec![
                0, 3, 2, 8, 0, // "bb": absolute (0, 3) — straddling token kept whole
                1, 0, 2, 13, 0, // "cc"
            ]
        );
    }

    // An empty node list (a cold island's degrade, or a genuinely token-free
    // buffer) encodes to the empty stream — a valid SemanticTokens, not null.
    #[test]
    fn semantic_tokens_of_no_nodes_is_the_empty_stream() {
        let tokens = semantic_tokens(&[], "val a = 1\n");
        assert_eq!(data_of(&tokens), Vec::<u32>::new());
    }

    // --- the full/delta prefix/suffix diff -----------------------------------

    /// A distinct five-word token: `length` carries `tag` so tokens compare
    /// unequal exactly when their tags differ.
    fn tok(tag: u32) -> lsp_types::SemanticToken {
        lsp_types::SemanticToken {
            delta_line: 1,
            delta_start: 0,
            length: tag,
            token_type: 8,
            token_modifiers_bitset: 0,
        }
    }

    /// The single splice (or none) between two tagged streams, as
    /// `(start, deleteCount, inserted tags)`.
    fn splice_of(prev: &[u32], next: &[u32]) -> Option<(u32, u32, Vec<u32>)> {
        let prev: Vec<_> = prev.iter().copied().map(tok).collect();
        let next: Vec<_> = next.iter().copied().map(tok).collect();
        let edits = semantic_tokens_edits(&prev, &next);
        assert!(edits.len() <= 1, "at most one splice: {edits:?}");
        edits.first().map(|edit| {
            (
                edit.start,
                edit.delete_count,
                edit.data
                    .as_ref()
                    .expect("data is always Some")
                    .iter()
                    .map(|t| t.length)
                    .collect(),
            )
        })
    }

    // Identical streams (both empty and non-empty) diff to NO edits — the
    // client keeps its stream as-is. The empty→empty case is exactly what a
    // cold-island blackbox round trip produces.
    #[test]
    fn tokens_diff_of_identical_streams_is_empty() {
        assert_eq!(splice_of(&[], &[]), None);
        assert_eq!(splice_of(&[1, 2, 3], &[1, 2, 3]), None);
    }

    // A mid-stream change splices exactly the changed window: the common
    // prefix and suffix are held, start/deleteCount are in raw u32 units (×5).
    #[test]
    fn tokens_diff_splices_a_mid_stream_change() {
        assert_eq!(
            splice_of(&[1, 2, 3, 4], &[1, 9, 9, 4]),
            Some((5, 10, vec![9, 9]))
        );
    }

    // Pure insertion (append and prepend): nothing deleted, the new tokens
    // carried in `data` at the splice point.
    #[test]
    fn tokens_diff_splices_pure_insertions() {
        assert_eq!(splice_of(&[1, 2], &[1, 2, 3]), Some((10, 0, vec![3])));
        assert_eq!(splice_of(&[1, 2], &[0, 1, 2]), Some((0, 0, vec![0])));
        assert_eq!(splice_of(&[], &[7]), Some((0, 0, vec![7])));
    }

    // Pure deletion: `data` is Some([]) — present but empty — so the client
    // splice stays uniform (rust-analyzer emits the same shape).
    #[test]
    fn tokens_diff_splices_pure_deletions() {
        assert_eq!(splice_of(&[1, 2, 3], &[1, 3]), Some((5, 5, vec![])));
        assert_eq!(splice_of(&[1, 2], &[]), Some((0, 10, vec![])));
    }

    // Repeated tokens: the prefix is consumed greedily and the suffix is
    // counted over the REMAINDERS only, so a token shared by both never
    // double-counts (prev [A] -> next [A, A] inserts one A after the prefix,
    // not zero or two).
    #[test]
    fn tokens_diff_never_overlaps_prefix_and_suffix() {
        assert_eq!(splice_of(&[1], &[1, 1]), Some((5, 0, vec![1])));
        assert_eq!(splice_of(&[1, 1], &[1]), Some((5, 5, vec![])));
    }

    // A full replacement spans the whole streams.
    #[test]
    fn tokens_diff_replaces_disjoint_streams_whole() {
        assert_eq!(splice_of(&[1, 2], &[3, 4, 5]), Some((0, 10, vec![3, 4, 5])));
    }

    // The wire shape of an edit: camelCase `deleteCount`, `data` flattened to
    // the raw five-words-per-token integer array (the same lsp-types
    // serializer the /full stream uses).
    #[test]
    fn tokens_diff_edit_serializes_to_the_flat_wire_shape() {
        let edits = semantic_tokens_edits(&[tok(1)], &[tok(2)]);
        assert_eq!(
            serde_json::to_value(&edits).unwrap(),
            json!([{ "start": 0, "deleteCount": 5, "data": [1, 0, 2, 8, 0] }])
        );
    }

    // The code-action assembly's inline edit: one buffer's island edits become
    // a `changes`-keyed WorkspaceEdit (never documentChanges — the server
    // advertises no resource operations), serialized as the exact LSP shape.
    #[test]
    fn workspace_edit_wraps_the_buffer_edits_under_changes() {
        let edits = [AbiTextEdit {
            range: rng(1, 4, 1, 4),
            new_text: ": Int".to_string(),
        }];
        let edit = workspace_edit("file:///ws/A.scala", &edits).expect("a parseable uri");
        assert_eq!(
            serde_json::to_value(edit).unwrap(),
            json!({
                "changes": {
                    "file:///ws/A.scala": [{
                        "range": { "start": { "line": 1, "character": 4 }, "end": { "line": 1, "character": 4 } },
                        "newText": ": Int"
                    }]
                }
            })
        );
    }

    // A URI that does not parse yields None (the assembly drops the action
    // instead of emitting a malformed edit target).
    #[test]
    fn workspace_edit_refuses_an_unparseable_uri() {
        assert!(workspace_edit("not a uri", &[]).is_none());
    }

    // The extract-method tied-start pair in provider order (replacement first,
    // zero-width insert second) is re-sorted to the LSP array-order rule: the
    // insert precedes the replacement at the same start position.
    #[test]
    #[allow(clippy::mutable_key_type)] // lsp_types::Uri key, read-only map
    fn workspace_edit_orders_a_tied_start_insert_before_the_replacement() {
        let edits = [
            AbiTextEdit {
                range: rng(3, 10, 3, 25),
                new_text: "newMethod()".to_string(),
            },
            AbiTextEdit {
                range: rng(3, 10, 3, 10),
                new_text: "def newMethod(): Int = ...\n  ".to_string(),
            },
        ];
        let edit = workspace_edit("file:///ws/A.scala", &edits).unwrap();
        let changes = edit.changes.unwrap();
        let sorted = &changes[&"file:///ws/A.scala".parse::<lsp_types::Uri>().unwrap()];
        assert!(sorted[0].new_text.starts_with("def newMethod"));
        assert_eq!(sorted[1].new_text, "newMethod()");
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
