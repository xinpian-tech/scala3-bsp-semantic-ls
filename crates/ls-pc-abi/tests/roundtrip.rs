//! Every op payload round-trips through its `encode`/`decode` pair without loss,
//! including the full LSP4J carrier surface (label details, tags, deprecated/
//! preselect, documentation variants, insert/replace edits, command, item
//! defaults, hover contents variants, signature parameter label offsets), the
//! nullable-vs-empty distinctions, and the definition origin tags.

use ls_pc_abi::payloads::{
    code_action_id, folding_kind, origin, AutoImport, AutoImportParams, AutoImportsResult,
    CodeActionParams, CodeActionResult, Command, CompilerPlugin, CompletionApplyKind,
    CompletionEdit, CompletionItem, CompletionItemDefaults, CompletionList, DefinitionResult,
    DidChangeParams, DidOpenParams, DisabledPlugin, Documentation, EditRange, FoldingRange,
    FoldingRangesResult, Hover, HoverContents, HoverResult, InlayHint, InlayHintParams,
    InlayHintsResult, InlayLabelPart, InsertReplaceEdit, LabelDetails, Location, LocationsResult,
    MarkedStringItem, MarkupContent, MethodHit, MethodHitsResult, ParameterInfo, ParameterLabel,
    PcDiagnostic, PcDiagnosticsResult, PluginStatus, Pos, PositionParams, PrepareRenameResult,
    ResolveParams, Rng, SelectionRangeParams, SelectionRangesResult, SemanticNode,
    SemanticTokensResult, ServicePlugin, SignatureHelp, SignatureInfo, TargetConfig, TextEdit,
    ToplevelsResult, UriParams,
};
use proptest::prelude::*;

/// A completion item with only its required `label` set (everything else null).
fn bare_item(label: &str) -> CompletionItem {
    CompletionItem {
        label: label.to_string(),
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
    }
}

fn range(a: u32, b: u32, c: u32, d: u32) -> Rng {
    Rng {
        start_line: a,
        start_character: b,
        end_line: c,
        end_character: d,
    }
}

// ---------------------------------------------------------------------------
// Carrier parity: the previously-dropped LSP4J fields survive a round trip.
// ---------------------------------------------------------------------------

