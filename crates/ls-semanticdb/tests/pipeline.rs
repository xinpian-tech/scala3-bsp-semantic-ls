//! End-to-end ingest over real encoded protobuf bytes (the production decoder,
//! not a shortcut), plus the md5-mismatch and malformed-payload negatives. This
//! exercises the same parse -> normalize -> assemble path as pinned-scalac
//! fixtures would, deterministically.

mod common;

use common::{doc, occ, range, sym};
use ls_index_model::{unsafe_reason, DocId, Role, SymbolKey};
use ls_semanticdb::model::{sdb_role, SdbDocument};
use ls_semanticdb::{md5, normalize, parse_text_documents, FreshnessCheck, SemanticBatch};

// SemanticDB kind codes.
const K_OBJECT: i32 = 10;
const K_CLASS: i32 = 13;
const K_CONSTRUCTOR: i32 = 21;

fn gk(s: &str) -> SymbolKey {
    SymbolKey::global(s)
}

/// A two-document `case class X()` program plus a use site referencing an
/// external symbol.
fn program() -> Vec<SdbDocument> {
    let defs = doc(
        4,
        "file:///X.scala",
        "case class X()",
        &md5::compute_hex("case class X()"),
        1,
        vec![
            sym("a/X#", K_CLASS, 0x80, "X", &[]),
            sym("a/X.", K_OBJECT, 0, "X", &[]),
            sym("a/X#`<init>`().", K_CONSTRUCTOR, 0x2000, "<init>", &[]),
        ],
        vec![
            occ(Some(range(0, 11, 0, 12)), "a/X#", sdb_role::DEFINITION),
            occ(Some(range(0, 11, 0, 12)), "a/X.", sdb_role::DEFINITION),
            occ(
                Some(range(0, 11, 0, 12)),
                "a/X#`<init>`().",
                sdb_role::DEFINITION,
            ),
        ],
    );
    let uses = doc(
        4,
        "file:///Use.scala",
        "val x = X()",
        &md5::compute_hex("val x = X()"),
        1,
        vec![sym("a/Use.", K_OBJECT, 0, "Use", &[])],
        vec![
            occ(Some(range(0, 8, 0, 9)), "a/X#", sdb_role::REFERENCE),
            occ(
                Some(range(0, 8, 0, 9)),
                "a/X#`<init>`().",
                sdb_role::REFERENCE,
            ),
            occ(Some(range(0, 8, 0, 11)), "scala/Int#", sdb_role::REFERENCE),
        ],
    );
    vec![defs, uses]
}

#[test]
fn end_to_end_parse_normalize_assemble() {
    // Encode -> decode with the production parser -> normalize -> assemble.
    let bytes = common::encode(&program(), false);
    let parsed = parse_text_documents(&bytes).expect("parse");
    let normalized: Vec<_> = parsed
        .documents
        .iter()
        .enumerate()
        .map(|(i, d)| normalize(d, DocId::new(i as u64)))
        .collect();
    let batch = SemanticBatch::assemble(normalized);

    // Class, companion object, and constructor share one ref group.
    let g = batch.ref_group_of(&gk("a/X#"));
    assert!(g.is_some());
    assert_eq!(batch.ref_group_of(&gk("a/X.")), g);
    assert_eq!(batch.ref_group_of(&gk("a/X#`<init>`().")), g);

    // That group is a safe, companion-bearing rename target.
    let profile = batch.rename_profile_of(&gk("a/X#")).unwrap();
    assert!(profile.has_companion);
    assert!(!profile.is_external);
    assert_eq!(profile.unsafe_reason_mask, 0);
    // 3 defs + 2 refs of members in the group, all editable.
    assert_eq!(profile.editable_occurrence_count, 5);

    // The external, reference-only symbol is a separate, unsafe group.
    let ext = batch.rename_profile_of(&gk("scala/Int#")).unwrap();
    assert!(ext.is_external);
    assert_ne!(batch.ref_group_of(&gk("scala/Int#")), g);
    assert_eq!(
        ext.unsafe_reason_mask & unsafe_reason::EXTERNAL,
        unsafe_reason::EXTERNAL
    );
}

#[test]
fn normalize_drops_unknown_role_and_rangeless_occurrences() {
    let d = doc(
        4,
        "file:///N.scala",
        "",
        "",
        1,
        vec![
            sym("a/N#", K_CLASS, 0, "N", &[]),
            sym("local1", 0, 0, "x", &[]),
        ],
        vec![
            occ(Some(range(0, 0, 0, 1)), "a/N#", sdb_role::DEFINITION),
            occ(Some(range(1, 0, 1, 1)), "a/N#", sdb_role::UNKNOWN_ROLE), // unknown role
            occ(None, "a/N#", sdb_role::REFERENCE),                       // no range
            occ(Some(range(2, 0, 2, 1)), "", sdb_role::REFERENCE),        // empty symbol
            occ(Some(range(3, 0, 3, 1)), "local1", sdb_role::REFERENCE),  // local -> local key
        ],
    );
    let bytes = common::encode(&[d], false);
    let parsed = parse_text_documents(&bytes).expect("parse");
    let n = normalize(&parsed.documents[0], DocId::new(7));

    // Only the definition and the local reference survive.
    assert_eq!(n.occurrences.len(), 2);
    assert_eq!(n.occurrences[0].key, gk("a/N#"));
    assert_eq!(n.occurrences[0].role, Role::Definition);
    // Local symbols carry the caller DocId.
    assert_eq!(
        n.occurrences[1].key,
        SymbolKey::local("local1", DocId::new(7))
    );
    assert!(n.occurrences[1].key.is_local());
}

#[test]
fn md5_mismatch_recorded_stale_never_fresh() {
    let bytes = common::encode(&program(), false);
    let parsed = parse_text_documents(&bytes).expect("parse");
    let x_doc = &parsed.documents[0];
    // Fresh against the exact source; stale against changed source.
    assert_eq!(
        md5::validate_doc("case class X()", x_doc),
        FreshnessCheck::Fresh
    );
    let stale = md5::validate_doc("case class Y()", x_doc);
    assert!(!stale.is_fresh());
    assert!(matches!(stale, FreshnessCheck::Stale { .. }));
}

#[test]
fn malformed_payload_is_typed_error_not_panic() {
    // A length-delimited field (field 1, wire 2) declaring more bytes than exist.
    let garbage = [0x0au8, 0x7f, 0x01, 0x02];
    let err = parse_text_documents(&garbage).unwrap_err();
    assert!(matches!(err, ls_semanticdb::SemanticdbError::Parse(_)));
    // Random high bytes also fail cleanly rather than panicking.
    assert!(parse_text_documents(&[0xff, 0xff, 0xff, 0xff]).is_err());
}
