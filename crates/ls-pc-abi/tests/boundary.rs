//! Exercises the vtables as real `#[repr(C)]` function-pointer tables: build a
//! PC vtable and the Rust vtable from stub ops, drive requests through their
//! slots, and prove the callee-measures / caller-frees response protocol leaks
//! nothing across many round-trips.
//!
//! Only this binary calls the Rust allocator, and only one `#[test]` here
//! touches the global live-allocation counter, so the accounting assertions are
//! race-free.

use std::mem::size_of;
use std::ptr;

use ls_pc_abi::abi::{
    PcPayloadQueryFn, PcQueryFn, PcRequestFn, PcResolveFn, PcStatusOutFn, PcUriFn, PcVoidFn,
    STATUS_ALLOC, STATUS_OK,
};
use ls_pc_abi::payloads::{
    code_action_id, folding_kind, origin, AutoImport, AutoImportParams, AutoImportsResult,
    CodeActionParams, CodeActionResult, CompilerPlugin, CompletionItem, CompletionList,
    DefinitionResult, DisabledPlugin, Documentation, FoldingRange, FoldingRangesResult,
    HoverResult, InlayHint, InlayHintParams, InlayHintsResult, InlayLabelPart, Location,
    LocationsResult, MethodHit, MethodHitsResult, PcDiagnostic, PcDiagnosticsResult, PluginStatus,
    Pos, PrepareRenameResult, ResolveParams, Rng, SelectionRangeParams, SelectionRangesResult,
    SemanticNode, SemanticTokensResult, ServicePlugin, SignatureHelp, SignatureInfo, TextEdit,
    ToplevelsResult, UriParams,
};
use ls_pc_abi::{
    compute_layout_canary, memory, LsBuf, LsStr, PcVtable, RustVtable, ABI_VERSION, LAYOUT_CANARY,
};

// --- marshalling helpers ---------------------------------------------------

unsafe fn read_ls_str(s: LsStr) -> String {
    if s.ptr.is_null() || s.len == 0 {
        return String::new();
    }
    let bytes = unsafe { std::slice::from_raw_parts(s.ptr, s.len as usize) };
    String::from_utf8_lossy(bytes).into_owned()
}

unsafe fn as_slice<'a>(buf: &LsBuf) -> &'a [u8] {
    if buf.ptr.is_null() {
        return &[];
    }
    unsafe { std::slice::from_raw_parts(buf.ptr, buf.len as usize) }
}

fn ls_str(s: &str) -> LsStr {
    LsStr {
        ptr: s.as_ptr(),
        len: s.len() as u32,
    }
}

fn empty_buf() -> LsBuf {
    LsBuf {
        ptr: ptr::null_mut(),
        len: 0,
    }
}

fn bare_item(label: String) -> CompletionItem {
    CompletionItem {
        label,
        label_details: None,
        kind: Some(1),
        tags: None,
        detail: None,
        documentation: None,
        deprecated: None,
        preselect: None,
        sort_text: None,
        filter_text: None,
        insert_text: None,
        insert_text_format: Some(1),
        insert_text_mode: None,
        text_edit: None,
        text_edit_text: None,
        additional_text_edits: None,
        commit_characters: None,
        command: None,
        data: None,
    }
}

fn single_item_list(label: String) -> CompletionList {
    CompletionList {
        is_incomplete: false,
        item_defaults: None,
        apply_kind: None,
        items: vec![bare_item(label)],
    }
}

// --- stub PC ops -----------------------------------------------------------

unsafe extern "C" fn stub_request(_ptr: *const u8, _len: u32) -> i32 {
    STATUS_OK
}

unsafe extern "C" fn stub_uri(_uri: LsStr) -> i32 {
    STATUS_OK
}

unsafe extern "C" fn stub_completion(
    uri: LsStr,
    line: u32,
    character: u32,
    out: *mut LsBuf,
) -> i32 {
    let label = format!("{}:{line}:{character}", unsafe { read_ls_str(uri) });
    let payload = single_item_list(label).encode().unwrap();
    if unsafe { memory::write_response(&payload, out) } {
        STATUS_OK
    } else {
        STATUS_ALLOC
    }
}

unsafe extern "C" fn stub_hover_null(
    _uri: LsStr,
    _line: u32,
    _character: u32,
    out: *mut LsBuf,
) -> i32 {
    let payload = HoverResult(None).encode().unwrap();
    if unsafe { memory::write_response(&payload, out) } {
        STATUS_OK
    } else {
        STATUS_ALLOC
    }
}