#[test]
fn completion_item_full_lsp4j_surface_round_trips() {
    let item = CompletionItem {
        label: "map".to_string(),
        label_details: Some(LabelDetails {
            detail: Some("[B](f: A => B)".to_string()),
            description: Some("scala.collection".to_string()),
        }),
        kind: Some(2),
        tags: Some(vec![1]),
        detail: Some("def map[B](f: A => B): List[B]".to_string()),
        documentation: Some(Documentation::Markup(MarkupContent {
            kind: "markdown".to_string(),
            value: "**maps** the list".to_string(),
        })),
        deprecated: Some(true),
        preselect: Some(false),
        sort_text: Some("00".to_string()),
        filter_text: Some("map".to_string()),
        insert_text: None,
        insert_text_format: Some(2),
        insert_text_mode: Some(1),
        text_edit: Some(CompletionEdit::InsertReplace(InsertReplaceEdit {
            new_text: "map($0)".to_string(),
            insert: range(0, 0, 0, 3),
            replace: range(0, 0, 0, 5),
        })),
        text_edit_text: Some("map".to_string()),
        additional_text_edits: Some(vec![TextEdit {
            range: range(1, 0, 1, 0),
            new_text: "import scala.collection\n".to_string(),
        }]),
        commit_characters: Some(vec![".".to_string()]),
        command: Some(Command {
            title: "trigger".to_string(),
            tooltip: Some("re-trigger suggestions".to_string()),
            command: "editor.action.triggerSuggest".to_string(),
            arguments: Some(br#"[{"uri":"file:///a"}]"#.to_vec()),
        }),
        data: Some(br#"{"symbol":"scala/collection/List#map()."}"#.to_vec()),
    };
    assert_eq!(
        CompletionItem::decode(&item.encode().unwrap()).unwrap(),
        item
    );
}

#[test]
fn runtime_lsp4j_1_0_0_fields_round_trip() {
    // Fields present on the evicted runtime lsp4j 1.0.0 carriers but absent
    // from the declared 0.24.0 surface: CompletionItem.textEditText,
    // Command.tooltip, and CompletionList.applyKind.
    let mut item = bare_item("f");
    item.text_edit_text = Some("insert-as-snippet".to_string());
    item.command = Some(Command {
        title: "cmd".to_string(),
        tooltip: Some("hover tip".to_string()),
        command: "run".to_string(),
        arguments: None,
    });
    assert_eq!(
        CompletionItem::decode(&item.encode().unwrap()).unwrap(),
        item
    );

    let list = CompletionList {
        is_incomplete: false,
        item_defaults: None,
        apply_kind: Some(CompletionApplyKind {
            commit_characters: Some(1), // Replace
            data: Some(2),              // Merge
        }),
        items: vec![item],
    };
    assert_eq!(
        CompletionList::decode(&list.encode().unwrap()).unwrap(),
        list
    );

    // applyKind present-but-empty (both merge modes null) is distinct from a
    // null applyKind, and a null command tooltip is distinct from Some("").
    let empty_kind = CompletionList {
        apply_kind: Some(CompletionApplyKind {
            commit_characters: None,
            data: None,
        }),
        ..list.clone()
    };
    let no_kind = CompletionList {
        apply_kind: None,
        ..list
    };
    assert_eq!(
        CompletionList::decode(&empty_kind.encode().unwrap()).unwrap(),
        empty_kind
    );
    assert_ne!(empty_kind.encode().unwrap(), no_kind.encode().unwrap());
}

#[test]
fn completion_edit_plain_variant_round_trips() {
    let mut item = bare_item("f");
    item.text_edit = Some(CompletionEdit::Plain(TextEdit {
        range: range(0, 0, 0, 1),
        new_text: "f()".to_string(),
    }));
    assert_eq!(
        CompletionItem::decode(&item.encode().unwrap()).unwrap(),
        item
    );
}

#[test]
fn completion_list_item_defaults_round_trip() {
    let list = CompletionList {
        is_incomplete: true,
        item_defaults: Some(CompletionItemDefaults {
            commit_characters: Some(vec![".".to_string(), "(".to_string()]),
            edit_range: Some(EditRange::InsertReplace {
                insert: range(0, 0, 0, 2),
                replace: range(0, 0, 0, 4),
            }),
            insert_text_format: Some(2),
            insert_text_mode: None,
            data: Some(b"defaults".to_vec()),
        }),
        apply_kind: None,
        items: vec![bare_item("a")],
    };
    assert_eq!(
        CompletionList::decode(&list.encode().unwrap()).unwrap(),
        list
    );

    // editRange as a plain Range is a distinct variant.
    let mut plain = list.clone();
    plain.item_defaults.as_mut().unwrap().edit_range = Some(EditRange::Range(range(0, 0, 0, 3)));
    assert_eq!(
        CompletionList::decode(&plain.encode().unwrap()).unwrap(),
        plain
    );
}

#[test]
fn hover_marked_string_contents_round_trips() {
    // The MarkedString-list contents variant (not just MarkupContent).
    let hover = HoverResult(Some(Hover {
        contents: HoverContents::Marked(vec![
            MarkedStringItem::Plain("plain".to_string()),
            MarkedStringItem::Marked {
                language: "scala".to_string(),
                value: "def f: Int".to_string(),
            },
        ]),
        range: Some(range(2, 1, 2, 4)),
    }));
    assert_eq!(
        HoverResult::decode(&hover.encode().unwrap()).unwrap(),
        hover
    );
}

#[test]
fn signature_parameter_label_offsets_round_trip() {
    let help = SignatureHelp {
        signatures: vec![SignatureInfo {
            label: "f(x: Int, y: Int): Int".to_string(),
            documentation: Some(Documentation::Plain("adds".to_string())),
            parameters: Some(vec![
                ParameterInfo {
                    label: ParameterLabel::Offsets { start: 2, end: 8 },
                    documentation: Some(Documentation::Markup(MarkupContent {
                        kind: "markdown".to_string(),
                        value: "the x".to_string(),
                    })),
                },
                ParameterInfo {
                    label: ParameterLabel::Str("y: Int".to_string()),
                    documentation: None,
                },
            ]),
            active_parameter: Some(0),
        }],
        active_signature: Some(0),
        active_parameter: Some(1),
    };
    assert_eq!(
        SignatureHelp::decode(&help.encode().unwrap()).unwrap(),
        help
    );
}

// ---------------------------------------------------------------------------
// Nullable-vs-empty and origin edge cases.
// ---------------------------------------------------------------------------

#[test]
fn hover_null_is_distinct_from_present_empty() {
    let null = HoverResult(None);
    assert_eq!(HoverResult::decode(&null.encode().unwrap()).unwrap(), null);

    let present_empty = HoverResult(Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: "plaintext".to_string(),
            value: String::new(),
        }),
        range: None,
    }));
    assert_eq!(
        HoverResult::decode(&present_empty.encode().unwrap()).unwrap(),
        present_empty
    );
    assert_ne!(null.encode().unwrap(), present_empty.encode().unwrap());
}

#[test]
fn prepare_rename_null_is_distinct_from_zero_range() {
    let null = PrepareRenameResult(None);
    let zero = PrepareRenameResult(Some(Rng::default()));
    assert_eq!(
        PrepareRenameResult::decode(&null.encode().unwrap()).unwrap(),
        null
    );
    assert_eq!(
        PrepareRenameResult::decode(&zero.encode().unwrap()).unwrap(),
        zero
    );
    assert_ne!(null.encode().unwrap(), zero.encode().unwrap());
}

#[test]
fn optional_scalars_distinguish_none_from_zero_and_false() {
    let mut some = bare_item("x");
    some.kind = Some(0);
    some.deprecated = Some(false);
    some.preselect = Some(false);
    let none = bare_item("x");
    assert_eq!(
        CompletionItem::decode(&some.encode().unwrap()).unwrap(),
        some
    );
    assert_eq!(
        CompletionItem::decode(&none.encode().unwrap()).unwrap(),
        none
    );
    assert_ne!(some.encode().unwrap(), none.encode().unwrap());
}

