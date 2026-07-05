//! Port of the Scala `GroupsSuite`: the v1 alias-group policy and rename profiles
//! over hand-built normalized documents.

use std::collections::HashMap;

use ls_index_model::{
    sym_props, unsafe_reason, DocId, NormalizedDocument, Occurrence, Role, Span, SymKind,
    SymbolInfo, SymbolKey,
};
use ls_semanticdb::{symbols, DocFacts, SemanticBatch};

fn gk(sym: &str) -> SymbolKey {
    SymbolKey::global(sym)
}

fn info(sym: &str, kind: SymKind, props: u32, overridden: &[&str]) -> SymbolInfo {
    SymbolInfo {
        key: gk(sym),
        display_name: symbols::display_name(sym).unwrap_or_else(|| sym.to_string()),
        owner_name: symbols::owner_name(sym),
        package_name: symbols::package_name(sym),
        kind,
        properties: props,
        overridden_symbols: overridden.iter().map(|s| s.to_string()).collect(),
    }
}

fn linfo(sym: &str, doc_id: u64) -> SymbolInfo {
    SymbolInfo {
        key: SymbolKey::local(sym, DocId::new(doc_id)),
        display_name: sym.to_string(),
        owner_name: None,
        package_name: None,
        kind: SymKind::LocalValue,
        properties: 0,
        overridden_symbols: vec![],
    }
}

// Group membership depends only on keys, so a fixed span suffices.
fn occ(key: SymbolKey, role: Role) -> Occurrence {
    Occurrence::new(key, Span::new(1, 0, 1, 5), role)
}

fn doc(uri: &str, symbols: Vec<SymbolInfo>, occurrences: Vec<Occurrence>) -> NormalizedDocument {
    NormalizedDocument {
        uri: uri.into(),
        md5: "MD5".into(),
        schema_version: 4,
        language: "scala".into(),
        symbols,
        occurrences,
    }
}

const X: &str = "a/X#";
const XOBJ: &str = "a/X.";
const XCTOR: &str = "a/X#`<init>`().";
const XCTOR2: &str = "a/X#`<init>`(+1).";
const XAPPLY: &str = "a/X.apply().";
const XAPPLY2: &str = "a/X.apply(+1).";
const XUNAPPLY: &str = "a/X.unapply().";

fn companion_batch() -> SemanticBatch {
    let defs = doc(
        "a/X.scala",
        vec![
            info(X, SymKind::Class, sym_props::CASE, &[]),
            info(XOBJ, SymKind::Object, 0, &[]),
            info(XCTOR, SymKind::Constructor, sym_props::PRIMARY, &[]),
            info(XCTOR2, SymKind::Constructor, 0, &[]),
            info(XAPPLY, SymKind::Method, 0, &[]),
            info(XAPPLY2, SymKind::Method, 0, &[]),
            info(XUNAPPLY, SymKind::Method, 0, &[]),
        ],
        vec![
            occ(gk(X), Role::Definition),
            occ(gk(XOBJ), Role::Definition),
            occ(gk(XCTOR), Role::Definition),
            occ(gk(XCTOR2), Role::Definition),
            occ(gk(XAPPLY), Role::Definition),
            occ(gk(XAPPLY2), Role::Definition),
            occ(gk(XUNAPPLY), Role::Definition),
        ],
    );
    let use_doc = doc(
        "a/Use.scala",
        vec![],
        vec![
            occ(gk(X), Role::Reference),
            occ(gk(XCTOR2), Role::Reference),
            occ(gk(XAPPLY), Role::Reference),
        ],
    );
    SemanticBatch::assemble(vec![defs, use_doc])
}

#[test]
fn class_companion_ctors_apply_unapply_share_one_ref_group() {
    let batch = companion_batch();
    let expected = batch.ref_group_of(&gk(X));
    assert!(expected.is_some());
    for s in [XOBJ, XCTOR, XCTOR2, XAPPLY, XAPPLY2, XUNAPPLY] {
        assert_eq!(batch.ref_group_of(&gk(s)), expected, "{s}");
    }
    let expected_rename = batch.rename_group_of(&gk(X));
    for s in [XOBJ, XCTOR, XCTOR2, XAPPLY, XAPPLY2, XUNAPPLY] {
        assert_eq!(batch.rename_group_of(&gk(s)), expected_rename, "{s}");
    }
}

