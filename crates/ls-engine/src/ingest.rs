//! Full-generation SemanticDB ingest, adapted to the SQLite-free
//! one-segment-per-generation store.
//!
//! One `ingest` call re-reads the complete workspace state: it locates every
//! `.semanticdb` file per target root and parses it, md5-validates each
//! TextDocument against its source file (stale docs are recorded and still
//! indexed; docs whose source no longer exists are skipped and counted),
//! assigns a per-document epoch from the previous generation's workspace state
//! (bumping on md5 change), normalizes, builds exact alias groups, materializes
//! the whole-workspace [`SegmentData`] with dense ordinals, and publishes it
//! through the store (segment → workspace-state → manifest → snapshot swap).
//!
//! Shared sources (one uri compiled by several targets) are indexed once: the
//! first target in workspace order that contains the uri is the *primary* and
//! owns the postings. Rename postings contain only genuinely renameable
//! occurrences: the doc must be editable AND the source token under the
//! occurrence span must textually match the member's renameable name.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use ls_index_model::{
    occ_flags, DocId, NormalizedDocument, Occurrence, Role, Span, SymKind, SymbolInfo, SymbolKey,
};
use ls_semanticdb::symbols::Descriptor;
use ls_semanticdb::{
    md5, normalize, parse_file, symbols, DocFacts, SdbDocument, SemanticBatch, SemanticdbLocator,
};
use ls_store::{
    DocOcc, GroupOcc, RenameProfile as StoreRenameProfile, SearchRow, SegmentData, SegmentDoc,
    SegmentSymbol, Snapshot, Store, StoreResult, SymbolMeta, TargetMeta,
};

use crate::hash::{doc_id_for, symbol_id_for, target_id_for};
use crate::state::{DocState, IngestState};
use crate::symbol_encoding::encode_key;
use crate::targets::WorkspaceTargets;

/// Result of one full-generation publish.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IngestReport {
    pub segment_id: u64,
    pub docs_indexed: usize,
    pub docs_shared: usize,
    pub docs_stale: usize,
    pub docs_skipped: usize,
    pub symbol_count: usize,
    pub ref_group_count: usize,
    pub rename_group_count: usize,
    pub stale_uris: Vec<String>,
    pub skipped_uris: Vec<String>,
    pub duration_ms: u64,
}

struct PrimaryDoc {
    uri: String,
    doc_id: DocId,
    epoch: i32,
    target_ord: usize,
    facts: DocFacts,
    sdb: SdbDocument,
    source_text: String,
}