#[test]
fn nullable_list_distinguishes_none_from_empty() {
    let mut none = bare_item("x");
    none.commit_characters = None;
    none.tags = None;
    let mut empty = bare_item("x");
    empty.commit_characters = Some(vec![]);
    empty.tags = Some(vec![]);
    assert_eq!(
        CompletionItem::decode(&none.encode().unwrap()).unwrap(),
        none
    );
    assert_eq!(
        CompletionItem::decode(&empty.encode().unwrap()).unwrap(),
        empty
    );
    assert_ne!(none.encode().unwrap(), empty.encode().unwrap());
}

#[test]
fn empty_completion_list_round_trips() {
    let empty = CompletionList {
        is_incomplete: false,
        item_defaults: None,
        apply_kind: None,
        items: vec![],
    };
    let decoded = CompletionList::decode(&empty.encode().unwrap()).unwrap();
    assert_eq!(decoded, empty);
    assert!(decoded.items.is_empty());
}

#[test]
fn definition_preserves_per_location_origin_tags() {
    let result = DefinitionResult {
        symbol: "com/example/Foo#bar().".to_string(),
        locations: vec![
            Location {
                uri: "file:///a.scala".to_string(),
                range: range(3, 6, 3, 9),
                origin: origin::WORKSPACE,
            },
            Location {
                uri: "file:///gen/b.scala".to_string(),
                range: Rng::default(),
                origin: origin::SYNTHETIC,
            },
            Location {
                uri: "file:///c.scala".to_string(),
                range: Rng::default(),
                origin: origin::PLUGIN,
            },
        ],
    };
    let decoded = DefinitionResult::decode(&result.encode().unwrap()).unwrap();
    assert_eq!(decoded, result);
    let origins: Vec<u32> = decoded.locations.iter().map(|l| l.origin).collect();
    assert_eq!(
        origins,
        vec![origin::WORKSPACE, origin::SYNTHETIC, origin::PLUGIN]
    );
}

#[test]
fn opaque_data_bytes_survive_and_empty_is_distinct_from_none() {
    let mut item = bare_item("x");
    item.data = Some(vec![0u8, 1, 2, 0, 255]);
    assert_eq!(
        CompletionItem::decode(&item.encode().unwrap()).unwrap(),
        item
    );

    let mut empty_data = bare_item("x");
    empty_data.data = Some(vec![]);
    let no_data = bare_item("x");
    assert_eq!(
        CompletionItem::decode(&empty_data.encode().unwrap()).unwrap(),
        empty_data
    );
    assert_ne!(empty_data.encode().unwrap(), no_data.encode().unwrap());
}

#[test]
fn method_hits_round_trip_and_empty_is_a_real_empty_list() {
    let result = MethodHitsResult {
        hits: vec![
            MethodHit {
                uri: "file:///w/Enrichments.scala".to_string(),
                symbol: "a/b/A$package.incr().".to_string(),
                kind: 3,
                range: range(1, 6, 1, 10),
            },
            MethodHit {
                uri: "file:///w/Ops.scala".to_string(),
                symbol: "pkg/Ops.deco().".to_string(),
                kind: 0,
                range: Rng::default(),
            },
        ],
    };
    let decoded = MethodHitsResult::decode(&result.encode().unwrap()).unwrap();
    assert_eq!(decoded, result);

    let empty = MethodHitsResult { hits: vec![] };
    let decoded = MethodHitsResult::decode(&empty.encode().unwrap()).unwrap();
    assert!(decoded.hits.is_empty());

    // A method-hits buffer is not decodable as the locations payload (distinct
    // envelope kinds), so the two callback responses cannot be confused.
    assert!(LocationsResult::decode(&result.encode().unwrap()).is_err());
}

// ---------------------------------------------------------------------------
// Payload-query carriers (ABI v2).
// ---------------------------------------------------------------------------

fn pos(line: u32, character: u32) -> Pos {
    Pos { line, character }
}

#[test]
fn inlay_hint_params_round_trip() {
    let params = InlayHintParams {
        uri: "file:///w/H.scala".to_string(),
        range: range(0, 0, 20, 0),
        flags: 0b101,
    };
    assert_eq!(
        InlayHintParams::decode(&params.encode().unwrap()).unwrap(),
        params
    );
}

#[test]
fn inlay_hints_full_surface_round_trips() {
    let result = InlayHintsResult {
        hints: vec![
            InlayHint {
                position: pos(2, 10),
                label_parts: vec![
                    InlayLabelPart {
                        text: ": Int".to_string(),
                        location: Some(("file:///w/I.scala".to_string(), range(1, 0, 1, 3))),
                        tooltip: Some("inferred type".to_string()),
                    },
                    InlayLabelPart {
                        text: "=>".to_string(),
                        location: None,
                        tooltip: None,
                    },
                ],
                kind: 1,
                padding_left: true,
                padding_right: false,
                text_edits: Some(vec![TextEdit {
                    range: range(2, 10, 2, 10),
                    new_text: ": Int".to_string(),
                }]),
                data: Some(vec![0u8, 1, 2, 0, 255]),
            },
            InlayHint {
                position: pos(0, 0),
                label_parts: vec![],
                kind: 0,
                padding_left: false,
                padding_right: false,
                text_edits: None,
                data: None,
            },
        ],
    };
    assert_eq!(
        InlayHintsResult::decode(&result.encode().unwrap()).unwrap(),
        result
    );

    // The opaque data bytes distinguish empty-present from absent, exactly
    // like CompletionItem.data.
    let mut empty_data = result.clone();
    empty_data.hints[0].data = Some(vec![]);
    let mut no_data = result.clone();
    no_data.hints[0].data = None;
    assert_eq!(
        InlayHintsResult::decode(&empty_data.encode().unwrap()).unwrap(),
        empty_data
    );
    assert_ne!(empty_data.encode().unwrap(), no_data.encode().unwrap());
}

