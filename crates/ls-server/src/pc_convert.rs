//! Converters from the flat `#[repr(C)]` PC ABI carriers (`ls-pc-abi`, lossless
//! mirrors of the runtime LSP4J-1.0.0 carriers) to the LSP JSON wire shapes the
//! client expects.
//!
//! The embedded PC island encodes each reply from the LSP4J object metals'
//! presentation compiler produced; the ready path decodes the carrier (in
//! [`crate::pc`]) and renders it here, so a `textDocument/completion`/`hover`/
//! `signatureHelp` answer carries the same LSP object the Scala server forwarded.
//!
//! Optional carrier fields are OMITTED (not emitted as `null`) when absent, and a
//! present-but-empty list stays an empty list — matching LSP4J's null-omitting
//! Gson serialization and preserving the empty-vs-null distinction (an empty
//! completion list vs a null hover). The genuinely opaque JSON fields (a
//! completion item's `data`, a command's `arguments`) are carried as bytes across
//! the boundary and re-parsed here, exactly as they were serialized.

use serde_json::{json, Map, Value};

use ls_pc_abi::payloads::{
    Command, CompletionApplyKind, CompletionEdit, CompletionItem, CompletionItemDefaults,
    CompletionList, Documentation, EditRange, Hover, HoverContents, HoverResult, LabelDetails,
    MarkedStringItem, MarkupContent, ParameterInfo, ParameterLabel, Rng, SignatureHelp,
    SignatureInfo, TextEdit,
};

/// An empty, complete completion list — the `withPcBuffer` fallback and the
/// backend/decode degrade for `textDocument/completion` (Scala `emptyCompletions()`).
pub(crate) fn empty_completions() -> Value {
    json!({ "isIncomplete": false, "items": [] })
}

/// `CompletionList` -> LSP `CompletionList` JSON.
pub(crate) fn completion_list(list: &CompletionList) -> Value {
    let mut obj = Map::new();
    obj.insert("isIncomplete".to_string(), json!(list.is_incomplete));
    if let Some(defaults) = &list.item_defaults {
        obj.insert("itemDefaults".to_string(), item_defaults(defaults));
    }
    if let Some(apply) = &list.apply_kind {
        obj.insert("applyKind".to_string(), apply_kind(apply));
    }
    obj.insert(
        "items".to_string(),
        Value::Array(list.items.iter().map(completion_item).collect()),
    );
    Value::Object(obj)
}

/// `HoverResult` -> LSP `Hover` JSON, or `null` when the presentation compiler
/// had nothing at the point (the nullable-hover distinction).
pub(crate) fn hover_result(result: &HoverResult) -> Value {
    match &result.0 {
        Some(hover) => hover_value(hover),
        None => Value::Null,
    }
}

/// `SignatureHelp` -> LSP `SignatureHelp` JSON.
pub(crate) fn signature_help(help: &SignatureHelp) -> Value {
    let mut obj = Map::new();
    obj.insert(
        "signatures".to_string(),
        Value::Array(help.signatures.iter().map(signature_info).collect()),
    );
    if let Some(active) = help.active_signature {
        obj.insert("activeSignature".to_string(), json!(active));
    }
    if let Some(active) = help.active_parameter {
        obj.insert("activeParameter".to_string(), json!(active));
    }
    Value::Object(obj)
}

// --- completion item ---

