//! Every op payload round-trips through its `encode`/`decode` pair without loss,
//! including the full LSP4J carrier surface (label details, tags, deprecated/
//! preselect, documentation variants, insert/replace edits, command, item
//! defaults, hover contents variants, signature parameter label offsets), the
//! nullable-vs-empty distinctions, and the definition origin tags.

use ls_pc_abi::payloads::{
    origin, Command, CompilerPlugin, CompletionApplyKind, CompletionEdit, CompletionItem,
    CompletionItemDefaults, CompletionList, DefinitionResult, DidChangeParams, DidOpenParams,
    DisabledPlugin, Documentation, EditRange, Hover, HoverContents, HoverResult, InsertReplaceEdit,
    LabelDetails, Location, LocationsResult, MarkedStringItem, MarkupContent, ParameterInfo,
    ParameterLabel, PluginStatus, PositionParams, PrepareRenameResult, ResolveParams, Rng,
    ServicePlugin, SignatureHelp, SignatureInfo, TargetConfig, TextEdit,
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
    assert_eq!(CompletionItem::decode(&item.encode()).unwrap(), item);
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
    assert_eq!(CompletionItem::decode(&item.encode()).unwrap(), item);

    let list = CompletionList {
        is_incomplete: false,
        item_defaults: None,
        apply_kind: Some(CompletionApplyKind {
            commit_characters: Some(1), // Replace
            data: Some(2),              // Merge
        }),
        items: vec![item],
    };
    assert_eq!(CompletionList::decode(&list.encode()).unwrap(), list);

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
        CompletionList::decode(&empty_kind.encode()).unwrap(),
        empty_kind
    );
    assert_ne!(empty_kind.encode(), no_kind.encode());
}

#[test]
fn completion_edit_plain_variant_round_trips() {
    let mut item = bare_item("f");
    item.text_edit = Some(CompletionEdit::Plain(TextEdit {
        range: range(0, 0, 0, 1),
        new_text: "f()".to_string(),
    }));
    assert_eq!(CompletionItem::decode(&item.encode()).unwrap(), item);
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
    assert_eq!(CompletionList::decode(&list.encode()).unwrap(), list);

    // editRange as a plain Range is a distinct variant.
    let mut plain = list.clone();
    plain.item_defaults.as_mut().unwrap().edit_range = Some(EditRange::Range(range(0, 0, 0, 3)));
    assert_eq!(CompletionList::decode(&plain.encode()).unwrap(), plain);
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
    assert_eq!(HoverResult::decode(&hover.encode()).unwrap(), hover);
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
    assert_eq!(SignatureHelp::decode(&help.encode()).unwrap(), help);
}

// ---------------------------------------------------------------------------
// Nullable-vs-empty and origin edge cases.
// ---------------------------------------------------------------------------

#[test]
fn hover_null_is_distinct_from_present_empty() {
    let null = HoverResult(None);
    assert_eq!(HoverResult::decode(&null.encode()).unwrap(), null);

    let present_empty = HoverResult(Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: "plaintext".to_string(),
            value: String::new(),
        }),
        range: None,
    }));
    assert_eq!(
        HoverResult::decode(&present_empty.encode()).unwrap(),
        present_empty
    );
    assert_ne!(null.encode(), present_empty.encode());
}

#[test]
fn prepare_rename_null_is_distinct_from_zero_range() {
    let null = PrepareRenameResult(None);
    let zero = PrepareRenameResult(Some(Rng::default()));
    assert_eq!(PrepareRenameResult::decode(&null.encode()).unwrap(), null);
    assert_eq!(PrepareRenameResult::decode(&zero.encode()).unwrap(), zero);
    assert_ne!(null.encode(), zero.encode());
}

#[test]
fn optional_scalars_distinguish_none_from_zero_and_false() {
    let mut some = bare_item("x");
    some.kind = Some(0);
    some.deprecated = Some(false);
    some.preselect = Some(false);
    let none = bare_item("x");
    assert_eq!(CompletionItem::decode(&some.encode()).unwrap(), some);
    assert_eq!(CompletionItem::decode(&none.encode()).unwrap(), none);
    assert_ne!(some.encode(), none.encode());
}

