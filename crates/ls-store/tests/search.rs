//! Workspace-symbol search: the `FuzzyRankSuite` port plus deterministic
//! query behavior over a built segment (tiering, multi-token across
//! display/owner/package, punctuation, camel-hump fuzzy fallback, bounded
//! candidate pull, exact-name membership, and no-match/empty negatives).

use ls_store::search::{initials, normalize, score};
use ls_store::{
    SearchIndex, SearchRow, SegmentData, SegmentDoc, SegmentReader, SegmentSymbol, SegmentWriter,
    SymbolMeta, TargetMeta, FUZZY_CANDIDATE_CAP,
};

// ---- FuzzyRankSuite port ----

#[test]
fn normalize_lowercases_and_keeps_alnum() {
    assert_eq!(normalize("workspaceSymbol"), "workspacesymbol");
    assert_eq!(normalize("Foo_Bar-42.Baz"), "foobar42baz");
    assert_eq!(normalize("(*)"), "");
    assert_eq!(normalize("数据库"), "数据库");
}

#[test]
fn initials_picks_hump_boundaries() {
    assert_eq!(initials("workspaceSymbol"), "ws");
    assert_eq!(initials("FooBarBaz"), "fbb");
    assert_eq!(initials("snake_case_name"), "scn");
    assert_eq!(initials("user2Name"), "u2n");
}

#[test]
fn score_ranks_exact_above_prefix_above_subsequence() {
    let exact = score("Core", "Core").unwrap();
    let prefix = score("Cor", "CoreThing").unwrap();
    let subseq = score("ce", "CoreEngine").unwrap();
    assert!(exact > prefix, "{exact} !> {prefix}");
    assert!(prefix > subseq, "{prefix} !> {subseq}");
}

#[test]
fn score_hump_aligned_beats_plain() {
    let strong = score("wSy", "workspaceSymbol").unwrap();
    let weak = score("wSy", "whimsy").unwrap();
    assert!(strong > weak, "{strong} !> {weak}");
}

#[test]
fn score_shorter_name_wins_tie() {
    let short = score("ab", "aXb").unwrap();
    let long = score("ab", "aXXXXXb").unwrap();
    assert!(short > long, "{short} !> {long}");
}

#[test]
fn score_non_subsequence_and_empty_are_not_matches() {
    assert_eq!(score("xyz", "workspaceSymbol"), None);
    assert_eq!(score("", "anything"), None);
    assert_eq!(score("abc", "(*)"), None);
}

// ---- workspace-symbol query over a built segment ----