fn completion_item(item: &CompletionItem) -> Value {
    let mut obj = Map::new();
    obj.insert("label".to_string(), json!(item.label));
    if let Some(details) = &item.label_details {
        obj.insert("labelDetails".to_string(), label_details(details));
    }
    if let Some(kind) = item.kind {
        obj.insert("kind".to_string(), json!(kind));
    }
    if let Some(tags) = &item.tags {
        obj.insert("tags".to_string(), json!(tags));
    }
    if let Some(detail) = &item.detail {
        obj.insert("detail".to_string(), json!(detail));
    }
    if let Some(doc) = &item.documentation {
        obj.insert("documentation".to_string(), documentation(doc));
    }
    if let Some(deprecated) = item.deprecated {
        obj.insert("deprecated".to_string(), json!(deprecated));
    }
    if let Some(preselect) = item.preselect {
        obj.insert("preselect".to_string(), json!(preselect));
    }
    if let Some(sort_text) = &item.sort_text {
        obj.insert("sortText".to_string(), json!(sort_text));
    }
    if let Some(filter_text) = &item.filter_text {
        obj.insert("filterText".to_string(), json!(filter_text));
    }
    if let Some(insert_text) = &item.insert_text {
        obj.insert("insertText".to_string(), json!(insert_text));
    }
    if let Some(format) = item.insert_text_format {
        obj.insert("insertTextFormat".to_string(), json!(format));
    }
    if let Some(mode) = item.insert_text_mode {
        obj.insert("insertTextMode".to_string(), json!(mode));
    }
    if let Some(edit) = &item.text_edit {
        obj.insert("textEdit".to_string(), completion_edit(edit));
    }
    if let Some(text) = &item.text_edit_text {
        obj.insert("textEditText".to_string(), json!(text));
    }
    if let Some(edits) = &item.additional_text_edits {
        obj.insert(
            "additionalTextEdits".to_string(),
            Value::Array(edits.iter().map(text_edit_value).collect()),
        );
    }
    if let Some(chars) = &item.commit_characters {
        obj.insert("commitCharacters".to_string(), json!(chars));
    }
    if let Some(cmd) = &item.command {
        obj.insert("command".to_string(), command(cmd));
    }
    if let Some(data) = &item.data {
        obj.insert("data".to_string(), opaque_json(data));
    }
    Value::Object(obj)
}

fn label_details(details: &LabelDetails) -> Value {
    let mut obj = Map::new();
    if let Some(detail) = &details.detail {
        obj.insert("detail".to_string(), json!(detail));
    }
    if let Some(description) = &details.description {
        obj.insert("description".to_string(), json!(description));
    }
    Value::Object(obj)
}

fn item_defaults(defaults: &CompletionItemDefaults) -> Value {
    let mut obj = Map::new();
    if let Some(chars) = &defaults.commit_characters {
        obj.insert("commitCharacters".to_string(), json!(chars));
    }
    if let Some(edit_range) = &defaults.edit_range {
        obj.insert("editRange".to_string(), edit_range_value(edit_range));
    }
    if let Some(format) = defaults.insert_text_format {
        obj.insert("insertTextFormat".to_string(), json!(format));
    }
    if let Some(mode) = defaults.insert_text_mode {
        obj.insert("insertTextMode".to_string(), json!(mode));
    }
    if let Some(data) = &defaults.data {
        obj.insert("data".to_string(), opaque_json(data));
    }
    Value::Object(obj)
}

fn edit_range_value(edit_range: &EditRange) -> Value {
    match edit_range {
        EditRange::Range(range) => range_value(range),
        EditRange::InsertReplace { insert, replace } => json!({
            "insert": range_value(insert),
            "replace": range_value(replace),
        }),
    }
}

/// The completion-list `applyKind` merge modes. Each carrier field is the LSP4J
/// `ApplyKind` ordinal, carried verbatim (the current Scala 3 presentation
/// compiler does not populate this LSP4J-1.0.0 field, so this is a faithful
/// pass-through for the rare case a future compiler does).
fn apply_kind(apply: &CompletionApplyKind) -> Value {
    let mut obj = Map::new();
    if let Some(chars) = apply.commit_characters {
        obj.insert("commitCharacters".to_string(), json!(chars));
    }
    if let Some(data) = apply.data {
        obj.insert("data".to_string(), json!(data));
    }
    Value::Object(obj)
}

fn completion_edit(edit: &CompletionEdit) -> Value {
    match edit {
        CompletionEdit::Plain(text_edit) => text_edit_value(text_edit),
        CompletionEdit::InsertReplace(edit) => json!({
            "newText": edit.new_text,
            "insert": range_value(&edit.insert),
            "replace": range_value(&edit.replace),
        }),
    }
}