unsafe extern "C" fn stub_signature_help(
    _uri: LsStr,
    _line: u32,
    _character: u32,
    out: *mut LsBuf,
) -> i32 {
    let payload = SignatureHelp {
        signatures: vec![SignatureInfo {
            label: "f(x: Int): Int".to_string(),
            documentation: None,
            parameters: None,
            active_parameter: None,
        }],
        active_signature: Some(0),
        active_parameter: Some(0),
    }
    .encode()
    .unwrap();
    if unsafe { memory::write_response(&payload, out) } {
        STATUS_OK
    } else {
        STATUS_ALLOC
    }
}

unsafe extern "C" fn stub_definition(
    uri: LsStr,
    line: u32,
    _character: u32,
    out: *mut LsBuf,
) -> i32 {
    let payload = DefinitionResult {
        symbol: "sym".to_string(),
        locations: vec![Location {
            uri: unsafe { read_ls_str(uri) },
            range: Rng {
                start_line: line,
                start_character: 0,
                end_line: line,
                end_character: 4,
            },
            origin: origin::WORKSPACE,
        }],
    }
    .encode()
    .unwrap();
    if unsafe { memory::write_response(&payload, out) } {
        STATUS_OK
    } else {
        STATUS_ALLOC
    }
}

unsafe extern "C" fn stub_prepare_rename_null(
    _uri: LsStr,
    _line: u32,
    _character: u32,
    out: *mut LsBuf,
) -> i32 {
    let payload = PrepareRenameResult(None).encode().unwrap();
    if unsafe { memory::write_response(&payload, out) } {
        STATUS_OK
    } else {
        STATUS_ALLOC
    }
}

unsafe extern "C" fn stub_resolve(
    _target_id: LsStr,
    _symbol: LsStr,
    item_ptr: *const u8,
    item_len: u32,
    out: *mut LsBuf,
) -> i32 {
    let bytes = if item_ptr.is_null() {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(item_ptr, item_len as usize) }
    };
    let mut item = match CompletionItem::decode(bytes) {
        Ok(item) => item,
        Err(_) => return ls_pc_abi::STATUS_DECODE,
    };
    // Enrich the item, as a real resolve would.
    item.documentation = Some(Documentation::Plain("resolved".to_string()));
    let payload = item.encode().unwrap();
    if unsafe { memory::write_response(&payload, out) } {
        STATUS_OK
    } else {
        STATUS_ALLOC
    }
}

unsafe extern "C" fn stub_plugin_status(out: *mut LsBuf) -> i32 {
    let payload = PluginStatus {
        compiler_plugins: vec![CompilerPlugin {
            jars: vec!["a.jar".to_string()],
            options: vec![],
            loaded: true,
            detail: "ok".to_string(),
        }],
        service_plugins: vec![ServicePlugin {
            id: "svc".to_string(),
            source: "builtin".to_string(),
            enabled: true,
            self_test_ok: true,
            self_test_detail: String::new(),
        }],
        disabled: vec![DisabledPlugin {
            id: "old".to_string(),
            reason: "superseded".to_string(),
        }],
    }
    .encode()
    .unwrap();
    if unsafe { memory::write_response(&payload, out) } {
        STATUS_OK
    } else {
        STATUS_ALLOC
    }
}

unsafe extern "C" fn stub_void() -> i32 {
    STATUS_OK
}

unsafe extern "C" fn stub_spawn_dispatch(_generation: u32) -> i32 {
    STATUS_OK
}

// --- payload-query stub ops (ABI v2): decode the params, answer a canned ----
// --- typed result derived from them, so both directions cross the slot.  ----

fn payload_request(ptr: *const u8, len: u32) -> Vec<u8> {
    if ptr.is_null() {
        return Vec::new();
    }
    unsafe { std::slice::from_raw_parts(ptr, len as usize) }.to_vec()
}

fn respond(payload: Vec<u8>, out: *mut LsBuf) -> i32 {
    if unsafe { memory::write_response(&payload, out) } {
        STATUS_OK
    } else {
        STATUS_ALLOC
    }
}