/// `(display, owner, package)` entries -> a built, opened `SearchIndex`. Symbols
/// carry pre-sorted semantic strings so the caller ordinal == on-disk ordinal.
fn index(entries: &[(&str, &str, &str)]) -> (tempfile::TempDir, SearchIndex) {
    let symbols = (0..entries.len())
        .map(|i| SegmentSymbol {
            semantic_symbol: format!("s{i:05}"),
            symbol_id: i as i64,
            ref_group_ord: -1,
            rename_group_ord: -1,
            def_target_ord: -1,
        })
        .collect();
    let symbol_meta = entries
        .iter()
        .map(|(display, owner, package)| SymbolMeta {
            display: (*display).into(),
            owner: (*owner).into(),
            package_name: (*package).into(),
            kind: 5,
            properties: 0,
            def_packed_start: 0,
            def_packed_end: 0,
            def_doc_ord: 0,
        })
        .collect();
    let search_rows = entries
        .iter()
        .enumerate()
        .map(|(i, (display, _, _))| SearchRow {
            normalized_name: normalize(display),
            symbol_ord: i as i32,
        })
        .collect();
    let data = SegmentData {
        docs: vec![SegmentDoc {
            uri: "file:///D.scala".into(),
            doc_id: 0,
            epoch: 1,
            target_ord: 0,
            generated: false,
            readonly: false,
        }],
        targets: vec![1],
        symbols,
        ref_occurrences: vec![],
        def_occurrences: vec![],
        rename_occurrences: vec![],
        rename_profiles: vec![],
        doc_occurrences: vec![vec![]],
        target_meta: vec![TargetMeta::default()],
        symbol_meta,
        search_rows,
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentWriter::write(tmp.path(), 1, &data, 0).expect("write");
    let reader = SegmentReader::open(&dir).expect("open");
    let idx = reader.build_search_index();
    (tmp, idx)
}

fn names(hits: &[ls_store::WorkspaceSymbolHit]) -> Vec<String> {
    hits.iter().map(|h| h.display.clone()).collect()
}

#[test]
fn ranks_exact_then_prefix_shorter_first() {
    let (_t, idx) = index(&[
        ("CoreEngine", "srv", "com.example"),
        ("Core", "srv", "com.example"),
        ("CoreThing", "srv", "com.example"),
    ]);
    // exact "Core" first, then prefix matches shortest-name-first.
    assert_eq!(
        names(&idx.workspace_symbol_search("Core", 10)),
        vec!["Core", "CoreThing", "CoreEngine"]
    );
}

#[test]
fn multi_token_matches_across_fields() {
    let (_t, idx) = index(&[
        ("CoreEngine", "srv", "com.example"),
        ("CoreThing", "srv", "com.example"),
        ("FooBar", "srv", "org.foo"),
    ]);
    // "com" must prefix a package token AND "core" a display token.
    let hits = names(&idx.workspace_symbol_search("com Core", 10));
    assert!(hits.contains(&"CoreEngine".to_string()));
    assert!(hits.contains(&"CoreThing".to_string()));
    assert!(
        !hits.contains(&"FooBar".to_string()),
        "org.foo must not match"
    );
}

#[test]
fn punctuation_query_tokenizes() {
    let (_t, idx) = index(&[
        ("CoreEngine", "srv", "com.example"),
        ("FooBar", "srv", "org.foo"),
    ]);
    // "com.example" -> tokens [com, example]; only the com.example row matches.
    let hits = names(&idx.workspace_symbol_search("com.example", 10));
    assert_eq!(hits, vec!["CoreEngine"]);
}

#[test]
fn camel_hump_fuzzy_fallback_ranks_hump_hits_first() {
    let (_t, idx) = index(&[
        ("Core", "srv", "com.example"),
        ("CoreEngine", "srv", "com.example"),
        ("CoreThing", "srv", "com.example"),
    ]);
    // "ce" is not a prefix of any display; the single-token fuzzy fallback finds
    // all three as subsequences, and CoreEngine (c+E on humps) ranks first.
    let hits = names(&idx.workspace_symbol_search("ce", 10));
    assert_eq!(hits.first().unwrap(), "CoreEngine");
    assert_eq!(hits.len(), 3);
}

#[test]
fn exact_name_membership_beyond_search_window() {
    let (_t, idx) = index(&[
        ("Core", "srv", "com.example"),
        ("CoreEngine", "srv", "com.example"),
        ("Zebra", "srv", "org.zoo"),
    ]);
    assert!(idx.workspace_symbol_name_exists("CoreEngine"));
    assert!(idx.workspace_symbol_name_exists("Zebra"));
    assert!(!idx.workspace_symbol_name_exists("Missing"));
    assert!(!idx.workspace_symbol_name_exists(""));
    // Membership is exact, never a prefix/fuzzy proxy.
    assert!(!idx.workspace_symbol_name_exists("Cor"));
}

#[test]
fn no_match_and_empty_queries_return_empty() {
    let (_t, idx) = index(&[("CoreEngine", "srv", "com.example")]);
    assert!(idx.workspace_symbol_search("zzz", 10).is_empty());
    assert!(idx.workspace_symbol_search("", 10).is_empty());
    assert!(idx.workspace_symbol_search("(*)", 10).is_empty());
    assert!(idx.workspace_symbol_search("Core", 0).is_empty());
}

#[test]
fn unsorted_input_resolves_correct_metadata_and_identity() {
    // Caller order is Zeta then Alpha; on disk the symbol dictionary sorts to
    // [Alpha, Zeta] and symbol-meta.bin is remapped with it. The search rows use
    // CALLER ordinals, so unless the writer remaps them a search for "Alpha"
    // resolves Zeta's metadata. Distinct doc/target ids prove full hit identity.
    let symbols = vec![
        SegmentSymbol {
            semantic_symbol: "z/pkg/Zeta#".into(),
            symbol_id: 200,
            ref_group_ord: -1,
            rename_group_ord: -1,
            def_target_ord: 0,
        },
        SegmentSymbol {
            semantic_symbol: "a/pkg/Alpha#".into(),
            symbol_id: 201,
            ref_group_ord: -1,
            rename_group_ord: -1,
            def_target_ord: 1,
        },
    ];
    let meta = |display: &str, owner: &str, pkg: &str, kind: i32, doc: i32| SymbolMeta {
        display: display.into(),
        owner: owner.into(),
        package_name: pkg.into(),
        kind,
        properties: 0,
        def_packed_start: 0,
        def_packed_end: 0,
        def_doc_ord: doc,
    };
    let data = SegmentData {
        docs: vec![
            SegmentDoc {
                uri: "file:///Z.scala".into(),
                doc_id: 500,
                epoch: 1,
                target_ord: 0,
                generated: false,
                readonly: false,
            },
            SegmentDoc {
                uri: "file:///A.scala".into(),
                doc_id: 501,
                epoch: 1,
                target_ord: 1,
                generated: false,
                readonly: false,
            },
        ],
        targets: vec![1000, 1001],
        symbols,
        ref_occurrences: vec![],
        def_occurrences: vec![],
        rename_occurrences: vec![],
        rename_profiles: vec![],
        doc_occurrences: vec![vec![], vec![]],
        target_meta: vec![TargetMeta::default(), TargetMeta::default()],
        // Parallel to caller symbols (Zeta, Alpha).
        symbol_meta: vec![
            meta("Zeta", "zowner", "z.pkg", 3, 0),
            meta("Alpha", "aowner", "a.pkg", 5, 1),
        ],
        // Caller ordinals: Zeta = 0, Alpha = 1.
        search_rows: vec![
            SearchRow {
                normalized_name: normalize("Zeta"),
                symbol_ord: 0,
            },
            SearchRow {
                normalized_name: normalize("Alpha"),
                symbol_ord: 1,
            },
        ],
    };
    let tmp = tempfile::tempdir().unwrap();
    let dir = SegmentWriter::write(tmp.path(), 1, &data, 0).expect("write");
    let reader = SegmentReader::open(&dir).expect("open");
    let idx = reader.build_search_index();

    let hits = idx.workspace_symbol_search("Alpha", 10);
    assert_eq!(hits.len(), 1);
    let h = &hits[0];
    assert_eq!(h.display, "Alpha");
    assert_eq!(h.owner, "aowner");
    assert_eq!(h.package_name, "a.pkg");
    assert_eq!(h.kind, 5);
    // Full identity resolves to Alpha, not Zeta.
    assert_eq!(h.symbol_id, 201);
    assert_eq!(h.def_doc_ord, 1);
    assert_eq!(h.doc_id, 501);
    assert_eq!(h.def_target_ord, 1);
    assert_eq!(h.target_id, 1001);
    // symbol_ord is the on-disk (sorted) ordinal: Alpha sorts first.
    assert_eq!(h.symbol_ord, 0);

    // And membership resolves the same way.
    assert!(idx.workspace_symbol_name_exists("Alpha"));
    assert!(idx.workspace_symbol_name_exists("Zeta"));
}

#[test]
fn fuzzy_candidate_pull_stays_bounded() {
    // More single-first-char candidates than the cap: the fuzzy pull must stop
    // at FUZZY_CANDIDATE_CAP rather than scan the whole corpus.
    let n = FUZZY_CANDIDATE_CAP + 100;
    let entries: Vec<(String, String, String)> = (0..n)
        .map(|i| (format!("aSym{i:05}"), "srv".into(), "com.example".into()))
        .collect();
    let refs: Vec<(&str, &str, &str)> = entries
        .iter()
        .map(|(d, o, p)| (d.as_str(), o.as_str(), p.as_str()))
        .collect();
    let (_t, idx) = index(&refs);
    let hits = idx.workspace_symbol_search("a", 10);
    assert_eq!(hits.len(), 10, "limit must bound the result");
    assert_eq!(
        idx.last_fuzzy_candidate_count(),
        FUZZY_CANDIDATE_CAP,
        "fuzzy pull must be capped"
    );
}