#[test]
fn semantic_tokens_round_trip_as_offsets() {
    let result = SemanticTokensResult {
        nodes: vec![
            SemanticNode {
                start: 0,
                end: 6,
                token_type: 3,
                token_modifier: 1,
            },
            SemanticNode {
                start: 10,
                end: 14,
                token_type: 15,
                token_modifier: 0,
            },
        ],
    };
    assert_eq!(
        SemanticTokensResult::decode(&result.encode().unwrap()).unwrap(),
        result
    );
    let empty = SemanticTokensResult { nodes: vec![] };
    assert!(SemanticTokensResult::decode(&empty.encode().unwrap())
        .unwrap()
        .nodes
        .is_empty());
}

#[test]
fn selection_ranges_round_trip_per_position_chains() {
    let params = SelectionRangeParams {
        uri: "file:///w/S.scala".to_string(),
        positions: vec![pos(1, 2), pos(3, 4)],
    };
    assert_eq!(
        SelectionRangeParams::decode(&params.encode().unwrap()).unwrap(),
        params
    );

    // Innermost-first chains; an empty chain (no enclosing range for a
    // position) is preserved as a real empty list.
    let result = SelectionRangesResult {
        chains: vec![
            vec![range(1, 2, 1, 4), range(1, 0, 2, 0), range(0, 0, 9, 0)],
            vec![],
        ],
    };
    let decoded = SelectionRangesResult::decode(&result.encode().unwrap()).unwrap();
    assert_eq!(decoded, result);
    assert!(decoded.chains[1].is_empty());
}

#[test]
fn code_action_params_round_trip_with_and_without_optionals() {
    let full = CodeActionParams {
        uri: "file:///w/C.scala".to_string(),
        action: code_action_id::EXTRACT_METHOD,
        position: pos(5, 1),
        extraction_end: Some(pos(7, 2)),
        arg_indices: Some(vec![0, 2]),
    };
    assert_eq!(
        CodeActionParams::decode(&full.encode().unwrap()).unwrap(),
        full
    );

    let bare = CodeActionParams {
        uri: "file:///w/C.scala".to_string(),
        action: code_action_id::INSERT_INFERRED_TYPE,
        position: pos(5, 1),
        extraction_end: None,
        arg_indices: None,
    };
    assert_eq!(
        CodeActionParams::decode(&bare.encode().unwrap()).unwrap(),
        bare
    );
    // An absent extraction end is distinct from a present zero position.
    let zero_end = CodeActionParams {
        extraction_end: Some(pos(0, 0)),
        ..bare.clone()
    };
    assert_ne!(bare.encode().unwrap(), zero_end.encode().unwrap());
}

#[test]
fn code_action_refusal_is_data_not_an_error() {
    let refused = CodeActionResult {
        edits: vec![],
        refusal: Some("Cannot extract selection".to_string()),
    };
    assert_eq!(
        CodeActionResult::decode(&refused.encode().unwrap()).unwrap(),
        refused
    );

    let edits = CodeActionResult {
        edits: vec![TextEdit {
            range: range(3, 0, 3, 0),
            new_text: ": Int".to_string(),
        }],
        refusal: None,
    };
    assert_eq!(
        CodeActionResult::decode(&edits.encode().unwrap()).unwrap(),
        edits
    );
    // A refusal-less empty result and an empty-string refusal stay distinct.
    let empty = CodeActionResult {
        edits: vec![],
        refusal: None,
    };
    let empty_refusal = CodeActionResult {
        edits: vec![],
        refusal: Some(String::new()),
    };
    assert_ne!(empty.encode().unwrap(), empty_refusal.encode().unwrap());
}

#[test]
fn auto_imports_round_trip() {
    let params = AutoImportParams {
        uri: "file:///w/A.scala".to_string(),
        position: pos(4, 9),
        name: "Future".to_string(),
        is_extension: false,
    };
    assert_eq!(
        AutoImportParams::decode(&params.encode().unwrap()).unwrap(),
        params
    );

    let result = AutoImportsResult {
        imports: vec![AutoImport {
            package_name: "scala.concurrent".to_string(),
            edits: vec![TextEdit {
                range: range(0, 0, 0, 0),
                new_text: "import scala.concurrent.Future\n".to_string(),
            }],
            symbol: Some("scala/concurrent/Future#".to_string()),
        }],
    };
    assert_eq!(
        AutoImportsResult::decode(&result.encode().unwrap()).unwrap(),
        result
    );
}

#[test]
fn pc_diagnostics_round_trip() {
    let result = PcDiagnosticsResult {
        diagnostics: vec![PcDiagnostic {
            range: range(3, 0, 3, 5),
            severity: 1,
            code: "E007".to_string(),
            message: "not found: value x".to_string(),
        }],
    };
    assert_eq!(
        PcDiagnosticsResult::decode(&result.encode().unwrap()).unwrap(),
        result
    );
}