#[test]
fn companion_group_has_companion_and_is_safe() {
    let batch = companion_batch();
    let profile = batch.rename_profile_of(&gk(X)).unwrap();
    assert!(profile.has_companion);
    assert!(!profile.is_external);
    assert!(!profile.is_local);
    assert_eq!(profile.unsafe_reason_mask, 0);
    assert_eq!(profile.editable_occurrence_count, 10);
}

#[test]
fn var_getter_and_setter_merge_including_field_term() {
    let getter = "a/B#value().";
    let setter = "a/B#`value_=`().";
    let field_term = "a/C#count.";
    let field_setter = "a/C#`count_=`().";
    let batch = SemanticBatch::assemble(vec![doc(
        "a/B.scala",
        vec![
            info(getter, SymKind::Method, sym_props::VAR, &[]),
            info(setter, SymKind::Method, sym_props::VAR, &[]),
            info(field_term, SymKind::LocalVariable, sym_props::VAR, &[]),
            info(field_setter, SymKind::Method, sym_props::VAR, &[]),
        ],
        vec![
            occ(gk(getter), Role::Definition),
            occ(gk(setter), Role::Definition),
            occ(gk(field_term), Role::Definition),
            occ(gk(field_setter), Role::Definition),
        ],
    )]);
    assert_eq!(
        batch.ref_group_of(&gk(getter)),
        batch.ref_group_of(&gk(setter))
    );
    assert_eq!(
        batch.ref_group_of(&gk(field_term)),
        batch.ref_group_of(&gk(field_setter))
    );
    assert_ne!(
        batch.ref_group_of(&gk(getter)),
        batch.ref_group_of(&gk(field_term))
    );
}

#[test]
fn method_overloads_stay_separate() {
    let f0 = "a/O.f().";
    let f1 = "a/O.f(+1).";
    let batch = SemanticBatch::assemble(vec![doc(
        "a/O.scala",
        vec![
            info(f0, SymKind::Method, 0, &[]),
            info(f1, SymKind::Method, 0, &[]),
        ],
        vec![occ(gk(f0), Role::Definition), occ(gk(f1), Role::Definition)],
    )]);
    assert_ne!(batch.ref_group_of(&gk(f0)), batch.ref_group_of(&gk(f1)));
    assert_ne!(
        batch.rename_group_of(&gk(f0)),
        batch.rename_group_of(&gk(f1))
    );
}

#[test]
fn plain_object_does_not_merge_with_its_apply() {
    let obj = "a/P.";
    let apply_sym = "a/P.apply().";
    let batch = SemanticBatch::assemble(vec![doc(
        "a/P.scala",
        vec![
            info(obj, SymKind::Object, 0, &[]),
            info(apply_sym, SymKind::Method, 0, &[]),
        ],
        vec![
            occ(gk(obj), Role::Definition),
            occ(gk(apply_sym), Role::Definition),
        ],
    )]);
    assert_ne!(
        batch.ref_group_of(&gk(obj)),
        batch.ref_group_of(&gk(apply_sym))
    );
}

#[test]
fn constructor_only_batch_synthesizes_class_key() {
    let ctor = "q/Ext#`<init>`().";
    let batch = SemanticBatch::assemble(vec![doc(
        "q/U.scala",
        vec![],
        vec![occ(gk(ctor), Role::Reference)],
    )]);
    let class_group = batch.ref_group_of(&gk("q/Ext#"));
    assert!(class_group.is_some());
    assert_eq!(batch.ref_group_of(&gk(ctor)), class_group);
}

#[test]
fn identical_local_names_in_different_documents_stay_separate() {
    let d1 = doc(
        "a/F1.scala",
        vec![linfo("local0", 1)],
        vec![occ(
            SymbolKey::local("local0", DocId::new(1)),
            Role::Definition,
        )],
    );
    let d2 = doc(
        "a/F2.scala",
        vec![linfo("local0", 2)],
        vec![occ(
            SymbolKey::local("local0", DocId::new(2)),
            Role::Definition,
        )],
    );
    let batch = SemanticBatch::assemble(vec![d1, d2]);
    let g1 = batch.ref_group_of(&SymbolKey::local("local0", DocId::new(1)));
    let g2 = batch.ref_group_of(&SymbolKey::local("local0", DocId::new(2)));
    assert!(g1.is_some() && g2.is_some());
    assert_ne!(g1, g2);
    let p1 = batch
        .rename_profile_of(&SymbolKey::local("local0", DocId::new(1)))
        .unwrap();
    assert!(p1.is_local);
    assert_eq!(p1.unsafe_reason_mask, 0);
}

