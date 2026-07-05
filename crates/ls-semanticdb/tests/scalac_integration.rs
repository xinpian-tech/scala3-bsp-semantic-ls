//! Port of the Scala `ScalacIntegrationSuite` over committed fixtures produced by
//! REAL pinned scalac (Scala 3.8.4, `-Xsemanticdb`). The fixtures live under
//! `tests/fixtures/target/META-INF/semanticdb/**` (the emitted `.semanticdb`
//! files) and `tests/fixtures/sources/**` (the exact compiled sources, for md5).
//! The test locates, parses, md5-validates, normalizes, and groups them.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use ls_index_model::{
    sym_props, unsafe_reason, DocId, NormalizedDocument, RenameProfile, Role, SymKind, SymbolKey,
};
use ls_semanticdb::{
    md5, normalize, parse_file, symbols, DocFacts, FreshnessCheck, SdbDocument, SemanticBatch,
    SemanticdbLocator,
};

struct Fixture {
    locator: SemanticdbLocator,
    sources: HashMap<String, String>,
    raw: HashMap<String, SdbDocument>,
    doc_ids: HashMap<String, DocId>,
    batch: SemanticBatch,
}

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

fn load() -> Fixture {
    let base = fixtures_dir();
    let locator = SemanticdbLocator::new(base.join("target"));
    let sources_root = base.join("sources");

    let files = locator.list_semanticdb_files();
    let mut raw: HashMap<String, SdbDocument> = HashMap::new();
    for f in &files {
        for d in parse_file(f).expect("parse fixture").documents {
            raw.insert(d.uri.clone(), d);
        }
    }
    let mut uris: Vec<String> = raw.keys().cloned().collect();
    uris.sort();
    let doc_ids: HashMap<String, DocId> = uris
        .iter()
        .enumerate()
        .map(|(i, uri)| (uri.clone(), DocId::new((i + 1) as u64)))
        .collect();
    let sources: HashMap<String, String> = uris
        .iter()
        .map(|uri| {
            let text = fs::read_to_string(sources_root.join(uri)).expect("read source");
            (uri.clone(), text)
        })
        .collect();
    let normalized: Vec<NormalizedDocument> = uris
        .iter()
        .map(|uri| normalize(&raw[uri], doc_ids[uri]))
        .collect();
    let batch = SemanticBatch::assemble(normalized);
    Fixture {
        locator,
        sources,
        raw,
        doc_ids,
        batch,
    }
}

fn gk(sym: &str) -> SymbolKey {
    SymbolKey::global(sym)
}

impl Fixture {
    fn all_global_symbols(&self) -> Vec<String> {
        let mut v: Vec<String> = self
            .batch
            .groups
            .ref_group_index
            .keys()
            .filter(|k| !k.is_local())
            .map(|k| k.semantic_symbol.clone())
            .collect();
        v.sort();
        v
    }

    fn normalized_doc(&self, uri: &str) -> &NormalizedDocument {
        self.batch
            .documents
            .iter()
            .find(|d| d.uri == uri)
            .unwrap_or_else(|| panic!("no normalized doc for {uri}"))
    }

    fn ref_group(&self, sym: &str) -> usize {
        self.batch
            .ref_group_of(&gk(sym))
            .unwrap_or_else(|| panic!("no ref group for {sym}"))
    }

    fn profile(&self, sym: &str) -> &RenameProfile {
        self.batch
            .rename_profile_of(&gk(sym))
            .unwrap_or_else(|| panic!("no rename profile for {sym}"))
    }
}

const SOURCES: [&str; 9] = [
    "src/fix/Person.scala",
    "src/fix/Use.scala",
    "src/fix/Box.scala",
    "src/fix/Over.scala",
    "src/fix/Animals.scala",
    "src/fix/Copy.scala",
    "src/fix/Export.scala",
    "src/fix/Derives.scala",
    "src/fix/Opaque.scala",
];

#[test]
fn locator_finds_one_semanticdb_file_per_fixture_source() {
    let fx = load();
    let files = fx.locator.list_semanticdb_files();
    assert_eq!(files.len(), SOURCES.len());
    let rels: std::collections::HashSet<String> = files
        .iter()
        .filter_map(|f| fx.locator.source_relative_path_for(f))
        .collect();
    assert_eq!(rels, SOURCES.iter().map(|s| s.to_string()).collect());
}