#[test]
fn folding_ranges_round_trip_with_kind_ordinals() {
    let result = FoldingRangesResult {
        ranges: vec![
            FoldingRange {
                range: range(0, 0, 5, 1),
                kind: folding_kind::IMPORTS,
            },
            FoldingRange {
                range: range(6, 10, 9, 1),
                kind: folding_kind::NONE,
            },
        ],
    };
    assert_eq!(
        FoldingRangesResult::decode(&result.encode().unwrap()).unwrap(),
        result
    );
}

#[test]
fn toplevels_round_trip_and_empty_is_a_real_empty_list() {
    let result = ToplevelsResult {
        symbols: vec!["a/b/Main.".to_string(), "a/b/Main#".to_string()],
    };
    assert_eq!(
        ToplevelsResult::decode(&result.encode().unwrap()).unwrap(),
        result
    );
    let empty = ToplevelsResult { symbols: vec![] };
    assert!(ToplevelsResult::decode(&empty.encode().unwrap())
        .unwrap()
        .symbols
        .is_empty());
}

#[test]
fn new_payload_kinds_cannot_be_confused() {
    // Every new envelope kind is distinct: a buffer of one payload never
    // decodes as another (the same guarantee method_hits vs locations pins).
    let uri = UriParams {
        uri: "file:///w/U.scala".to_string(),
    }
    .encode()
    .unwrap();
    assert!(InlayHintParams::decode(&uri).is_err());
    assert!(SemanticTokensResult::decode(&uri).is_err());
    assert!(FoldingRangesResult::decode(&uri).is_err());
    assert!(PcDiagnosticsResult::decode(&uri).is_err());

    let toplevels = ToplevelsResult {
        symbols: vec!["a/b/Main.".to_string()],
    }
    .encode()
    .unwrap();
    assert!(LocationsResult::decode(&toplevels).is_err());
    assert!(MethodHitsResult::decode(&toplevels).is_err());

    let tokens = SemanticTokensResult {
        nodes: vec![SemanticNode {
            start: 0,
            end: 4,
            token_type: 1,
            token_modifier: 0,
        }],
    }
    .encode()
    .unwrap();
    assert!(SelectionRangesResult::decode(&tokens).is_err());
    assert!(InlayHintsResult::decode(&tokens).is_err());

    let refusal = CodeActionResult {
        edits: vec![],
        refusal: None,
    }
    .encode()
    .unwrap();
    assert!(AutoImportsResult::decode(&refusal).is_err());
    assert!(CodeActionParams::decode(&refusal).is_err());
}

#[test]
fn unicode_strings_round_trip() {
    let params = DidOpenParams {
        target_id: "root/módulo".to_string(),
        uri: "file:///café/★.scala".to_string(),
        text: "val 名前 = \"🎉\"\n".to_string(),
    };
    assert_eq!(
        DidOpenParams::decode(&params.encode().unwrap()).unwrap(),
        params
    );
}

// ---------------------------------------------------------------------------
// Property strategies for the expanded shapes.
// ---------------------------------------------------------------------------

fn rng_strat() -> impl Strategy<Value = Rng> {
    (any::<u32>(), any::<u32>(), any::<u32>(), any::<u32>())
        .prop_map(|(a, b, c, d)| range(a, b, c, d))
}

fn text_edit_strat() -> impl Strategy<Value = TextEdit> {
    (rng_strat(), ".*").prop_map(|(range, new_text)| TextEdit { range, new_text })
}

fn markup_strat() -> impl Strategy<Value = MarkupContent> {
    (".*", ".*").prop_map(|(kind, value)| MarkupContent { kind, value })
}

fn documentation_strat() -> impl Strategy<Value = Documentation> {
    prop_oneof![
        ".*".prop_map(Documentation::Plain),
        markup_strat().prop_map(Documentation::Markup),
    ]
}

fn opt_bytes() -> impl Strategy<Value = Option<Vec<u8>>> {
    proptest::option::of(proptest::collection::vec(any::<u8>(), 0..8))
}

fn completion_edit_strat() -> impl Strategy<Value = CompletionEdit> {
    prop_oneof![
        text_edit_strat().prop_map(CompletionEdit::Plain),
        (".*", rng_strat(), rng_strat()).prop_map(|(new_text, insert, replace)| {
            CompletionEdit::InsertReplace(InsertReplaceEdit {
                new_text,
                insert,
                replace,
            })
        }),
    ]
}

fn command_strat() -> impl Strategy<Value = Command> {
    (".*", proptest::option::of(".*"), ".*", opt_bytes()).prop_map(
        |(title, tooltip, command, arguments)| Command {
            title,
            tooltip,
            command,
            arguments,
        },
    )
}

fn label_details_strat() -> impl Strategy<Value = LabelDetails> {
    (proptest::option::of(".*"), proptest::option::of(".*")).prop_map(|(detail, description)| {
        LabelDetails {
            detail,
            description,
        }
    })
}