#[test]
fn override_family_flags_both_groups() {
    let base = "a/Animal#sound().";
    let imp = "a/Dog#sound().";
    let batch = SemanticBatch::assemble(vec![doc(
        "a/Animals.scala",
        vec![
            info("a/Animal#", SymKind::Trait, 0, &[]),
            info("a/Dog#", SymKind::Class, 0, &[]),
            info(base, SymKind::Method, sym_props::ABSTRACT, &[]),
            info(imp, SymKind::Method, 0, &[base]),
        ],
        vec![
            occ(gk("a/Animal#"), Role::Definition),
            occ(gk("a/Dog#"), Role::Definition),
            occ(gk(base), Role::Definition),
            occ(gk(imp), Role::Definition),
        ],
    )]);
    assert_ne!(
        batch.rename_group_of(&gk(base)),
        batch.rename_group_of(&gk(imp))
    );
    let base_profile = batch.rename_profile_of(&gk(base)).unwrap();
    let imp_profile = batch.rename_profile_of(&gk(imp)).unwrap();
    assert!(base_profile.has_override_family);
    assert!(imp_profile.has_override_family);
    assert_eq!(
        base_profile.unsafe_reason_mask,
        unsafe_reason::OVERRIDE_FAMILY
    );
    assert_eq!(
        imp_profile.unsafe_reason_mask,
        unsafe_reason::OVERRIDE_FAMILY
    );
    assert_eq!(
        batch
            .rename_profile_of(&gk("a/Animal#"))
            .unwrap()
            .unsafe_reason_mask,
        0
    );
}

#[test]
fn reference_only_symbols_are_external_and_unsafe() {
    let ext = "scala/Int#";
    let batch = SemanticBatch::assemble(vec![doc(
        "a/U.scala",
        vec![],
        vec![occ(gk(ext), Role::Reference)],
    )]);
    let profile = batch.rename_profile_of(&gk(ext)).unwrap();
    assert!(profile.is_external);
    assert_eq!(
        profile.unsafe_reason_mask & unsafe_reason::EXTERNAL,
        unsafe_reason::EXTERNAL
    );
}

#[test]
fn doc_facts_drive_bits_and_editable_count() {
    let sym = "a/G.";
    let d1 = doc(
        "a/G.scala",
        vec![info(sym, SymKind::Object, 0, &[])],
        vec![
            occ(gk(sym), Role::Definition),
            occ(gk(sym), Role::Reference),
        ],
    );
    let d2 = doc("gen/G.scala", vec![], vec![occ(gk(sym), Role::Reference)]);
    let d3 = doc("ro/G.scala", vec![], vec![occ(gk(sym), Role::Reference)]);
    let d4 = doc("dep/G.scala", vec![], vec![occ(gk(sym), Role::Reference)]);
    let mut facts: HashMap<String, DocFacts> = HashMap::new();
    facts.insert(
        "gen/G.scala".into(),
        DocFacts {
            generated: true,
            readonly: false,
            is_dependency_source: false,
        },
    );
    facts.insert(
        "ro/G.scala".into(),
        DocFacts {
            generated: false,
            readonly: true,
            is_dependency_source: false,
        },
    );
    facts.insert(
        "dep/G.scala".into(),
        DocFacts {
            generated: false,
            readonly: false,
            is_dependency_source: true,
        },
    );
    let batch = SemanticBatch::assemble_with_facts(vec![d1, d2, d3, d4], &facts);
    let profile = batch.rename_profile_of(&gk(sym)).unwrap();
    assert!(profile.has_generated_occurrences);
    assert!(profile.has_readonly_occurrences);
    assert_eq!(profile.editable_occurrence_count, 2); // only the two in a/G.scala
    let expected_mask = unsafe_reason::GENERATED_OCCURRENCE
        | unsafe_reason::READONLY_OCCURRENCE
        | unsafe_reason::DEPENDENCY_SOURCE;
    assert_eq!(profile.unsafe_reason_mask, expected_mask);
    assert!(!profile.is_external);
}

#[test]
fn groups_are_deterministic_across_assemble_calls() {
    let b1 = companion_batch();
    let b2 = companion_batch();
    assert_eq!(b1.groups.ref_groups, b2.groups.ref_groups);
    assert_eq!(b1.groups.rename_groups, b2.groups.rename_groups);
    assert_eq!(b1.rename_profiles, b2.rename_profiles);
}