#[test]
fn uris_match_source_relative_paths_and_schema_is_semanticdb4() {
    let fx = load();
    let keys: std::collections::HashSet<&String> = fx.raw.keys().collect();
    let expected: std::collections::HashSet<String> =
        SOURCES.iter().map(|s| s.to_string()).collect();
    assert_eq!(keys, expected.iter().collect());
    for (uri, doc) in &fx.raw {
        assert_eq!(doc.schema, 4, "{uri}");
        assert_eq!(doc.language_code, 1, "{uri}"); // SdbLanguage::Scala
    }
}

#[test]
fn md5_validation_is_fresh_for_every_compiled_source() {
    let fx = load();
    for (uri, doc) in &fx.raw {
        assert_eq!(
            md5::validate_doc(&fx.sources[uri], doc),
            FreshnessCheck::Fresh,
            "{uri}"
        );
    }
}

#[test]
fn md5_validation_catches_edits() {
    let fx = load();
    let doc = &fx.raw["src/fix/Person.scala"];
    let edited = format!("{}\n// edited\n", fx.sources["src/fix/Person.scala"]);
    assert!(!md5::validate_doc(&edited, doc).is_fresh());
}

#[test]
fn known_symbols_with_kinds_and_properties_are_present() {
    let fx = load();
    let person = fx.normalized_doc("src/fix/Person.scala");
    let by_name: HashMap<&str, _> = person
        .symbols
        .iter()
        .map(|s| (s.key.semantic_symbol.as_str(), s))
        .collect();
    let cls = by_name["fix/Person#"];
    assert_eq!(cls.kind, SymKind::Class);
    assert!(cls.properties & sym_props::CASE != 0, "case class bit");
    assert_eq!(cls.display_name, "Person");
    assert_eq!(cls.package_name.as_deref(), Some("fix"));
    let obj = by_name["fix/Person."];
    assert_eq!(obj.kind, SymKind::Object);
}

#[test]
fn occurrence_roles_definition_and_references() {
    let fx = load();
    let person = fx.normalized_doc("src/fix/Person.scala");
    let defs: Vec<_> = person
        .occurrences
        .iter()
        .filter(|o| o.key == gk("fix/Person#") && o.role == Role::Definition)
        .collect();
    assert!(!defs.is_empty());
    assert!(defs.iter().any(|o| o.span.start_line == 2));

    let use_doc = fx.normalized_doc("src/fix/Use.scala");
    let person_group = fx.ref_group("fix/Person#");
    let ref_lines: std::collections::HashSet<u32> = use_doc
        .occurrences
        .iter()
        .filter(|o| {
            !o.key.is_local()
                && fx.batch.ref_group_of(&o.key) == Some(person_group)
                && o.role == Role::Reference
        })
        .map(|o| o.span.start_line)
        .collect();
    assert!(
        ref_lines.len() >= 4,
        "person refs on >= 4 lines: {ref_lines:?}"
    );
}

#[test]
fn class_companion_constructor_and_apply_share_one_ref_group() {
    let fx = load();
    let g = fx.ref_group("fix/Person#");
    assert_eq!(fx.ref_group("fix/Person."), g);
    let all = fx.all_global_symbols();
    let ctors: Vec<&String> = all
        .iter()
        .filter(|s| s.starts_with("fix/Person#") && symbols::is_constructor(s))
        .collect();
    assert!(!ctors.is_empty());
    for c in ctors {
        assert_eq!(fx.ref_group(c), g, "{c}");
    }
    let applies: Vec<&String> = all
        .iter()
        .filter(|s| s.starts_with("fix/Person.apply(") && s.ends_with('.'))
        .collect();
    assert!(!applies.is_empty());
    for a in applies {
        assert_eq!(fx.ref_group(a), g, "{a}");
    }
}

#[test]
fn case_class_has_companion_on_rename_profile() {
    let fx = load();
    let profile = fx.profile("fix/Person#");
    assert!(profile.has_companion);
    assert!(!profile.is_external);
    assert!(profile.editable_occurrence_count > 0);
}

#[test]
fn var_getter_and_setter_are_merged() {
    let fx = load();
    let all = fx.all_global_symbols();
    let value_setter = all
        .iter()
        .filter(|s| symbols::is_setter(s))
        .find(|s| s.contains("value_="))
        .expect("value_= setter");
    let getter = ["fix/Box#value().", "fix/Box#value."]
        .into_iter()
        .find(|s| fx.batch.ref_group_of(&gk(s)).is_some())
        .expect("getter symbol");
    assert_eq!(fx.ref_group(value_setter), fx.ref_group(getter));
    assert_eq!(
        fx.batch.rename_group_of(&gk(value_setter)),
        fx.batch.rename_group_of(&gk(getter))
    );
}