#[test]
fn nullable_list_distinguishes_none_from_empty() {
    let mut none = bare_item("x");
    none.commit_characters = None;
    none.tags = None;
    let mut empty = bare_item("x");
    empty.commit_characters = Some(vec![]);
    empty.tags = Some(vec![]);
    assert_eq!(CompletionItem::decode(&none.encode()).unwrap(), none);
    assert_eq!(CompletionItem::decode(&empty.encode()).unwrap(), empty);
    assert_ne!(none.encode(), empty.encode());
}

#[test]
fn empty_completion_list_round_trips() {
    let empty = CompletionList {
        is_incomplete: false,
        item_defaults: None,
        apply_kind: None,
        items: vec![],
    };
    let decoded = CompletionList::decode(&empty.encode()).unwrap();
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
    let decoded = DefinitionResult::decode(&result.encode()).unwrap();
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
    assert_eq!(CompletionItem::decode(&item.encode()).unwrap(), item);

    let mut empty_data = bare_item("x");
    empty_data.data = Some(vec![]);
    let no_data = bare_item("x");
    assert_eq!(
        CompletionItem::decode(&empty_data.encode()).unwrap(),
        empty_data
    );
    assert_ne!(empty_data.encode(), no_data.encode());
}

#[test]
fn unicode_strings_round_trip() {
    let params = DidOpenParams {
        target_id: "root/módulo".to_string(),
        uri: "file:///café/★.scala".to_string(),
        text: "val 名前 = \"🎉\"\n".to_string(),
    };
    assert_eq!(DidOpenParams::decode(&params.encode()).unwrap(), params);
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
        prop_assert_eq!(TargetConfig::decode(&cfg.encode()).unwrap(), cfg);
    }

    #[test]
    fn did_change_round_trips(uri in ".*", text in ".*") {
        let params = DidChangeParams { uri, text };
        prop_assert_eq!(DidChangeParams::decode(&params.encode()).unwrap(), params);
    }

    #[test]
    fn position_params_round_trip(uri in ".*", line in any::<u32>(), character in any::<u32>()) {
        let params = PositionParams { uri, line, character };
        prop_assert_eq!(PositionParams::decode(&params.encode()).unwrap(), params);
    }

    #[test]
    fn completion_item_round_trips(item in completion_item_strat()) {
        prop_assert_eq!(CompletionItem::decode(&item.encode()).unwrap(), item);
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
        prop_assert_eq!(CompletionList::decode(&list.encode()).unwrap(), list);
    }

    #[test]
    fn resolve_params_round_trip(
        target_id in ".*",
        symbol in ".*",
        item in completion_item_strat(),
    ) {
        let params = ResolveParams { target_id, symbol, item };
        prop_assert_eq!(ResolveParams::decode(&params.encode()).unwrap(), params);
    }

    #[test]
    fn hover_round_trips(
        present in any::<bool>(),
        contents in hover_contents_strat(),
        range in proptest::option::of(rng_strat()),
    ) {
        let hover = present.then_some(Hover { contents, range });
        let result = HoverResult(hover);
        prop_assert_eq!(HoverResult::decode(&result.encode()).unwrap(), result);
    }

    #[test]
    fn signature_help_round_trips(
        signatures in proptest::collection::vec(signature_strat(), 0..4),
        active_signature in proptest::option::of(any::<i32>()),
        active_parameter in proptest::option::of(any::<i32>()),
    ) {
        let help = SignatureHelp { signatures, active_signature, active_parameter };
        prop_assert_eq!(SignatureHelp::decode(&help.encode()).unwrap(), help);
    }

    #[test]
    fn definition_round_trips(
        symbol in ".*",
        locations in proptest::collection::vec(location_strat(), 0..6),
    ) {
        let result = DefinitionResult { symbol, locations };
        prop_assert_eq!(DefinitionResult::decode(&result.encode()).unwrap(), result);
    }

    #[test]
    fn locations_round_trip(locations in proptest::collection::vec(location_strat(), 0..6)) {
        let result = LocationsResult { locations };
        prop_assert_eq!(LocationsResult::decode(&result.encode()).unwrap(), result);
    }

    #[test]
    fn prepare_rename_round_trips(range in proptest::option::of(rng_strat())) {
        let result = PrepareRenameResult(range);
        prop_assert_eq!(PrepareRenameResult::decode(&result.encode()).unwrap(), result);
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
        prop_assert_eq!(PluginStatus::decode(&status.encode()).unwrap(), status);
    }
}