fn command(cmd: &Command) -> Value {
    let mut obj = Map::new();
    obj.insert("title".to_string(), json!(cmd.title));
    obj.insert("command".to_string(), json!(cmd.command));
    if let Some(tooltip) = &cmd.tooltip {
        obj.insert("tooltip".to_string(), json!(tooltip));
    }
    if let Some(arguments) = &cmd.arguments {
        obj.insert("arguments".to_string(), opaque_json(arguments));
    }
    Value::Object(obj)
}

// --- hover ---

fn hover_value(hover: &Hover) -> Value {
    let mut obj = Map::new();
    obj.insert("contents".to_string(), hover_contents(&hover.contents));
    if let Some(range) = &hover.range {
        obj.insert("range".to_string(), range_value(range));
    }
    Value::Object(obj)
}

fn hover_contents(contents: &HoverContents) -> Value {
    match contents {
        HoverContents::Markup(markup) => markup_content(markup),
        HoverContents::Marked(items) => Value::Array(items.iter().map(marked_string).collect()),
    }
}

fn marked_string(item: &MarkedStringItem) -> Value {
    match item {
        MarkedStringItem::Plain(value) => Value::String(value.clone()),
        MarkedStringItem::Marked { language, value } => json!({
            "language": language,
            "value": value,
        }),
    }
}

// --- signature help ---

fn signature_info(sig: &SignatureInfo) -> Value {
    let mut obj = Map::new();
    obj.insert("label".to_string(), json!(sig.label));
    if let Some(doc) = &sig.documentation {
        obj.insert("documentation".to_string(), documentation(doc));
    }
    if let Some(params) = &sig.parameters {
        obj.insert(
            "parameters".to_string(),
            Value::Array(params.iter().map(parameter_info).collect()),
        );
    }
    if let Some(active) = sig.active_parameter {
        obj.insert("activeParameter".to_string(), json!(active));
    }
    Value::Object(obj)
}

fn parameter_info(param: &ParameterInfo) -> Value {
    let mut obj = Map::new();
    obj.insert("label".to_string(), parameter_label(&param.label));
    if let Some(doc) = &param.documentation {
        obj.insert("documentation".to_string(), documentation(doc));
    }
    Value::Object(obj)
}

fn parameter_label(label: &ParameterLabel) -> Value {
    match label {
        ParameterLabel::Str(value) => Value::String(value.clone()),
        ParameterLabel::Offsets { start, end } => json!([start, end]),
    }
}

// --- shared value types ---

/// A documentation body: LSP4J `Either<String, MarkupContent>` — a bare string or
/// a `{kind, value}` object.
fn documentation(doc: &Documentation) -> Value {
    match doc {
        Documentation::Plain(value) => Value::String(value.clone()),
        Documentation::Markup(markup) => markup_content(markup),
    }
}

fn markup_content(markup: &MarkupContent) -> Value {
    json!({ "kind": markup.kind, "value": markup.value })
}

fn text_edit_value(edit: &TextEdit) -> Value {
    json!({ "range": range_value(&edit.range), "newText": edit.new_text })
}

fn range_value(range: &Rng) -> Value {
    json!({
        "start": { "line": range.start_line, "character": range.start_character },
        "end": { "line": range.end_line, "character": range.end_character },
    })
}