#[test]
fn method_overloads_stay_in_separate_groups() {
    let fx = load();
    let overloads: Vec<String> = fx
        .all_global_symbols()
        .into_iter()
        .filter(|s| s.starts_with("fix/Over.f(") && s.ends_with('.'))
        .collect();
    assert_eq!(overloads.len(), 2, "{overloads:?}");
    assert_ne!(fx.ref_group(&overloads[0]), fx.ref_group(&overloads[1]));
}

#[test]
fn override_family_is_flagged_unsafe_on_both_sides() {
    let fx = load();
    let all = fx.all_global_symbols();
    let dog = all.iter().find(|s| s.starts_with("fix/Dog#sound")).unwrap();
    let animal = all
        .iter()
        .find(|s| s.starts_with("fix/Animal#sound"))
        .unwrap();
    let dog_p = fx.profile(dog);
    let animal_p = fx.profile(animal);
    assert!(dog_p.has_override_family, "{dog}");
    assert!(animal_p.has_override_family, "{animal}");
    assert!(dog_p.unsafe_reason_mask & unsafe_reason::OVERRIDE_FAMILY != 0);
    assert!(animal_p.unsafe_reason_mask & unsafe_reason::OVERRIDE_FAMILY != 0);
    assert!(!dog_p.is_safe());
}

#[test]
fn references_to_library_symbols_are_external() {
    let fx = load();
    let profile = fx
        .batch
        .rename_profile_of(&gk("scala/Int#"))
        .expect("scala/Int#");
    assert!(profile.is_external);
    assert!(profile.unsafe_reason_mask & unsafe_reason::EXTERNAL != 0);
}

#[test]
fn local_symbols_carry_caller_doc_id_and_stay_isolated() {
    let fx = load();
    let box_doc_id = fx.doc_ids["src/fix/Box.scala"];
    let box_doc = fx.normalized_doc("src/fix/Box.scala");
    let locals: Vec<&SymbolKey> = {
        let mut v: Vec<&SymbolKey> = box_doc
            .occurrences
            .iter()
            .filter(|o| o.key.is_local())
            .map(|o| &o.key)
            .collect();
        v.dedup();
        v
    };
    assert!(!locals.is_empty());
    for l in &locals {
        assert_eq!(l.local_doc, Some(box_doc_id));
    }
    let profile = fx.batch.rename_profile_of(locals[0]).unwrap();
    assert!(profile.is_local);
}

#[test]
fn marking_a_document_generated_poisons_groups_touching_it() {
    let fx = load();
    let mut facts: HashMap<String, DocFacts> = HashMap::new();
    facts.insert(
        "src/fix/Use.scala".into(),
        DocFacts {
            generated: true,
            readonly: false,
            is_dependency_source: false,
        },
    );
    let poisoned = SemanticBatch::assemble_with_facts(fx.batch.documents.clone(), &facts);
    let before = fx.profile("fix/Person#");
    let after = poisoned.rename_profile_of(&gk("fix/Person#")).unwrap();
    assert!(!before.has_generated_occurrences);
    assert!(after.has_generated_occurrences);
    assert!(after.unsafe_reason_mask & unsafe_reason::GENERATED_OCCURRENCE != 0);
    assert!(after.editable_occurrence_count < before.editable_occurrence_count);
}

#[test]
fn export_forwarder_symbol_exists_with_no_definition_occurrence() {
    let fx = load();
    let doc = fx.normalized_doc("src/fix/Export.scala");
    let by_sym: HashMap<&str, _> = doc
        .symbols
        .iter()
        .map(|s| (s.key.semantic_symbol.as_str(), s))
        .collect();
    let fwd = by_sym["fix/Api.work()."];
    assert_eq!(fwd.kind, SymKind::Method);
    assert_eq!(fwd.display_name, "work");
    assert!(fwd.overridden_symbols.is_empty());
    assert!(by_sym.contains_key("fix/Impl.work()."));

    let def_occs = |sym: &str| {
        doc.occurrences
            .iter()
            .filter(|o| o.key == gk(sym) && o.role == Role::Definition)
            .count()
    };
    assert_eq!(
        def_occs("fix/Api.work()."),
        0,
        "forwarder has no def occurrence"
    );
    assert!(
        def_occs("fix/Impl.work().") > 0,
        "original has a def occurrence"
    );
    assert!(doc
        .occurrences
        .iter()
        .any(|o| o.key == gk("fix/Impl.") && o.role == Role::Reference));
    assert!(doc
        .occurrences
        .iter()
        .any(|o| o.key == gk("fix/Api.work().") && o.role == Role::Reference));
}