unsafe extern "C" fn stub_inlay_hints(ptr: *const u8, len: u32, out: *mut LsBuf) -> i32 {
    let params = match InlayHintParams::decode(&payload_request(ptr, len)) {
        Ok(params) => params,
        Err(_) => return ls_pc_abi::STATUS_DECODE,
    };
    let payload = InlayHintsResult {
        hints: vec![InlayHint {
            position: Pos {
                line: params.range.start_line,
                character: params.flags,
            },
            label_parts: vec![InlayLabelPart {
                text: ": Int".to_string(),
                location: Some((params.uri, Rng::default())),
                tooltip: None,
            }],
            kind: 1,
            padding_left: true,
            padding_right: false,
            text_edits: None,
            data: Some(vec![0xde, 0xad]),
        }],
    }
    .encode()
    .unwrap();
    respond(payload, out)
}

unsafe extern "C" fn stub_semantic_tokens(ptr: *const u8, len: u32, out: *mut LsBuf) -> i32 {
    let params = match UriParams::decode(&payload_request(ptr, len)) {
        Ok(params) => params,
        Err(_) => return ls_pc_abi::STATUS_DECODE,
    };
    let payload = SemanticTokensResult {
        nodes: vec![SemanticNode {
            start: 0,
            end: params.uri.len() as u32,
            token_type: 3,
            token_modifier: 1,
        }],
    }
    .encode()
    .unwrap();
    respond(payload, out)
}

unsafe extern "C" fn stub_selection_range(ptr: *const u8, len: u32, out: *mut LsBuf) -> i32 {
    let params = match SelectionRangeParams::decode(&payload_request(ptr, len)) {
        Ok(params) => params,
        Err(_) => return ls_pc_abi::STATUS_DECODE,
    };
    // Per position: an innermost-first chain of one range around the position.
    let payload = SelectionRangesResult {
        chains: params
            .positions
            .iter()
            .map(|pos| {
                vec![Rng {
                    start_line: pos.line,
                    start_character: pos.character,
                    end_line: pos.line,
                    end_character: pos.character + 1,
                }]
            })
            .collect(),
    }
    .encode()
    .unwrap();
    respond(payload, out)
}

unsafe extern "C" fn stub_code_action(ptr: *const u8, len: u32, out: *mut LsBuf) -> i32 {
    let params = match CodeActionParams::decode(&payload_request(ptr, len)) {
        Ok(params) => params,
        Err(_) => return ls_pc_abi::STATUS_DECODE,
    };
    // An extract-method refusal is DATA on STATUS_OK, not an error status.
    let payload = if params.action == code_action_id::EXTRACT_METHOD {
        CodeActionResult {
            edits: Vec::new(),
            refusal: Some("Cannot extract selection".to_string()),
        }
    } else {
        CodeActionResult {
            edits: vec![TextEdit {
                range: Rng {
                    start_line: params.position.line,
                    start_character: params.position.character,
                    end_line: params.position.line,
                    end_character: params.position.character,
                },
                new_text: ": Int".to_string(),
            }],
            refusal: None,
        }
    }
    .encode()
    .unwrap();
    respond(payload, out)
}

unsafe extern "C" fn stub_auto_imports(ptr: *const u8, len: u32, out: *mut LsBuf) -> i32 {
    let params = match AutoImportParams::decode(&payload_request(ptr, len)) {
        Ok(params) => params,
        Err(_) => return ls_pc_abi::STATUS_DECODE,
    };
    let payload = AutoImportsResult {
        imports: vec![AutoImport {
            package_name: "scala.concurrent".to_string(),
            edits: vec![TextEdit {
                range: Rng::default(),
                new_text: format!("import scala.concurrent.{}\n", params.name),
            }],
            symbol: Some(format!("scala/concurrent/{}#", params.name)),
        }],
    }
    .encode()
    .unwrap();
    respond(payload, out)
}

unsafe extern "C" fn stub_pc_diagnostics(ptr: *const u8, len: u32, out: *mut LsBuf) -> i32 {
    if UriParams::decode(&payload_request(ptr, len)).is_err() {
        return ls_pc_abi::STATUS_DECODE;
    }
    let payload = PcDiagnosticsResult {
        diagnostics: vec![PcDiagnostic {
            range: Rng {
                start_line: 3,
                start_character: 0,
                end_line: 3,
                end_character: 5,
            },
            severity: 1,
            code: "E007".to_string(),
            message: "not found: value x".to_string(),
        }],
    }
    .encode()
    .unwrap();
    respond(payload, out)
}

