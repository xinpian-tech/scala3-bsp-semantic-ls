//! Port of the Scala `WireDecoderSuite`: the hand-rolled protobuf decoder over
//! the SemanticDB `TextDocuments` subset, including legacy-group skipping,
//! negative int32, over-long/truncated varints, and proto3 defaults.

mod common;

use common::{doc, occ, range, sym, ProtoTestWriter};
use ls_semanticdb::model::{sdb_language, sdb_role};
use ls_semanticdb::{parse_text_documents, SdbDocument, SdbDocuments};

fn doc1() -> SdbDocument {
    doc(
        4,
        "src/A.scala",
        "object A:\n  def f = 1\n",
        "ABCDEF0123456789ABCDEF0123456789",
        sdb_language::SCALA,
        vec![
            sym("a/A.", 10, 0x8 | 0x4000, "A", &[]),
            // multi-byte varint properties: OPAQUE | INLINE = 0x220000
            sym(
                "a/A.f().",
                3,
                0x200000 | 0x20000,
                "f",
                &["a/B#f().", "a/C#f()."],
            ),
        ],
        vec![
            occ(Some(range(0, 7, 0, 8)), "a/A.", sdb_role::DEFINITION),
            // multi-byte varint line numbers
            occ(
                Some(range(12345, 40, 12345, 45)),
                "a/A.f().",
                sdb_role::REFERENCE,
            ),
            occ(None, "a/NoRange#", sdb_role::REFERENCE),
            occ(Some(range(0, 0, 0, 0)), "local0", sdb_role::DEFINITION),
        ],
    )
}

fn doc2() -> SdbDocument {
    doc(3, "src/B.scala", "", "", sdb_language::JAVA, vec![], vec![])
}

fn parse(bytes: &[u8]) -> SdbDocuments {
    parse_text_documents(bytes).expect("parse")
}

#[test]
fn round_trips_a_text_documents_payload() {
    let bytes = common::encode(&[doc1(), doc2()], false);
    assert_eq!(
        parse(&bytes),
        SdbDocuments {
            documents: vec![doc1(), doc2()]
        }
    );
}

#[test]
fn skips_unknown_fields_of_every_wire_type_diagnostics_and_synthetics() {
    let noisy = common::encode(&[doc1(), doc2()], true);
    let plain = common::encode(&[doc1(), doc2()], false);
    assert!(noisy.len() > plain.len(), "noise must actually add bytes");
    assert_eq!(
        parse(&noisy),
        SdbDocuments {
            documents: vec![doc1(), doc2()]
        }
    );
}

#[test]
fn decodes_negative_int32_range_values() {
    // Range fields are plain int32, so -1 rides as a 10-byte sign-extended
    // varint and must decode back to -1 (not zigzag 0).
    let d = doc(
        3,
        "src/B.scala",
        "",
        "",
        sdb_language::JAVA,
        vec![],
        vec![occ(Some(range(-1, 0, -1, 5)), "a/X#", sdb_role::REFERENCE)],
    );
    let parsed = parse(&common::encode(&[d], false));
    assert_eq!(
        parsed.documents[0].occurrences[0].range,
        Some(range(-1, 0, -1, 5))
    );
}

#[test]
fn empty_embedded_range_decodes_to_all_zeros() {
    let mut w = ProtoTestWriter::new();
    w.message_field(1, |dw| {
        dw.string_field(2, "u.scala");
        dw.message_field(6, |ow| {
            ow.message_field(1, |_| {}); // Range present but empty
            ow.string_field(2, "a/X#");
            ow.varint_field(3, 1);
        });
    });
    let parsed = parse(&w.bytes());
    assert_eq!(
        parsed.documents[0].occurrences,
        vec![occ(Some(range(0, 0, 0, 0)), "a/X#", sdb_role::REFERENCE)]
    );
}

#[test]
fn empty_payload_decodes_to_zero_documents() {
    assert_eq!(parse(&[]), SdbDocuments { documents: vec![] });
}

#[test]
fn proto3_defaults_keep_zero_values() {
    let mut w = ProtoTestWriter::new();
    w.message_field(1, |_| {}); // fully empty TextDocument
    let parsed = parse(&w.bytes());
    assert_eq!(
        parsed.documents,
        vec![doc(0, "", "", "", 0, vec![], vec![])]
    );
}

#[test]
fn truncated_varint_fails() {
    // tag says field 1 varint, then a continuation byte with no terminator
    assert!(parse_text_documents(&[0x08, 0x80]).is_err());
}

#[test]
fn over_long_varint_fails() {
    // tag (field 1, varint) + 10 continuation bytes + terminator: one byte
    // longer than the longest legal varint.
    let mut bytes = vec![0x08u8];
    bytes.extend_from_slice(&[0x80u8; 10]);
    bytes.push(0x01);
    assert!(parse_text_documents(&bytes).is_err());
}

#[test]
fn truncated_length_delimited_field_fails() {
    let mut w = ProtoTestWriter::new();
    w.write_tag(1, 2);
    w.write_raw_varint(100); // declares 100 bytes, provides none
    assert!(parse_text_documents(&w.bytes()).is_err());
}

#[test]
fn multi_byte_varint_boundary_values_survive_skipping() {
    // Unknown varint fields with i64::MAX and i64::MIN around real data.
    let mut w = ProtoTestWriter::new();
    w.varint_field(77, i64::MAX as u64);
    w.message_field(1, |dw| {
        dw.string_field(2, "x.scala");
    });
    w.varint_field(78, i64::MIN as u64);
    let parsed = parse(&w.bytes());
    let uris: Vec<&str> = parsed.documents.iter().map(|d| d.uri.as_str()).collect();
    assert_eq!(uris, vec!["x.scala"]);
}