/// Runs a full-generation ingest over `workspace`, publishing a fresh segment
/// and returning the report plus the newly active snapshot.
pub fn ingest(
    store: &Store,
    workspace: &WorkspaceTargets,
) -> StoreResult<(IngestReport, Arc<Snapshot>)> {
    let t0 = Instant::now();
    let prev_state = store
        .current()
        .map(|s| IngestState::decode(&s.state().payload))
        .unwrap_or_default();

    let mut primaries: Vec<PrimaryDoc> = Vec::new();
    let mut primary_index: HashMap<String, usize> = HashMap::new();
    let mut shared_count = 0usize;
    let mut stale_uris: Vec<String> = Vec::new();
    let mut skipped_uris: Vec<String> = Vec::new();
    let mut new_state = IngestState::default();

    for (target_ord, spec) in workspace.targets.iter().enumerate() {
        let locator = SemanticdbLocator::new(spec.semanticdb_root.clone());
        for file in locator.list_semanticdb_files() {
            let documents = match parse_file(&file) {
                Ok(d) => d.documents,
                Err(_) => continue, // malformed file: skip, keep ingesting
            };
            for sdb in documents {
                let uri = sdb.uri.clone();
                let source_path = spec.sourceroot.join(&uri);
                if !source_path.is_file() {
                    skipped_uris.push(uri);
                    continue;
                }
                let source_text = match std::fs::read_to_string(&source_path) {
                    Ok(t) => t,
                    Err(_) => {
                        skipped_uris.push(uri);
                        continue;
                    }
                };
                if !md5::validate_doc(&source_text, &sdb).is_fresh() {
                    stale_uris.push(uri.clone());
                }
                let facts = spec.facts(&uri);
                if primary_index.contains_key(&uri) {
                    shared_count += 1;
                } else {
                    let doc_id = doc_id_for(&uri);
                    let epoch = match prev_state.get(&uri) {
                        Some(p) if p.md5 == sdb.md5 => p.epoch,
                        Some(p) => p.epoch + 1,
                        None => 1,
                    };
                    new_state.docs.insert(
                        uri.clone(),
                        DocState {
                            epoch,
                            md5: sdb.md5.clone(),
                        },
                    );
                    primary_index.insert(uri.clone(), primaries.len());
                    primaries.push(PrimaryDoc {
                        uri,
                        doc_id,
                        epoch,
                        target_ord,
                        facts,
                        sdb,
                        source_text,
                    });
                }
            }
        }
    }

    let normalized: Vec<NormalizedDocument> = primaries
        .iter()
        .map(|p| normalize(&p.sdb, p.doc_id))
        .collect();
    let facts_by_uri: HashMap<String, DocFacts> =
        primaries.iter().map(|p| (p.uri.clone(), p.facts)).collect();
    let batch = SemanticBatch::assemble_with_facts(normalized, &facts_by_uri);

    // Deterministic symbol universe: every ref-group key, sorted.
    let mut keys: Vec<SymbolKey> = batch.groups.ref_group_index.keys().cloned().collect();
    keys.sort_by(|a, b| {
        a.semantic_symbol
            .cmp(&b.semantic_symbol)
            .then_with(|| ord_val(a).cmp(&ord_val(b)))
    });
    let caller_ord_of: HashMap<SymbolKey, usize> = keys
        .iter()
        .cloned()
        .enumerate()
        .map(|(i, k)| (k, i))
        .collect();

    // First-wins symbol info / display name, and first definition occurrence.
    let mut info_by_key: HashMap<SymbolKey, SymbolInfo> = HashMap::new();
    let mut display_name_of: HashMap<SymbolKey, String> = HashMap::new();
    for doc in &batch.documents {
        for s in &doc.symbols {
            info_by_key
                .entry(s.key.clone())
                .or_insert_with(|| s.clone());
            display_name_of
                .entry(s.key.clone())
                .or_insert_with(|| s.display_name.clone());
        }
    }
    let mut def_by_key: HashMap<SymbolKey, (i32, Span)> = HashMap::new();
    for (doc_ord, doc) in batch.documents.iter().enumerate() {
        for o in &doc.occurrences {
            if o.role == Role::Definition {
                def_by_key
                    .entry(o.key.clone())
                    .or_insert((doc_ord as i32, o.span));
            }
        }
    }

    let ref_group_count = batch.groups.ref_groups.len();
    let rename_group_count = batch.groups.rename_groups.len();
    let mut ref_postings: Vec<Vec<GroupOcc>> = vec![Vec::new(); ref_group_count];
    let mut def_postings: Vec<Vec<GroupOcc>> = vec![Vec::new(); ref_group_count];
    let mut rename_postings: Vec<Vec<GroupOcc>> = vec![Vec::new(); rename_group_count];
    let mut doc_occurrences: Vec<Vec<DocOcc>> = vec![Vec::new(); primaries.len()];
    let mut def_target_of: HashMap<SymbolKey, i32> = HashMap::new();

    for (doc_ord, (p, doc)) in primaries.iter().zip(batch.documents.iter()).enumerate() {
        let lines: Vec<&str> = p.source_text.split_inclusive('\n').collect();
        for occ in &doc.occurrences {
            let flags = occ_flags_for(occ, &p.facts);
            let caller = caller_ord_of[&occ.key] as i32;
            doc_occurrences[doc_ord].push(DocOcc {
                symbol_ord: caller,
                span: occ.span,
                flags,
            });
            let g = batch.groups.ref_group_index[&occ.key];
            let group_occ = GroupOcc {
                doc_ord: doc_ord as i32,
                doc_epoch: p.epoch,
                target_ord: p.target_ord as i32,
                span: occ.span,
                flags,
            };
            if occ.role == Role::Definition {
                def_postings[g].push(group_occ);
                def_target_of
                    .entry(occ.key.clone())
                    .or_insert(p.target_ord as i32);
            } else {
                ref_postings[g].push(group_occ);
            }
            if p.facts.editable()
                && !occ.synthetic
                && rename_token_matches(occ, &lines, &display_name_of, &batch)
            {
                let rg = batch.groups.rename_group_index[&occ.key];
                rename_postings[rg].push(GroupOcc {
                    doc_ord: doc_ord as i32,
                    doc_epoch: p.epoch,
                    target_ord: p.target_ord as i32,
                    span: occ.span,
                    flags: flags | occ_flags::EDITABLE as i32,
                });
            }
        }
    }

    // Profiles: batch truth, editable count aligned to materialized postings.
    let rename_profiles: Vec<StoreRenameProfile> = batch
        .rename_profiles
        .iter()
        .enumerate()
        .map(|(rg, prof)| StoreRenameProfile {
            is_local: prof.is_local,
            is_external: prof.is_external,
            has_generated_occurrences: prof.has_generated_occurrences,
            has_readonly_occurrences: prof.has_readonly_occurrences,
            has_override_family: prof.has_override_family,
            has_companion: prof.has_companion,
            editable_occurrence_count: rename_postings[rg].len() as i32,
            unsafe_reason_mask: prof.unsafe_reason_mask as i64,
        })
        .collect();

    let segment_symbols: Vec<SegmentSymbol> = keys
        .iter()
        .map(|k| SegmentSymbol {
            semantic_symbol: encode_key(k),
            symbol_id: symbol_id_for(k),
            ref_group_ord: batch.groups.ref_group_index[k] as i32,
            rename_group_ord: batch.groups.rename_group_index[k] as i32,
            def_target_ord: *def_target_of.get(k).unwrap_or(&-1),
        })
        .collect();

    let symbol_meta: Vec<SymbolMeta> = keys
        .iter()
        .map(|k| {
            let info = info_by_key.get(k);
            let def = def_by_key.get(k);
            SymbolMeta {
                display: info.map(|i| i.display_name.clone()).unwrap_or_default(),
                owner: info.and_then(|i| i.owner_name.clone()).unwrap_or_default(),
                package_name: info
                    .and_then(|i| i.package_name.clone())
                    .unwrap_or_default(),
                kind: info.map(|i| i.kind.code()).unwrap_or(0),
                properties: info.map(|i| i.properties).unwrap_or(0),
                def_packed_start: def
                    .map(|(_, s)| Span::pack(s.start_line, s.start_char) as i32)
                    .unwrap_or(-1),
                def_packed_end: def
                    .map(|(_, s)| Span::pack(s.end_line, s.end_char) as i32)
                    .unwrap_or(-1),
                def_doc_ord: def.map(|(d, _)| *d).unwrap_or(-1),
            }
        })
        .collect();

    let mut search_rows: Vec<SearchRow> = Vec::new();
    for (i, k) in keys.iter().enumerate() {
        if k.is_local() {
            continue;
        }
        let Some(info) = info_by_key.get(k) else {
            continue;
        };
        if !def_by_key.contains_key(k) || !workspace_symbol_kind(info.kind) {
            continue;
        }
        search_rows.push(SearchRow {
            normalized_name: ls_store::search::normalize(&info.display_name),
            symbol_ord: i as i32,
        });
    }
    search_rows.sort_by(|a, b| {
        a.normalized_name
            .cmp(&b.normalized_name)
            .then(a.symbol_ord.cmp(&b.symbol_ord))
    });

    let target_meta: Vec<TargetMeta> = workspace
        .targets
        .iter()
        .map(|t| TargetMeta {
            bsp_id: t.bsp_id.clone(),
            scala_version: t.scala_version.clone(),
            sourceroot: t.sourceroot.to_string_lossy().into_owned(),
            semanticdb_root: t.semanticdb_root.to_string_lossy().into_owned(),
            content_hash: t.content_hash,
            options_hash: t.options_hash,
        })
        .collect();
    let targets: Vec<i64> = workspace
        .targets
        .iter()
        .map(|t| target_id_for(&t.bsp_id))
        .collect();

    let docs: Vec<SegmentDoc> = primaries
        .iter()
        .map(|p| SegmentDoc {
            uri: p.uri.clone(),
            doc_id: p.doc_id.value() as i64,
            epoch: p.epoch,
            target_ord: p.target_ord as i32,
            generated: p.facts.generated,
            readonly: p.facts.readonly,
        })
        .collect();

    let data = SegmentData {
        docs,
        targets,
        symbols: segment_symbols,
        ref_occurrences: ref_postings,
        def_occurrences: def_postings,
        rename_occurrences: rename_postings,
        rename_profiles,
        doc_occurrences,
        target_meta,
        symbol_meta,
        search_rows,
    };

    let created_at_ms = now_ms();
    let snapshot = store.publish(&data, &new_state.encode(), created_at_ms)?;
    store.run_janitor();

    let report = IngestReport {
        segment_id: snapshot.segment_id(),
        docs_indexed: primaries.len(),
        docs_shared: shared_count,
        docs_stale: stale_uris.len(),
        docs_skipped: skipped_uris.len(),
        symbol_count: keys.len(),
        ref_group_count,
        rename_group_count,
        stale_uris,
        skipped_uris,
        duration_ms: t0.elapsed().as_millis() as u64,
    };
    Ok((report, snapshot))
}