unsafe extern "C" fn stub_folding_range(ptr: *const u8, len: u32, out: *mut LsBuf) -> i32 {
    if UriParams::decode(&payload_request(ptr, len)).is_err() {
        return ls_pc_abi::STATUS_DECODE;
    }
    let payload = FoldingRangesResult {
        ranges: vec![FoldingRange {
            range: Rng {
                start_line: 0,
                start_character: 0,
                end_line: 5,
                end_character: 1,
            },
            kind: folding_kind::IMPORTS,
        }],
    }
    .encode()
    .unwrap();
    respond(payload, out)
}

fn build_pc_vtable() -> PcVtable {
    PcVtable {
        abi_version: ABI_VERSION,
        register_target: stub_request as PcRequestFn,
        did_open: stub_request as PcRequestFn,
        did_change: stub_request as PcRequestFn,
        did_close: stub_uri as PcUriFn,
        completion: stub_completion as PcQueryFn,
        completion_resolve: stub_resolve as PcResolveFn,
        hover: stub_hover_null as PcQueryFn,
        signature_help: stub_signature_help as PcQueryFn,
        definition: stub_definition as PcQueryFn,
        type_definition: stub_definition as PcQueryFn,
        prepare_rename: stub_prepare_rename_null as PcQueryFn,
        plugin_status: stub_plugin_status as PcStatusOutFn,
        restart_instances: stub_void as PcVoidFn,
        shutdown: stub_void as PcVoidFn,
        spawn_dispatch: stub_spawn_dispatch,
        inlay_hints: stub_inlay_hints as PcPayloadQueryFn,
        semantic_tokens: stub_semantic_tokens as PcPayloadQueryFn,
        selection_range: stub_selection_range as PcPayloadQueryFn,
        code_action: stub_code_action as PcPayloadQueryFn,
        auto_imports: stub_auto_imports as PcPayloadQueryFn,
        pc_diagnostics: stub_pc_diagnostics as PcPayloadQueryFn,
        folding_range: stub_folding_range as PcPayloadQueryFn,
    }
}

// --- stub Rust vtable ------------------------------------------------------

unsafe extern "C" fn stub_log(_level: i32, _ptr: *const u8, _len: u32) {}

unsafe extern "C" fn stub_register_pc_vtable(_pc: *const PcVtable) -> i32 {
    STATUS_OK
}

unsafe extern "C" fn stub_pc_dispatch_loop(_worker_index: i32) {}

unsafe extern "C" fn stub_symbol_definition(
    symbol: LsStr,
    _from_uri: LsStr,
    out: *mut LsBuf,
) -> i32 {
    let payload = LocationsResult {
        locations: vec![Location {
            uri: format!("file:///defs/{}.scala", unsafe { read_ls_str(symbol) }),
            range: Rng::default(),
            origin: origin::WORKSPACE,
        }],
    }
    .encode()
    .unwrap();
    if unsafe { memory::write_response(&payload, out) } {
        STATUS_OK
    } else {
        STATUS_ALLOC
    }
}

unsafe extern "C" fn stub_definition_source_toplevels(
    symbol: LsStr,
    source_uri: LsStr,
    out: *mut LsBuf,
) -> i32 {
    let payload = ToplevelsResult {
        symbols: vec![
            format!("{}.", unsafe { read_ls_str(symbol) }),
            format!("toplevels-of:{}", unsafe { read_ls_str(source_uri) }),
        ],
    }
    .encode()
    .unwrap();
    if unsafe { memory::write_response(&payload, out) } {
        STATUS_OK
    } else {
        STATUS_ALLOC
    }
}

unsafe extern "C" fn stub_search_methods(query: LsStr, target: LsStr, out: *mut LsBuf) -> i32 {
    let payload = MethodHitsResult {
        hits: vec![MethodHit {
            uri: format!("file:///methods/{}.scala", unsafe { read_ls_str(target) }),
            symbol: format!("pkg/Ops.{}().", unsafe { read_ls_str(query) }),
            kind: 3,
            range: Rng::default(),
        }],
    }
    .encode()
    .unwrap();
    if unsafe { memory::write_response(&payload, out) } {
        STATUS_OK
    } else {
        STATUS_ALLOC
    }
}