#[test]
fn export_forwarder_call_sites_join_the_originals_ref_group() {
    let fx = load();
    assert_eq!(
        fx.ref_group("fix/Api.work()."),
        fx.ref_group("fix/Impl.work().")
    );
}

#[test]
fn export_forwarder_marks_rename_group_unsupported_symbol_family() {
    let fx = load();
    let profile = fx.profile("fix/Impl.work().");
    assert!(
        profile.unsafe_reason_mask & unsafe_reason::UNSUPPORTED_SYMBOL_FAMILY != 0,
        "mask=0x{:x}",
        profile.unsafe_reason_mask
    );
    assert!(!profile.is_safe());
}

#[test]
fn derives_clause_case_class_defined_and_derived_given_synthetic_only() {
    let fx = load();
    let doc = fx.normalized_doc("src/fix/Derives.scala");
    let syms: std::collections::HashSet<&str> = doc
        .occurrences
        .iter()
        .map(|o| o.key.semantic_symbol.as_str())
        .collect();
    assert!(syms.contains("fix/Coord#"));
    assert!(!syms.iter().any(|s| s.contains("CanEqual")));
    assert!(!syms.iter().any(|s| s.contains("derived")));
}

#[test]
fn opaque_type_carries_opaque_property_and_group_flagged_unsafe() {
    let fx = load();
    let user_id = fx
        .all_global_symbols()
        .into_iter()
        .find(|s| s.contains("UserId") && s.ends_with('#'))
        .expect("UserId type symbol");
    let info = fx
        .normalized_doc("src/fix/Opaque.scala")
        .symbols
        .iter()
        .find(|s| s.key.semantic_symbol == user_id)
        .expect("UserId symbol info");
    assert!(
        info.properties & sym_props::OPAQUE != 0,
        "properties=0x{:x}",
        info.properties
    );
    let profile = fx.profile(&user_id);
    assert!(
        profile.unsafe_reason_mask & unsafe_reason::OPAQUE_TYPE != 0,
        "mask=0x{:x}",
        profile.unsafe_reason_mask
    );
    assert!(!profile.is_safe());
}

#[test]
fn synthetic_only_copy_has_no_definition_but_defined_owner() {
    let fx = load();
    let doc = fx.normalized_doc("src/fix/Copy.scala");
    let copy_key = gk("fix/Pt#copy().");
    assert!(doc.symbols.iter().any(|s| s.key == copy_key));
    assert_eq!(
        doc.occurrences
            .iter()
            .filter(|o| o.key == copy_key && o.role == Role::Definition)
            .count(),
        0
    );
    assert!(doc
        .occurrences
        .iter()
        .any(|o| o.key == copy_key && o.role == Role::Reference));
    assert!(doc
        .occurrences
        .iter()
        .any(|o| o.key == gk("fix/Pt#") && o.role == Role::Definition));
}

#[test]
fn synthetic_only_symbol_flagged_synthetic_only_not_external() {
    let fx = load();
    let profile = fx.profile("fix/Pt#copy().");
    assert!(
        profile.unsafe_reason_mask & unsafe_reason::SYNTHETIC_ONLY != 0,
        "mask=0x{:x}",
        profile.unsafe_reason_mask
    );
    assert!(!profile.is_external, "synthesized member is not external");
    assert!(!profile.is_safe());
}

#[test]
fn symbol_with_editable_definition_not_flagged_synthetic_only() {
    let fx = load();
    let x_profile = fx.profile("fix/Pt#x.");
    assert_eq!(
        x_profile.unsafe_reason_mask & unsafe_reason::SYNTHETIC_ONLY,
        0
    );
    let int_profile = fx.profile("scala/Int#");
    assert_eq!(
        int_profile.unsafe_reason_mask & unsafe_reason::SYNTHETIC_ONLY,
        0
    );
    assert!(int_profile.is_external);
}

#[test]
fn normalization_is_deterministic() {
    let fx = load();
    let mut uris: Vec<String> = fx.raw.keys().cloned().collect();
    uris.sort();
    let docs2: Vec<NormalizedDocument> = uris
        .iter()
        .map(|uri| normalize(&fx.raw[uri], fx.doc_ids[uri]))
        .collect();
    let batch2 = SemanticBatch::assemble(docs2);
    assert_eq!(batch2.documents, fx.batch.documents);
    assert!(batch2.groups == fx.batch.groups);
    assert_eq!(batch2.rename_profiles, fx.batch.rename_profiles);
}
