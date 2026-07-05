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
    PcQueryFn, PcRequestFn, PcResolveFn, PcStatusOutFn, PcUriFn, PcVoidFn, STATUS_ALLOC, STATUS_OK,
};
use ls_pc_abi::payloads::{
    origin, CompilerPlugin, CompletionItem, CompletionList, DefinitionResult, DisabledPlugin,
    Documentation, HoverResult, Location, LocationsResult, PluginStatus, PrepareRenameResult,
    ResolveParams, Rng, ServicePlugin, SignatureHelp, SignatureInfo,
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
    }
}

// --- tests -----------------------------------------------------------------

#[test]
fn vtable_layouts_match_the_canary_contract() {
    assert_eq!(size_of::<PcVtable>(), 128);
    assert_eq!(size_of::<RustVtable>(), 64);
    assert_eq!(LAYOUT_CANARY, compute_layout_canary());
    assert_ne!(LAYOUT_CANARY, 0);
    assert_eq!(ABI_VERSION, 1);
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
