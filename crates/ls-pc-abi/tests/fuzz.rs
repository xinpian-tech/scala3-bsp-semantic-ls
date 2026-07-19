//! Malformed input at the decode boundary must yield a typed [`AbiError`], never
//! a panic or out-of-bounds read: truncated buffers, corrupted envelopes,
//! fabricated list counts, out-of-range blob slices, invalid enum-variant tags,
//! and arbitrary bytes.

use ls_pc_abi::codec::{Reader, Writer, MAGIC};
use ls_pc_abi::payloads::{
    CompletionItem, CompletionList, DefinitionResult, DidChangeParams, DidOpenParams, HoverResult,
    LocationsResult, MethodHitsResult, PluginStatus, PositionParams, PrepareRenameResult,
    ResolveParams, SignatureHelp, TargetConfig,
};
use proptest::prelude::*;

/// Runs every decode entry point on `bytes`; any panic fails the test. Return
/// values are intentionally ignored — the guarantee under test is "no panic,
/// no out-of-bounds read", not a particular Ok/Err split.
fn decode_all(bytes: &[u8]) {
    let _ = TargetConfig::decode(bytes);
    let _ = DidOpenParams::decode(bytes);
    let _ = DidChangeParams::decode(bytes);
    let _ = PositionParams::decode(bytes);
    let _ = ResolveParams::decode(bytes);
    let _ = CompletionList::decode(bytes);
    let _ = CompletionItem::decode(bytes);
    let _ = HoverResult::decode(bytes);
    let _ = SignatureHelp::decode(bytes);
    let _ = DefinitionResult::decode(bytes);
    let _ = PrepareRenameResult::decode(bytes);
    let _ = PluginStatus::decode(bytes);
    let _ = LocationsResult::decode(bytes);
    let _ = MethodHitsResult::decode(bytes);
}

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
        data: Some(b"symbol".to_vec()),
    }
}

fn sample_completion_list() -> Vec<u8> {
    CompletionList {
        is_incomplete: true,
        item_defaults: None,
        apply_kind: None,
        items: vec![bare_item("hello")],
    }
    .encode()
    .unwrap()
}

#[test]
fn empty_and_short_buffers_are_rejected() {
    for len in 0..16usize {
        let bytes = vec![0u8; len];
        assert!(CompletionList::decode(&bytes).is_err());
        decode_all(&bytes);
    }
}

#[test]
fn bad_magic_is_rejected() {
    let mut buf = sample_completion_list();
    buf[0] ^= 0xff;
    assert!(CompletionList::decode(&buf).is_err());
}

#[test]
fn wrong_kind_is_rejected() {
    // A completion-list buffer decoded as a definition result: same envelope
    // shape, different kind tag.
    let buf = sample_completion_list();
    assert!(DefinitionResult::decode(&buf).is_err());
}

#[test]
fn length_mismatch_is_rejected() {
    let buf = sample_completion_list();
    // One byte short and one byte long both violate the exact-length envelope.
    assert!(CompletionList::decode(&buf[..buf.len() - 1]).is_err());
    let mut longer = buf.clone();
    longer.push(0);
    assert!(CompletionList::decode(&longer).is_err());
}

#[test]
fn fabricated_huge_count_is_rejected_without_allocating() {
    // Patch the item-count field to u32::MAX. The reader must reject it against
    // the remaining body rather than attempt a gigantic allocation. The count
    // follows the is_incomplete flag (4), the null itemDefaults flag (4), and
    // the null applyKind flag (4).
    let mut buf = sample_completion_list();
    let count_at = 16 + 12;
    buf[count_at..count_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
    assert!(CompletionList::decode(&buf).is_err());
}

#[test]
fn out_of_range_blob_offset_is_rejected() {
    // Hand-build a definition buffer whose symbol BlobStr points past the blob.
    let mut w = Writer::new();
    w.str("sym"); // body: offset=0, len=3
    w.u32(0); // zero locations
              // Kind 10 == KIND_DEFINITION (see payloads.rs); build it directly so we can
              // then corrupt the offset.
    let mut buf = w.finish(10).unwrap();
    // The symbol's offset field is the first body u32 (buffer offset 16).
    buf[16..20].copy_from_slice(&0xffff_ffffu32.to_le_bytes());
    assert!(DefinitionResult::decode(&buf).is_err());
}

#[test]
fn invalid_utf8_in_blob_is_rejected() {
    // A DidChange buffer whose uri bytes are not valid UTF-8.
    let mut w = Writer::new();
    w.str("\u{0}"); // uri: offset 0, len 1 — will be overwritten below
    w.str(""); // text
    let mut buf = w.finish(3).unwrap(); // KIND_DID_CHANGE
                                        // The blob is the trailing region; overwrite its first byte with 0xff.
    let blob_start = buf.len() - 1;
    buf[blob_start] = 0xff;
    assert!(DidChangeParams::decode(&buf).is_err());
}

#[test]
fn invalid_enum_variant_tag_is_rejected() {
    // A present hover whose contents variant tag is neither 0 (Markup) nor 1
    // (MarkedString). Body: present flag (u32=1) then the contents tag (u32).
    let buf = HoverResult(Some(ls_pc_abi::payloads::Hover {
        contents: ls_pc_abi::payloads::HoverContents::Markup(ls_pc_abi::payloads::MarkupContent {
            kind: "plaintext".to_string(),
            value: "x".to_string(),
        }),
        range: None,
    }))
    .encode()
    .unwrap();
    let mut buf = buf;
    let tag_at = 16 + 4;
    buf[tag_at..tag_at + 4].copy_from_slice(&99u32.to_le_bytes());
    assert!(HoverResult::decode(&buf).is_err());
}

#[test]
fn reader_rejects_trailing_body_bytes() {
    // A valid envelope whose body has an unconsumed tail.
    let mut w = Writer::new();
    w.u32(7);
    w.u32(9);
    let buf = w.finish(4).unwrap(); // KIND_POSITION expects uri + line + character
    assert!(PositionParams::decode(&buf).is_err());
    // And a reader constructed directly rejects the leftover tail.
    let mut reader = Reader::new(&buf, 4).unwrap();
    let _ = reader.u32().unwrap();
    assert!(reader.finish().is_err());
}

#[test]
fn magic_constant_is_stable() {
    // Guards against an accidental envelope-magic change (a silent ABI break).
    assert_eq!(MAGIC, 0x4241_504c);
}

proptest! {
    #[test]
    fn arbitrary_bytes_never_panic(bytes in proptest::collection::vec(any::<u8>(), 0..512)) {
        decode_all(&bytes);
    }

    #[test]
    fn valid_magic_prefixed_bytes_never_panic(rest in proptest::collection::vec(any::<u8>(), 0..512)) {
        // Prefix the real magic so the fuzzer reaches the body/blob decode paths
        // more often instead of bouncing off the magic check.
        let mut bytes = MAGIC.to_le_bytes().to_vec();
        bytes.extend_from_slice(&rest);
        decode_all(&bytes);
    }

    #[test]
    fn single_byte_corruption_of_valid_buffer_never_panics(
        labels in proptest::collection::vec(".*", 0..4),
        index in any::<prop::sample::Index>(),
        xor in 1u8..=255,
    ) {
        let list = CompletionList {
            is_incomplete: true,
            item_defaults: None,
            apply_kind: None,
            items: labels.iter().map(|l| bare_item(l)).collect(),
        };
        let mut buf = list.encode().unwrap();
        if !buf.is_empty() {
            let i = index.index(buf.len());
            buf[i] ^= xor;
            decode_all(&buf);
        }
    }
}