fn completion_item_strat() -> impl Strategy<Value = CompletionItem> {
    let head = (
        ".*",
        proptest::option::of(label_details_strat()),
        proptest::option::of(any::<i32>()),
        proptest::option::of(proptest::collection::vec(any::<i32>(), 0..3)),
        proptest::option::of(".*"),
        proptest::option::of(documentation_strat()),
        proptest::option::of(any::<bool>()),
        proptest::option::of(any::<bool>()),
        proptest::option::of(".*"),
    );
    let tail = (
        proptest::option::of(".*"),
        proptest::option::of(".*"),
        proptest::option::of(any::<i32>()),
        proptest::option::of(any::<i32>()),
        proptest::option::of(completion_edit_strat()),
        proptest::option::of(".*"),
        proptest::option::of(proptest::collection::vec(text_edit_strat(), 0..3)),
        proptest::option::of(proptest::collection::vec(".*", 0..3)),
        proptest::option::of(command_strat()),
        opt_bytes(),
    );
    (head, tail).prop_map(
        |(
            (
                label,
                label_details,
                kind,
                tags,
                detail,
                documentation,
                deprecated,
                preselect,
                sort_text,
            ),
            (
                filter_text,
                insert_text,
                insert_text_format,
                insert_text_mode,
                text_edit,
                text_edit_text,
                additional_text_edits,
                commit_characters,
                command,
                data,
            ),
        )| CompletionItem {
            label,
            label_details,
            kind,
            tags,
            detail,
            documentation,
            deprecated,
            preselect,
            sort_text,
            filter_text,
            insert_text,
            insert_text_format,
            insert_text_mode,
            text_edit,
            text_edit_text,
            additional_text_edits,
            commit_characters,
            command,
            data,
        },
    )
}

fn item_defaults_strat() -> impl Strategy<Value = CompletionItemDefaults> {
    let edit_range = prop_oneof![
        rng_strat().prop_map(EditRange::Range),
        (rng_strat(), rng_strat())
            .prop_map(|(insert, replace)| EditRange::InsertReplace { insert, replace }),
    ];
    (
        proptest::option::of(proptest::collection::vec(".*", 0..3)),
        proptest::option::of(edit_range),
        proptest::option::of(any::<i32>()),
        proptest::option::of(any::<i32>()),
        opt_bytes(),
    )
        .prop_map(
            |(commit_characters, edit_range, insert_text_format, insert_text_mode, data)| {
                CompletionItemDefaults {
                    commit_characters,
                    edit_range,
                    insert_text_format,
                    insert_text_mode,
                    data,
                }
            },
        )
}

fn hover_contents_strat() -> impl Strategy<Value = HoverContents> {
    let marked = prop_oneof![
        ".*".prop_map(MarkedStringItem::Plain),
        (".*", ".*").prop_map(|(language, value)| MarkedStringItem::Marked { language, value }),
    ];
    prop_oneof![
        markup_strat().prop_map(HoverContents::Markup),
        proptest::collection::vec(marked, 0..3).prop_map(HoverContents::Marked),
    ]
}

fn signature_strat() -> impl Strategy<Value = SignatureInfo> {
    let param_label = prop_oneof![
        ".*".prop_map(ParameterLabel::Str),
        (any::<u32>(), any::<u32>())
            .prop_map(|(start, end)| ParameterLabel::Offsets { start, end }),
    ];
    let param = (param_label, proptest::option::of(documentation_strat())).prop_map(
        |(label, documentation)| ParameterInfo {
            label,
            documentation,
        },
    );
    (
        ".*",
        proptest::option::of(documentation_strat()),
        proptest::option::of(proptest::collection::vec(param, 0..4)),
        proptest::option::of(any::<i32>()),
    )
        .prop_map(
            |(label, documentation, parameters, active_parameter)| SignatureInfo {
                label,
                documentation,
                parameters,
                active_parameter,
            },
        )
}

fn location_strat() -> impl Strategy<Value = Location> {
    (".*", rng_strat(), 0u32..3).prop_map(|(uri, range, origin)| Location { uri, range, origin })
}

fn method_hit_strat() -> impl Strategy<Value = MethodHit> {
    (".*", ".*", any::<i32>(), rng_strat()).prop_map(|(uri, symbol, kind, range)| MethodHit {
        uri,
        symbol,
        kind,
        range,
    })
}

fn pos_strat() -> impl Strategy<Value = Pos> {
    (any::<u32>(), any::<u32>()).prop_map(|(line, character)| Pos { line, character })
}

fn inlay_label_part_strat() -> impl Strategy<Value = InlayLabelPart> {
    (
        ".*",
        proptest::option::of((".*", rng_strat())),
        proptest::option::of(".*"),
    )
        .prop_map(|(text, location, tooltip)| InlayLabelPart {
            text,
            location,
            tooltip,
        })
}

fn inlay_hint_strat() -> impl Strategy<Value = InlayHint> {
    (
        pos_strat(),
        proptest::collection::vec(inlay_label_part_strat(), 0..3),
        any::<i32>(),
        any::<bool>(),
        any::<bool>(),
        proptest::option::of(proptest::collection::vec(text_edit_strat(), 0..3)),
        opt_bytes(),
    )
        .prop_map(
            |(position, label_parts, kind, padding_left, padding_right, text_edits, data)| {
                InlayHint {
                    position,
                    label_parts,
                    kind,
                    padding_left,
                    padding_right,
                    text_edits,
                    data,
                }
            },
        )
}