/// An opaque, verbatim-carried JSON blob (a completion item's `data`, a command's
/// `arguments`) re-parsed to the value it was serialized from. A blob that does
/// not parse degrades to `null` rather than surfacing raw bytes.
fn opaque_json(bytes: &[u8]) -> Value {
    serde_json::from_slice(bytes).unwrap_or(Value::Null)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ls_pc_abi::payloads::InsertReplaceEdit;

    fn rng(sl: u32, sc: u32, el: u32, ec: u32) -> Rng {
        Rng {
            start_line: sl,
            start_character: sc,
            end_line: el,
            end_character: ec,
        }
    }

    // A minimal completion item carries only its label; every optional field is
    // omitted (not null), and the list wrapper keeps `isIncomplete`/`items`.
    #[test]
    fn a_minimal_completion_list_omits_every_absent_field() {
        let list = CompletionList {
            is_incomplete: true,
            item_defaults: None,
            apply_kind: None,
            items: vec![CompletionItem {
                label: "foo".to_string(),
                label_details: None,
                kind: None,
                tags: None,
                detail: None,
                documentation: None,
                deprecated: None,
                preselect: None,
                sort_text: None,
                filter_text: None,
                insert_text: None,
                insert_text_format: None,
                insert_text_mode: None,
                text_edit: None,
                text_edit_text: None,
                additional_text_edits: None,
                commit_characters: None,
                command: None,
                data: None,
            }],
        };
        assert_eq!(
            completion_list(&list),
            json!({
                "isIncomplete": true,
                "items": [ { "label": "foo" } ],
            })
        );
    }

    // The rich fields all render with their LSP names: kind/detail, a markup
    // documentation object, an insert-replace text edit, a command with re-parsed
    // opaque arguments, and re-parsed opaque `data`.
    #[test]
    fn a_rich_completion_item_renders_every_field_with_its_lsp_name() {
        let item = CompletionItem {
            label: "map".to_string(),
            label_details: Some(LabelDetails {
                detail: Some("[B]".to_string()),
                description: Some("List[B]".to_string()),
            }),
            kind: Some(2),
            tags: Some(vec![1]),
            detail: Some("def map".to_string()),
            documentation: Some(Documentation::Markup(MarkupContent {
                kind: "markdown".to_string(),
                value: "**doc**".to_string(),
            })),
            deprecated: Some(false),
            preselect: Some(true),
            sort_text: Some("00".to_string()),
            filter_text: Some("map".to_string()),
            insert_text: None,
            insert_text_format: Some(2),
            insert_text_mode: Some(1),
            text_edit: Some(CompletionEdit::InsertReplace(InsertReplaceEdit {
                new_text: "map($0)".to_string(),
                insert: rng(1, 2, 1, 2),
                replace: rng(1, 2, 1, 5),
            })),
            text_edit_text: None,
            additional_text_edits: Some(vec![TextEdit {
                range: rng(0, 0, 0, 0),
                new_text: "import scala.collection\n".to_string(),
            }]),
            commit_characters: Some(vec![".".to_string()]),
            command: Some(Command {
                title: "trigger".to_string(),
                tooltip: None,
                command: "editor.action.triggerSuggest".to_string(),
                arguments: Some(br#"[{"uri":"x"}]"#.to_vec()),
            }),
            data: Some(br#"{"symbol":"scala/Predef.map()."}"#.to_vec()),
        };
        assert_eq!(
            completion_item(&item),
            json!({
                "label": "map",
                "labelDetails": { "detail": "[B]", "description": "List[B]" },
                "kind": 2,
                "tags": [1],
                "detail": "def map",
                "documentation": { "kind": "markdown", "value": "**doc**" },
                "deprecated": false,
                "preselect": true,
                "sortText": "00",
                "filterText": "map",
                "insertTextFormat": 2,
                "insertTextMode": 1,
                "textEdit": {
                    "newText": "map($0)",
                    "insert": { "start": { "line": 1, "character": 2 }, "end": { "line": 1, "character": 2 } },
                    "replace": { "start": { "line": 1, "character": 2 }, "end": { "line": 1, "character": 5 } },
                },
                "additionalTextEdits": [ {
                    "range": { "start": { "line": 0, "character": 0 }, "end": { "line": 0, "character": 0 } },
                    "newText": "import scala.collection\n",
                } ],
                "commitCharacters": ["."],
                "command": {
                    "title": "trigger",
                    "command": "editor.action.triggerSuggest",
                    "arguments": [ { "uri": "x" } ],
                },
                "data": { "symbol": "scala/Predef.map()." },
            })
        );
    }

    // A plain `TextEdit` completion edit renders as a `{range, newText}` object.
    #[test]
    fn a_plain_completion_edit_renders_as_a_text_edit() {
        let item = CompletionItem {
            label: "x".to_string(),
            label_details: None,
            kind: None,
            tags: None,
            detail: None,
            documentation: Some(Documentation::Plain("plain".to_string())),
            deprecated: None,
            preselect: None,
            sort_text: None,
            filter_text: None,
            insert_text: Some("x".to_string()),
            insert_text_format: None,
            insert_text_mode: None,
            text_edit: Some(CompletionEdit::Plain(TextEdit {
                range: rng(3, 4, 3, 5),
                new_text: "xs".to_string(),
            })),
            text_edit_text: Some("xs".to_string()),
            additional_text_edits: None,
            commit_characters: None,
            command: None,
            data: None,
        };
        let value = completion_item(&item);
        assert_eq!(value["documentation"], json!("plain"));
        assert_eq!(value["insertText"], json!("x"));
        assert_eq!(value["textEditText"], json!("xs"));
        assert_eq!(
            value["textEdit"],
            json!({
                "range": { "start": { "line": 3, "character": 4 }, "end": { "line": 3, "character": 5 } },
                "newText": "xs",
            })
        );
    }

    // A null hover (the PC had nothing) is JSON null, distinct from a present
    // hover with empty contents.
    #[test]
    fn a_null_hover_is_json_null() {
        assert_eq!(hover_result(&HoverResult(None)), Value::Null);
    }

    // A markup hover with a range renders contents + range; a marked-string list
    // renders the string/{language,value} either-arms.
    #[test]
    fn a_present_hover_renders_contents_and_optional_range() {
        let markup = HoverResult(Some(Hover {
            contents: HoverContents::Markup(MarkupContent {
                kind: "markdown".to_string(),
                value: "T".to_string(),
            }),
            range: Some(rng(1, 0, 1, 4)),
        }));
        assert_eq!(
            hover_result(&markup),
            json!({
                "contents": { "kind": "markdown", "value": "T" },
                "range": { "start": { "line": 1, "character": 0 }, "end": { "line": 1, "character": 4 } },
            })
        );

        let marked = HoverResult(Some(Hover {
            contents: HoverContents::Marked(vec![
                MarkedStringItem::Plain("plain".to_string()),
                MarkedStringItem::Marked {
                    language: "scala".to_string(),
                    value: "def f".to_string(),
                },
            ]),
            range: None,
        }));
        assert_eq!(
            hover_result(&marked),
            json!({
                "contents": [ "plain", { "language": "scala", "value": "def f" } ],
            })
        );
    }

    // Signature help renders signatures with parameter label either-arms (string
    // and `[start, end]` offsets) and the active indices.
    #[test]
    fn signature_help_renders_labels_docs_and_active_indices() {
        let help = SignatureHelp {
            signatures: vec![SignatureInfo {
                label: "f(a: Int, b: Int)".to_string(),
                documentation: Some(Documentation::Plain("adds".to_string())),
                parameters: Some(vec![
                    ParameterInfo {
                        label: ParameterLabel::Str("a: Int".to_string()),
                        documentation: None,
                    },
                    ParameterInfo {
                        label: ParameterLabel::Offsets { start: 10, end: 16 },
                        documentation: Some(Documentation::Markup(MarkupContent {
                            kind: "plaintext".to_string(),
                            value: "the b".to_string(),
                        })),
                    },
                ]),
                active_parameter: Some(0),
            }],
            active_signature: Some(0),
            active_parameter: Some(1),
        };
        assert_eq!(
            signature_help(&help),
            json!({
                "signatures": [ {
                    "label": "f(a: Int, b: Int)",
                    "documentation": "adds",
                    "parameters": [
                        { "label": "a: Int" },
                        { "label": [10, 16], "documentation": { "kind": "plaintext", "value": "the b" } },
                    ],
                    "activeParameter": 0,
                } ],
                "activeSignature": 0,
                "activeParameter": 1,
            })
        );
    }

    // An empty signature list stays an empty array (present, not null).
    #[test]
    fn empty_signature_help_is_an_empty_signature_array() {
        let help = SignatureHelp {
            signatures: Vec::new(),
            active_signature: None,
            active_parameter: None,
        };
        assert_eq!(signature_help(&help), json!({ "signatures": [] }));
    }

    // A blob that is not valid JSON degrades to null rather than surfacing bytes.
    #[test]
    fn an_unparseable_opaque_blob_degrades_to_null() {
        assert_eq!(opaque_json(&[0xff, 0x00]), Value::Null);
    }
}