fn ord_val(k: &SymbolKey) -> i64 {
    k.local_doc.map(|d| d.value() as i64).unwrap_or(-1)
}

fn occ_flags_for(occ: &Occurrence, facts: &DocFacts) -> i32 {
    let mut f = 0u32;
    if occ.role == Role::Definition {
        f |= occ_flags::DEFINITION;
    }
    if facts.editable() {
        f |= occ_flags::EDITABLE;
    }
    if facts.generated {
        f |= occ_flags::GENERATED;
    }
    if facts.readonly {
        f |= occ_flags::READONLY;
    }
    if occ.synthetic {
        f |= occ_flags::SYNTHETIC;
    }
    f as i32
}

fn workspace_symbol_kind(kind: SymKind) -> bool {
    matches!(
        kind,
        SymKind::Class
            | SymKind::Trait
            | SymKind::Interface
            | SymKind::Object
            | SymKind::PackageObject
            | SymKind::Method
            | SymKind::Macro
            | SymKind::Type
            | SymKind::Field
    )
}

/// True when the source token under `occ.span` is the identifier a rename would
/// rewrite. Multi-line spans never match.
fn rename_token_matches(
    occ: &Occurrence,
    lines: &[&str],
    display_name_of: &HashMap<SymbolKey, String>,
    batch: &SemanticBatch,
) -> bool {
    if occ.span.start_line != occ.span.end_line {
        return false;
    }
    match token_at(lines, &occ.span) {
        None => false,
        Some(token) => match expected_token(&occ.key, display_name_of, batch) {
            Some(expected) => token == expected,
            None => true,
        },
    }
}