fn semantic_node_strat() -> impl Strategy<Value = SemanticNode> {
    (any::<u32>(), any::<u32>(), any::<i32>(), any::<i32>()).prop_map(
        |(start, end, token_type, token_modifier)| SemanticNode {
            start,
            end,
            token_type,
            token_modifier,
        },
    )
}

fn auto_import_strat() -> impl Strategy<Value = AutoImport> {
    (
        ".*",
        proptest::collection::vec(text_edit_strat(), 0..3),
        proptest::option::of(".*"),
    )
        .prop_map(|(package_name, edits, symbol)| AutoImport {
            package_name,
            edits,
            symbol,
        })
}

prop_compose! {
    fn compiler_plugin_strat()(
        jars in proptest::collection::vec(".*", 0..3),
        options in proptest::collection::vec(".*", 0..3),
        loaded in any::<bool>(),
        detail in ".*",
    ) -> CompilerPlugin {
        CompilerPlugin { jars, options, loaded, detail }
    }
}

prop_compose! {
    fn service_plugin_strat()(
        id in ".*",
        source in ".*",
        enabled in any::<bool>(),
        self_test_ok in any::<bool>(),
        self_test_detail in ".*",
    ) -> ServicePlugin {
        ServicePlugin { id, source, enabled, self_test_ok, self_test_detail }
    }
}

