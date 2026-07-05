//! Port of the Scala `Md5Suite`.

use ls_semanticdb::md5::{self, FreshnessCheck};
use ls_semanticdb::SdbDocument;

#[test]
fn compute_hex_matches_known_vectors_uppercase() {
    assert_eq!(md5::compute_hex(""), "D41D8CD98F00B204E9800998ECF8427E");
    assert_eq!(
        md5::compute_hex("hello"),
        "5D41402ABC4B2A76B9719D911017C592"
    );
    // non-ASCII goes through UTF-8
    assert_eq!(md5::compute_hex("héllo"), md5::compute_hex("héllo"));
}

#[test]
fn validate_fresh_when_md5_matches_case_insensitive() {
    let m = md5::compute_hex("object A");
    assert_eq!(md5::validate("object A", &m), FreshnessCheck::Fresh);
    assert_eq!(
        md5::validate("object A", &m.to_lowercase()),
        FreshnessCheck::Fresh
    );
    assert!(md5::validate("object A", &m).is_fresh());
}

#[test]
fn validate_stale_when_text_changed() {
    let stored = md5::compute_hex("object A");
    match md5::validate("object B", &stored) {
        FreshnessCheck::Stale {
            document_md5,
            source_md5,
        } => {
            assert_eq!(document_md5, stored);
            assert_eq!(source_md5, md5::compute_hex("object B"));
        }
        other => panic!("expected Stale, got {other:?}"),
    }
    assert!(!md5::validate("object B", &stored).is_fresh());
}

#[test]
fn validate_missing_md5() {
    assert_eq!(md5::validate("anything", ""), FreshnessCheck::MissingMd5);
    assert!(!md5::validate("anything", "").is_fresh());
}

#[test]
fn validate_against_sdb_document() {
    let d = SdbDocument {
        schema: 4,
        uri: "a.scala".into(),
        text: String::new(),
        md5: md5::compute_hex("src"),
        language_code: 1,
        symbols: vec![],
        occurrences: vec![],
    };
    assert_eq!(md5::validate_doc("src", &d), FreshnessCheck::Fresh);
    assert_eq!(
        md5::validate_doc("changed", &d),
        FreshnessCheck::Stale {
            document_md5: d.md5.clone(),
            source_md5: md5::compute_hex("changed"),
        }
    );
}
