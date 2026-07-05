//! Every op payload round-trips through its `encode`/`decode` pair without loss,
//! including the nullable-vs-empty distinctions and the definition origin tags.
//! Ported from the fidelity guarantees today's `ls.pc.worker` carriers hold.

use ls_pc_abi::payloads::{
    origin, CompilerPlugin, CompletionItem, CompletionList, DefinitionResult, DidChangeParams,
    DidOpenParams, DisabledPlugin, Hover, HoverResult, Location, LocationsResult, ParameterInfo,
    PluginStatus, PositionParams, PrepareRenameResult, ResolveParams, Rng, ServicePlugin,
    SignatureHelp, SignatureInfo, TargetConfig, TextEdit,
};
use proptest::prelude::*;

// ---------------------------------------------------------------------------
// Explicit edge cases (the distinctions Codex called out).
// ---------------------------------------------------------------------------

#[test]
fn hover_null_is_distinct_from_present_empty() {
    let null = HoverResult(None);
    assert_eq!(HoverResult::decode(&null.encode()).unwrap(), null);

    let present_empty = HoverResult(Some(Hover {
        contents: String::new(),
        kind: 1,
        range: None,
    }));
    assert_eq!(
        HoverResult::decode(&present_empty.encode()).unwrap(),
        present_empty
    );
    // The two encode to different buffers — null is not "empty contents".
    assert_ne!(null.encode(), present_empty.encode());
}

#[test]
fn hover_range_is_independently_nullable() {
    let with_range = HoverResult(Some(Hover {
        contents: "x: Int".to_string(),
        kind: 1,
        range: Some(Rng {
            start_line: 1,
            start_character: 2,
            end_line: 1,
            end_character: 5,
        }),
    }));
    assert_eq!(
        HoverResult::decode(&with_range.encode()).unwrap(),
        with_range
    );
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
fn empty_completion_list_round_trips() {
    let empty = CompletionList {
        is_incomplete: false,
        items: vec![],
    };
    let decoded = CompletionList::decode(&empty.encode()).unwrap();
    assert_eq!(decoded, empty);
    assert!(decoded.items.is_empty());

    let incomplete_empty = CompletionList {
        is_incomplete: true,
        items: vec![],
    };
    assert_eq!(
        CompletionList::decode(&incomplete_empty.encode()).unwrap(),
        incomplete_empty
    );
}

#[test]
fn definition_preserves_per_location_origin_tags() {
    let result = DefinitionResult {
        symbol: "com/example/Foo#bar().".to_string(),
        locations: vec![
            Location {
                uri: "file:///a.scala".to_string(),
                range: Rng {
                    start_line: 3,
                    start_character: 6,
                    end_line: 3,
                    end_character: 9,
                },
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
fn completion_item_optional_and_opaque_fields_round_trip() {
    // A `data` payload with an embedded NUL and non-textual bytes must survive
    // verbatim; `Some(vec![])` is distinct from `None`.
    let item = CompletionItem {
        label: "map".to_string(),
        kind: 2,
        detail: Some("def map[B](f: A => B): List[B]".to_string()),
        documentation: None,
        sort_text: Some(String::new()),
        filter_text: None,
        insert_text: None,
        insert_text_format: 2,
        text_edit: Some(TextEdit {
            range: Rng {
                start_line: 0,
                start_character: 0,
                end_line: 0,
                end_character: 3,
            },
            new_text: "map($0)".to_string(),
        }),
        additional_text_edits: vec![],
        commit_characters: vec![".".to_string(), "(".to_string()],
        data: Some(vec![0u8, 1, 2, 0, 255]),
    };
    assert_eq!(CompletionItem::decode(&item.encode()).unwrap(), item);

    let empty_data = CompletionItem {
        data: Some(vec![]),
        ..item.clone()
    };
    let no_data = CompletionItem { data: None, ..item };
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
// Property-based round trips.
// ---------------------------------------------------------------------------

fn rng_strat() -> impl Strategy<Value = Rng> {
    (any::<u32>(), any::<u32>(), any::<u32>(), any::<u32>()).prop_map(
        |(start_line, start_character, end_line, end_character)| Rng {
            start_line,
            start_character,
            end_line,
            end_character,
        },
    )
}

fn text_edit_strat() -> impl Strategy<Value = TextEdit> {
    (rng_strat(), ".*").prop_map(|(range, new_text)| TextEdit { range, new_text })
}

fn location_strat() -> impl Strategy<Value = Location> {
    (".*", rng_strat(), 0u32..3).prop_map(|(uri, range, origin)| Location { uri, range, origin })
}

prop_compose! {
    fn completion_item_strat()(
        label in ".*",
        kind in any::<i32>(),
        detail in proptest::option::of(".*"),
        documentation in proptest::option::of(".*"),
        sort_text in proptest::option::of(".*"),
        filter_text in proptest::option::of(".*"),
        insert_text in proptest::option::of(".*"),
        insert_text_format in any::<i32>(),
        text_edit in proptest::option::of(text_edit_strat()),
        additional_text_edits in proptest::collection::vec(text_edit_strat(), 0..4),
        commit_characters in proptest::collection::vec(".*", 0..4),
        data in proptest::option::of(proptest::collection::vec(any::<u8>(), 0..16)),
    ) -> CompletionItem {
        CompletionItem {
            label, kind, detail, documentation, sort_text, filter_text, insert_text,
            insert_text_format, text_edit, additional_text_edits, commit_characters, data,
        }
    }
}

prop_compose! {
    fn signature_strat()(
        label in ".*",
        documentation in proptest::option::of(".*"),
        parameters in proptest::collection::vec(
            (".*", proptest::option::of(".*")).prop_map(|(label, documentation)| ParameterInfo { label, documentation }),
            0..4,
        ),
    ) -> SignatureInfo {
        SignatureInfo { label, documentation, parameters }
    }
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
    fn completion_list_round_trips(
        is_incomplete in any::<bool>(),
        items in proptest::collection::vec(completion_item_strat(), 0..6),
    ) {
        let list = CompletionList { is_incomplete, items };
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
        contents in ".*",
        kind in any::<i32>(),
        range in proptest::option::of(rng_strat()),
    ) {
        let hover = present.then_some(Hover { contents, kind, range });
        let result = HoverResult(hover);
        prop_assert_eq!(HoverResult::decode(&result.encode()).unwrap(), result);
    }

    #[test]
    fn signature_help_round_trips(
        signatures in proptest::collection::vec(signature_strat(), 0..4),
        active_signature in any::<i32>(),
        active_parameter in any::<i32>(),
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