fn token_at(lines: &[&str], span: &Span) -> Option<String> {
    let sl = span.start_line as usize;
    if sl >= lines.len() {
        return None;
    }
    let chars: Vec<char> = lines[sl].chars().collect();
    let sc = span.start_char as usize;
    let ec = span.end_char as usize;
    if ec > chars.len() || sc > ec {
        return None;
    }
    let raw: String = chars[sc..ec].iter().collect();
    let token = if raw.chars().count() >= 2 && raw.starts_with('`') && raw.ends_with('`') {
        raw[1..raw.len() - 1].to_string()
    } else {
        raw
    };
    Some(token)
}

fn expected_token(
    key: &SymbolKey,
    display_name_of: &HashMap<SymbolKey, String>,
    batch: &SemanticBatch,
) -> Option<String> {
    if key.is_local() {
        return display_name_of.get(key).cloned();
    }
    match symbols::split_last(&key.semantic_symbol) {
        Some((owner, Descriptor::Method(name, _))) => {
            if name == symbols::CONSTRUCTOR_NAME {
                symbols::display_name(&owner)
            } else if name.chars().count() > 2 && name.ends_with("_=") {
                Some(name[..name.len() - 2].to_string())
            } else if name == "apply" || name == "unapply" {
                let class_key = symbols::companion(&owner).map(SymbolKey::global);
                let same_group = class_key.as_ref().is_some_and(|ck| {
                    batch.groups.rename_group_of(ck) == batch.groups.rename_group_of(key)
                });
                if same_group {
                    symbols::display_name(&owner)
                } else {
                    Some(name)
                }
            } else {
                Some(name)
            }
        }
        _ => display_name_of
            .get(key)
            .cloned()
            .or_else(|| symbols::display_name(&key.semantic_symbol)),
    }
}

fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