proptest! {
    #[test]
    fn target_config_round_trips(
        bsp_id in ".*",
        scala_version in ".*",
        classpath in proptest::collection::vec(".*", 0..5),
        scalac_options in proptest::collection::vec(".*", 0..5),
        source_dirs in proptest::collection::vec(".*", 0..5),
    ) {
        let cfg = TargetConfig { bsp_id, scala_version, classpath, scalac_options, source_dirs };
        prop_assert_eq!(TargetConfig::decode(&cfg.encode().unwrap()).unwrap(), cfg);
    }

    #[test]
    fn did_change_round_trips(uri in ".*", text in ".*") {
        let params = DidChangeParams { uri, text };
        prop_assert_eq!(DidChangeParams::decode(&params.encode().unwrap()).unwrap(), params);
    }

    #[test]
    fn position_params_round_trip(uri in ".*", line in any::<u32>(), character in any::<u32>()) {
        let params = PositionParams { uri, line, character };
        prop_assert_eq!(PositionParams::decode(&params.encode().unwrap()).unwrap(), params);
    }

    #[test]
    fn completion_item_round_trips(item in completion_item_strat()) {
        prop_assert_eq!(CompletionItem::decode(&item.encode().unwrap()).unwrap(), item);
    }

    #[test]
    fn completion_list_round_trips(
        is_incomplete in any::<bool>(),
        item_defaults in proptest::option::of(item_defaults_strat()),
        apply_kind in proptest::option::of(
            (proptest::option::of(any::<i32>()), proptest::option::of(any::<i32>()))
                .prop_map(|(commit_characters, data)| CompletionApplyKind { commit_characters, data }),
        ),
        items in proptest::collection::vec(completion_item_strat(), 0..5),
    ) {
        let list = CompletionList { is_incomplete, item_defaults, apply_kind, items };
        prop_assert_eq!(CompletionList::decode(&list.encode().unwrap()).unwrap(), list);
    }

    #[test]
    fn resolve_params_round_trip(
        target_id in ".*",
        symbol in ".*",
        item in completion_item_strat(),
    ) {
        let params = ResolveParams { target_id, symbol, item };
        prop_assert_eq!(ResolveParams::decode(&params.encode().unwrap()).unwrap(), params);
    }

    #[test]
    fn hover_round_trips(
        present in any::<bool>(),
        contents in hover_contents_strat(),
        range in proptest::option::of(rng_strat()),
    ) {
        let hover = present.then_some(Hover { contents, range });
        let result = HoverResult(hover);
        prop_assert_eq!(HoverResult::decode(&result.encode().unwrap()).unwrap(), result);
    }

    #[test]
    fn signature_help_round_trips(
        signatures in proptest::collection::vec(signature_strat(), 0..4),
        active_signature in proptest::option::of(any::<i32>()),
        active_parameter in proptest::option::of(any::<i32>()),
    ) {
        let help = SignatureHelp { signatures, active_signature, active_parameter };
        prop_assert_eq!(SignatureHelp::decode(&help.encode().unwrap()).unwrap(), help);
    }

    #[test]
    fn definition_round_trips(
        symbol in ".*",
        locations in proptest::collection::vec(location_strat(), 0..6),
    ) {
        let result = DefinitionResult { symbol, locations };
        prop_assert_eq!(DefinitionResult::decode(&result.encode().unwrap()).unwrap(), result);
    }

    #[test]
    fn locations_round_trip(locations in proptest::collection::vec(location_strat(), 0..6)) {
        let result = LocationsResult { locations };
        prop_assert_eq!(LocationsResult::decode(&result.encode().unwrap()).unwrap(), result);
    }

    #[test]
    fn method_hits_round_trip(hits in proptest::collection::vec(method_hit_strat(), 0..6)) {
        let result = MethodHitsResult { hits };
        prop_assert_eq!(MethodHitsResult::decode(&result.encode().unwrap()).unwrap(), result);
    }

    #[test]
    fn inlay_hint_params_round_trip_prop(uri in ".*", range in rng_strat(), flags in any::<u32>()) {
        let params = InlayHintParams { uri, range, flags };
        prop_assert_eq!(InlayHintParams::decode(&params.encode().unwrap()).unwrap(), params);
    }

    #[test]
    fn inlay_hints_round_trip_prop(hints in proptest::collection::vec(inlay_hint_strat(), 0..4)) {
        let result = InlayHintsResult { hints };
        prop_assert_eq!(InlayHintsResult::decode(&result.encode().unwrap()).unwrap(), result);
    }

    #[test]
    fn uri_params_round_trip_prop(uri in ".*") {
        let params = UriParams { uri };
        prop_assert_eq!(UriParams::decode(&params.encode().unwrap()).unwrap(), params);
    }

    #[test]
    fn semantic_tokens_round_trip_prop(nodes in proptest::collection::vec(semantic_node_strat(), 0..8)) {
        let result = SemanticTokensResult { nodes };
        prop_assert_eq!(SemanticTokensResult::decode(&result.encode().unwrap()).unwrap(), result);
    }

    #[test]
    fn selection_range_params_round_trip_prop(
        uri in ".*",
        positions in proptest::collection::vec(pos_strat(), 0..5),
    ) {
        let params = SelectionRangeParams { uri, positions };
        prop_assert_eq!(SelectionRangeParams::decode(&params.encode().unwrap()).unwrap(), params);
    }

    #[test]
    fn selection_ranges_round_trip_prop(
        chains in proptest::collection::vec(proptest::collection::vec(rng_strat(), 0..4), 0..4),
    ) {
        let result = SelectionRangesResult { chains };
        prop_assert_eq!(SelectionRangesResult::decode(&result.encode().unwrap()).unwrap(), result);
    }

    #[test]
    fn code_action_params_round_trip_prop(
        uri in ".*",
        action in any::<i32>(),
        position in pos_strat(),
        extraction_end in proptest::option::of(pos_strat()),
        arg_indices in proptest::option::of(proptest::collection::vec(any::<i32>(), 0..4)),
    ) {
        let params = CodeActionParams { uri, action, position, extraction_end, arg_indices };
        prop_assert_eq!(CodeActionParams::decode(&params.encode().unwrap()).unwrap(), params);
    }

    #[test]
    fn code_action_result_round_trip_prop(
        edits in proptest::collection::vec(text_edit_strat(), 0..4),
        refusal in proptest::option::of(".*"),
    ) {
        let result = CodeActionResult { edits, refusal };
        prop_assert_eq!(CodeActionResult::decode(&result.encode().unwrap()).unwrap(), result);
    }

    #[test]
    fn auto_import_params_round_trip_prop(
        uri in ".*",
        position in pos_strat(),
        name in ".*",
        is_extension in any::<bool>(),
    ) {
        let params = AutoImportParams { uri, position, name, is_extension };
        prop_assert_eq!(AutoImportParams::decode(&params.encode().unwrap()).unwrap(), params);
    }

    #[test]
    fn auto_imports_round_trip_prop(imports in proptest::collection::vec(auto_import_strat(), 0..4)) {
        let result = AutoImportsResult { imports };
        prop_assert_eq!(AutoImportsResult::decode(&result.encode().unwrap()).unwrap(), result);
    }

    #[test]
    fn pc_diagnostics_round_trip_prop(
        diagnostics in proptest::collection::vec(
            (rng_strat(), any::<i32>(), ".*", ".*").prop_map(|(range, severity, code, message)| {
                PcDiagnostic { range, severity, code, message }
            }),
            0..5,
        ),
    ) {
        let result = PcDiagnosticsResult { diagnostics };
        prop_assert_eq!(PcDiagnosticsResult::decode(&result.encode().unwrap()).unwrap(), result);
    }

    #[test]
    fn folding_ranges_round_trip_prop(
        ranges in proptest::collection::vec(
            (rng_strat(), any::<i32>()).prop_map(|(range, kind)| FoldingRange { range, kind }),
            0..5,
        ),
    ) {
        let result = FoldingRangesResult { ranges };
        prop_assert_eq!(FoldingRangesResult::decode(&result.encode().unwrap()).unwrap(), result);
    }

    #[test]
    fn toplevels_round_trip_prop(symbols in proptest::collection::vec(".*", 0..6)) {
        let result = ToplevelsResult { symbols };
        prop_assert_eq!(ToplevelsResult::decode(&result.encode().unwrap()).unwrap(), result);
    }

    #[test]
    fn prepare_rename_round_trips(range in proptest::option::of(rng_strat())) {
        let result = PrepareRenameResult(range);
        prop_assert_eq!(PrepareRenameResult::decode(&result.encode().unwrap()).unwrap(), result);
    }

    #[test]
    fn plugin_status_round_trips(
        compiler_plugins in proptest::collection::vec(compiler_plugin_strat(), 0..3),
        service_plugins in proptest::collection::vec(service_plugin_strat(), 0..3),
        disabled in proptest::collection::vec(
            (".*", ".*").prop_map(|(id, reason)| DisabledPlugin { id, reason }),
            0..3,
        ),
    ) {
        let status = PluginStatus { compiler_plugins, service_plugins, disabled };
        prop_assert_eq!(PluginStatus::decode(&status.encode().unwrap()).unwrap(), status);
    }
}