fn build_rust_vtable() -> RustVtable {
    RustVtable {
        abi_version: ABI_VERSION,
        layout_canary: LAYOUT_CANARY,
        alloc: memory::abi_alloc,
        free: memory::abi_free,
        log: stub_log,
        register_pc_vtable: stub_register_pc_vtable,
        pc_dispatch_loop: stub_pc_dispatch_loop,
        symbol_definition: stub_symbol_definition,
        search_methods: stub_search_methods,
        definition_source_toplevels: stub_definition_source_toplevels,
    }
}

// --- tests -----------------------------------------------------------------

#[test]
fn vtable_layouts_match_the_canary_contract() {
    assert_eq!(size_of::<PcVtable>(), 184);
    assert_eq!(size_of::<RustVtable>(), 80);
    assert_eq!(LAYOUT_CANARY, compute_layout_canary());
    assert_ne!(LAYOUT_CANARY, 0);
    assert_eq!(ABI_VERSION, 2);
    // The tables carry the version the two sides check at registration.
    assert_eq!(build_pc_vtable().abi_version, ABI_VERSION);
    let rust = build_rust_vtable();
    assert_eq!(rust.abi_version, ABI_VERSION);
    assert_eq!(rust.layout_canary, compute_layout_canary());
}

#[test]
fn boundary_calls_round_trip_and_free_without_leaking() {
    let pc = build_pc_vtable();
    let rust = build_rust_vtable();
    let uri = "file:///x.scala";

    assert_eq!(
        memory::live_allocations(),
        0,
        "another test dirtied the allocator"
    );

    for i in 0..400u32 {
        // completion — verifies argument marshalling through the slot.
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (pc.completion)(ls_str(uri), i, i + 1, &mut out) },
            STATUS_OK
        );
        let list = CompletionList::decode(unsafe { as_slice(&out) }).unwrap();
        assert_eq!(list.items[0].label, format!("{uri}:{i}:{}", i + 1));
        unsafe { (rust.free)(out.ptr, out.len) };

        // hover — a null result still round-trips as a real buffer.
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (pc.hover)(ls_str(uri), i, i, &mut out) },
            STATUS_OK
        );
        assert_eq!(
            HoverResult::decode(unsafe { as_slice(&out) }).unwrap(),
            HoverResult(None)
        );
        unsafe { (rust.free)(out.ptr, out.len) };

        // definition — origin-tagged location survives the round trip.
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (pc.definition)(ls_str(uri), i, 0, &mut out) },
            STATUS_OK
        );
        let def = DefinitionResult::decode(unsafe { as_slice(&out) }).unwrap();
        assert_eq!(def.locations[0].origin, origin::WORKSPACE);
        unsafe { (rust.free)(out.ptr, out.len) };

        // prepare_rename null + signature help + plugin status.
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (pc.prepare_rename)(ls_str(uri), i, i, &mut out) },
            STATUS_OK
        );
        assert_eq!(
            PrepareRenameResult::decode(unsafe { as_slice(&out) }).unwrap(),
            PrepareRenameResult(None)
        );
        unsafe { (rust.free)(out.ptr, out.len) };

        let mut out = empty_buf();
        assert_eq!(
            unsafe { (pc.signature_help)(ls_str(uri), i, i, &mut out) },
            STATUS_OK
        );
        assert!(!SignatureHelp::decode(unsafe { as_slice(&out) })
            .unwrap()
            .signatures
            .is_empty());
        unsafe { (rust.free)(out.ptr, out.len) };

        let mut out = empty_buf();
        assert_eq!(unsafe { (pc.plugin_status)(&mut out) }, STATUS_OK);
        assert!(!PluginStatus::decode(unsafe { as_slice(&out) })
            .unwrap()
            .compiler_plugins
            .is_empty());
        unsafe { (rust.free)(out.ptr, out.len) };

        // completion_resolve — the encoded item argument decodes and re-encodes.
        let item_buf = single_item_list("m".to_string())
            .items
            .remove(0)
            .encode()
            .unwrap();
        let mut out = empty_buf();
        assert_eq!(
            unsafe {
                (pc.completion_resolve)(
                    ls_str("root/t"),
                    ls_str("sym"),
                    item_buf.as_ptr(),
                    item_buf.len() as u32,
                    &mut out,
                )
            },
            STATUS_OK
        );
        let resolved = CompletionItem::decode(unsafe { as_slice(&out) }).unwrap();
        assert_eq!(
            resolved.documentation,
            Some(Documentation::Plain("resolved".to_string()))
        );
        unsafe { (rust.free)(out.ptr, out.len) };

        // symbol_definition callback through the Rust vtable.
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (rust.symbol_definition)(ls_str("Foo"), ls_str(uri), &mut out) },
            STATUS_OK
        );
        assert!(!LocationsResult::decode(unsafe { as_slice(&out) })
            .unwrap()
            .locations
            .is_empty());
        unsafe { (rust.free)(out.ptr, out.len) };

        // search_methods callback through the Rust vtable — the query/target
        // arguments marshal through the slot and the hits decode back.
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (rust.search_methods)(ls_str("incr"), ls_str("root/t"), &mut out) },
            STATUS_OK
        );
        let hits = MethodHitsResult::decode(unsafe { as_slice(&out) }).unwrap();
        assert_eq!(hits.hits[0].symbol, "pkg/Ops.incr().");
        assert_eq!(hits.hits[0].uri, "file:///methods/root/t.scala");
        assert_eq!(hits.hits[0].kind, 3);
        unsafe { (rust.free)(out.ptr, out.len) };

        // inlay_hints — the payload-in/payload-out slot shape: the encoded
        // params cross in, the typed hints (opaque data bytes intact) come back.
        let params = InlayHintParams {
            uri: uri.to_string(),
            range: Rng {
                start_line: i,
                start_character: 0,
                end_line: i + 5,
                end_character: 0,
            },
            flags: 3,
        }
        .encode()
        .unwrap();
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (pc.inlay_hints)(params.as_ptr(), params.len() as u32, &mut out) },
            STATUS_OK
        );
        let hints = InlayHintsResult::decode(unsafe { as_slice(&out) }).unwrap();
        assert_eq!(hints.hints[0].position.line, i);
        assert_eq!(hints.hints[0].position.character, 3);
        assert_eq!(
            hints.hints[0].label_parts[0].location.as_ref().unwrap().0,
            uri
        );
        assert_eq!(hints.hints[0].data.as_deref(), Some(&[0xde, 0xad][..]));
        unsafe { (rust.free)(out.ptr, out.len) };

        // semantic_tokens — uri-only params, offset-based nodes back.
        let params = UriParams {
            uri: uri.to_string(),
        }
        .encode()
        .unwrap();
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (pc.semantic_tokens)(params.as_ptr(), params.len() as u32, &mut out) },
            STATUS_OK
        );
        let tokens = SemanticTokensResult::decode(unsafe { as_slice(&out) }).unwrap();
        assert_eq!(tokens.nodes[0].end, uri.len() as u32);
        assert_eq!(tokens.nodes[0].token_type, 3);
        unsafe { (rust.free)(out.ptr, out.len) };

        // selection_range — a chain per query position, innermost first.
        let params = SelectionRangeParams {
            uri: uri.to_string(),
            positions: vec![
                Pos {
                    line: i,
                    character: 2,
                },
                Pos {
                    line: i + 1,
                    character: 0,
                },
            ],
        }
        .encode()
        .unwrap();
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (pc.selection_range)(params.as_ptr(), params.len() as u32, &mut out) },
            STATUS_OK
        );
        let chains = SelectionRangesResult::decode(unsafe { as_slice(&out) }).unwrap();
        assert_eq!(chains.chains.len(), 2);
        assert_eq!(chains.chains[0][0].start_line, i);
        unsafe { (rust.free)(out.ptr, out.len) };

        // code_action — the typed-refusal answer is DATA on STATUS_OK.
        let params = CodeActionParams {
            uri: uri.to_string(),
            action: code_action_id::EXTRACT_METHOD,
            position: Pos {
                line: i,
                character: 1,
            },
            extraction_end: Some(Pos {
                line: i + 2,
                character: 0,
            }),
            arg_indices: None,
        }
        .encode()
        .unwrap();
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (pc.code_action)(params.as_ptr(), params.len() as u32, &mut out) },
            STATUS_OK
        );
        let refused = CodeActionResult::decode(unsafe { as_slice(&out) }).unwrap();
        assert!(refused.edits.is_empty());
        assert_eq!(refused.refusal.as_deref(), Some("Cannot extract selection"));
        unsafe { (rust.free)(out.ptr, out.len) };

        let params = CodeActionParams {
            uri: uri.to_string(),
            action: code_action_id::INSERT_INFERRED_TYPE,
            position: Pos {
                line: i,
                character: 4,
            },
            extraction_end: None,
            arg_indices: Some(vec![0, 2]),
        }
        .encode()
        .unwrap();
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (pc.code_action)(params.as_ptr(), params.len() as u32, &mut out) },
            STATUS_OK
        );
        let acted = CodeActionResult::decode(unsafe { as_slice(&out) }).unwrap();
        assert_eq!(acted.edits[0].new_text, ": Int");
        assert_eq!(acted.edits[0].range.start_line, i);
        assert_eq!(acted.refusal, None);
        unsafe { (rust.free)(out.ptr, out.len) };

        // auto_imports — name + position params, candidate edits back.
        let params = AutoImportParams {
            uri: uri.to_string(),
            position: Pos {
                line: i,
                character: 9,
            },
            name: "Future".to_string(),
            is_extension: false,
        }
        .encode()
        .unwrap();
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (pc.auto_imports)(params.as_ptr(), params.len() as u32, &mut out) },
            STATUS_OK
        );
        let imports = AutoImportsResult::decode(unsafe { as_slice(&out) }).unwrap();
        assert_eq!(imports.imports[0].package_name, "scala.concurrent");
        assert_eq!(
            imports.imports[0].symbol.as_deref(),
            Some("scala/concurrent/Future#")
        );
        unsafe { (rust.free)(out.ptr, out.len) };

        // pc_diagnostics — uri-only params, reduced diagnostic records back.
        let params = UriParams {
            uri: uri.to_string(),
        }
        .encode()
        .unwrap();
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (pc.pc_diagnostics)(params.as_ptr(), params.len() as u32, &mut out) },
            STATUS_OK
        );
        let diags = PcDiagnosticsResult::decode(unsafe { as_slice(&out) }).unwrap();
        assert_eq!(diags.diagnostics[0].code, "E007");
        assert_eq!(diags.diagnostics[0].severity, 1);
        unsafe { (rust.free)(out.ptr, out.len) };

        // folding_range — uri-only params, kind-tagged ranges back.
        let mut out = empty_buf();
        assert_eq!(
            unsafe { (pc.folding_range)(params.as_ptr(), params.len() as u32, &mut out) },
            STATUS_OK
        );
        let folds = FoldingRangesResult::decode(unsafe { as_slice(&out) }).unwrap();
        assert_eq!(folds.ranges[0].kind, folding_kind::IMPORTS);
        unsafe { (rust.free)(out.ptr, out.len) };

        // definition_source_toplevels callback through the Rust vtable — both
        // string arguments marshal through the slot and the symbols decode back.
        let mut out = empty_buf();
        assert_eq!(
            unsafe {
                (rust.definition_source_toplevels)(ls_str("a/b/Main#"), ls_str(uri), &mut out)
            },
            STATUS_OK
        );
        let toplevels = ToplevelsResult::decode(unsafe { as_slice(&out) }).unwrap();
        assert_eq!(
            toplevels.symbols,
            vec!["a/b/Main#.".to_string(), format!("toplevels-of:{uri}"),]
        );
        unsafe { (rust.free)(out.ptr, out.len) };

        // Lifecycle ops carry no payload.
        assert_eq!(unsafe { (pc.register_target)(ptr::null(), 0) }, STATUS_OK);
        assert_eq!(unsafe { (pc.did_close)(ls_str(uri)) }, STATUS_OK);
        assert_eq!(unsafe { (pc.restart_instances)() }, STATUS_OK);
        assert_eq!(unsafe { (pc.spawn_dispatch)(i) }, STATUS_OK);
    }

    assert_eq!(
        memory::live_allocations(),
        0,
        "response buffers leaked across the boundary"
    );

    // The counter is not trivially zero: an unfreed buffer is observable, then
    // reclaimed. This proves the leak assertion above has teeth.
    let mut leaked = empty_buf();
    let params = ResolveParams {
        target_id: "t".to_string(),
        symbol: "s".to_string(),
        item: single_item_list("x".to_string()).items.remove(0),
    };
    let item_buf = params.item.encode().unwrap();
    assert_eq!(
        unsafe {
            (pc.completion_resolve)(
                ls_str(&params.target_id),
                ls_str(&params.symbol),
                item_buf.as_ptr(),
                item_buf.len() as u32,
                &mut leaked,
            )
        },
        STATUS_OK
    );
    assert_eq!(memory::live_allocations(), 1);
    unsafe { (rust.free)(leaked.ptr, leaked.len) };
    assert_eq!(memory::live_allocations(), 0);
}
