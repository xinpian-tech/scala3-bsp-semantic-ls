//! Workspace references over exact mmap postings.
//!
//! Pipeline: symbol-at-cursor -> ref group -> allowed targets = reverse
//! dependency closure of the definition target -> scan references (+ scan
//! definitions when includeDeclaration; definitions are also restricted to the
//! allowed targets so disconnected targets that reuse symbol names never leak
//! in) -> dedupe -> sorted by (uri, position). The epoch filter runs inside the
//! segment reader. The dirty-buffer overlay may add hits, marked `from_overlay`.

use std::collections::HashSet;

use ls_index_model::{occ_flags, Loc, LsError, Role, Span};
use ls_store::{GroupRecord, SegmentReader, Snapshot};

use crate::orchestrator::{CursorSymbol, QueryOrchestrator, ResolutionSource};
use crate::symbol_encoding;

/// One reference location. `from_overlay` marks dirty-buffer additions (not
/// index truth); `role` is `Definition` for declaration hits surfaced by
/// includeDeclaration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferenceHit {
    pub loc: Loc,
    pub role: Role,
    pub from_overlay: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReferencesResult {
    pub hits: Vec<ReferenceHit>,
    pub needs_reindex: bool,
}

impl ReferencesResult {
    pub fn locations(&self) -> Vec<Loc> {
        self.hits.iter().map(|h| h.loc.clone()).collect()
    }
}

pub struct ReferencesEngine<'a> {
    orch: &'a QueryOrchestrator,
}

impl<'a> ReferencesEngine<'a> {
    pub fn new(orch: &'a QueryOrchestrator) -> Self {
        ReferencesEngine { orch }
    }

    pub fn references(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        include_declaration: bool,
    ) -> Result<ReferencesResult, LsError> {
        let cursor = self.orch.symbol_at_cursor(uri, line, character)?;
        if cursor.pc_only {
            return Err(LsError::PcOnlySymbol);
        }

        let index_hits: Vec<ReferenceHit> = match self.orch.current_snapshot() {
            Some(snap) => self.snapshot_hits(&snap, &cursor, include_declaration)?,
            None => {
                if cursor.source == ResolutionSource::RawSemanticdb {
                    self.raw_fallback(&cursor, include_declaration)
                } else {
                    return Err(LsError::NotIndexed {
                        uri: uri.to_string(),
                    });
                }
            }
        };

        // Group-keyed dirty-buffer overlay: fan the overlay query across every
        // member of the cursor's alias group. Skipped entirely for overlays that
        // contribute no occurrences (the production PC overlay).
        let overlay_hits: Vec<ReferenceHit> = if !self.orch.overlay().contributes_occurrences() {
            Vec::new()
        } else {
            let mut seen: HashSet<Span> = HashSet::new();
            let mut hits = Vec::new();
            for sym in self.overlay_group_symbols(&cursor) {
                if let Some(occs) = self.orch.overlay().occurrences_of(&sym) {
                    for loc in occs {
                        if seen.insert(loc.span) {
                            hits.push(ReferenceHit {
                                loc,
                                role: Role::Reference,
                                from_overlay: true,
                            });
                        }
                    }
                }
            }
            hits
        };

        let mut all = index_hits;
        all.extend(overlay_hits);
        Ok(ReferencesResult {
            hits: dedupe_and_sort(all),
            needs_reindex: cursor.needs_reindex,
        })
    }

    /// The raw semantic symbols of the cursor's alias (ref) group, for the
    /// group-keyed overlay fan-out. Always includes the cursor's own symbol.
    fn overlay_group_symbols(&self, cursor: &CursorSymbol) -> Vec<String> {
        let mut out = vec![cursor.semantic_symbol.clone()];
        if let Some(snap) = self.orch.current_snapshot() {
            let seg = snap.segment();
            if let Some(ord) = seg.find_symbol_ord(&cursor.encoded_symbol()) {
                let g = seg.symbol_view(ord).ref_group_ord;
                if g >= 0 {
                    for o in 0..seg.symbol_count() as u32 {
                        if seg.symbol_view(o).ref_group_ord == g {
                            out.push(symbol_encoding::decode(seg.semantic_symbol_of(o)).0);
                        }
                    }
                }
            }
        }
        dedupe_preserve(out)
    }

    fn snapshot_hits(
        &self,
        snap: &Snapshot,
        cursor: &CursorSymbol,
        include_declaration: bool,
    ) -> Result<Vec<ReferenceHit>, LsError> {
        let seg = snap.segment();
        match seg.find_symbol_ord(&cursor.encoded_symbol()) {
            None => {
                // Fresh symbol not in the snapshot yet: RawSemanticDBPath serves
                // its own document; anything else is a stale index.
                if cursor.source == ResolutionSource::RawSemanticdb {
                    Ok(self.raw_fallback(cursor, include_declaration))
                } else {
                    Err(LsError::StaleIndex {
                        uri: cursor.uri.clone(),
                    })
                }
            }
            Some(ord) => {
                let ref_group = seg.symbol_view(ord).ref_group_ord;
                if ref_group < 0 {
                    return Ok(Vec::new());
                }
                let group = ref_group as u32;
                let allowed = self.orch.allowed_targets_for(snap, ord);
                let mut out: Vec<ReferenceHit> = Vec::new();
                {
                    let out_ref = &mut out;
                    let mut sink = |rec: GroupRecord| {
                        // The reader only prunes reference scans by target; the
                        // sink enforces the allowed set for definition scans too.
                        if allowed.contains(rec.target_ord as u32) {
                            out_ref.push(record_to_hit(seg, rec));
                        }
                    };
                    seg.scan_ref_group(group, Some(&allowed), &mut sink);
                    if include_declaration {
                        seg.scan_def_group(group, &mut sink);
                    }
                }
                Ok(out)
            }
        }
    }

    fn raw_fallback(&self, cursor: &CursorSymbol, include_declaration: bool) -> Vec<ReferenceHit> {
        self.orch
            .raw_doc_occurrences(&cursor.uri, &cursor.semantic_symbol)
            .into_iter()
            .filter(|(_, role)| include_declaration || *role == Role::Reference)
            .map(|(span, role)| ReferenceHit {
                loc: Loc::new(cursor.uri.clone(), span),
                role,
                from_overlay: false,
            })
            .collect()
    }
}

fn record_to_hit(seg: &SegmentReader, rec: GroupRecord) -> ReferenceHit {
    let ps = rec.packed_start as u32;
    let pe = rec.packed_end as u32;
    let span = Span::new(
        Span::unpack_line(ps),
        Span::unpack_char(ps),
        Span::unpack_line(pe),
        Span::unpack_char(pe),
    );
    let role = if occ_flags::has(rec.flags as u32, occ_flags::DEFINITION) {
        Role::Definition
    } else {
        Role::Reference
    };
    ReferenceHit {
        loc: Loc::new(seg.uri_of(rec.doc_ord as u32).to_string(), span),
        role,
        from_overlay: false,
    }
}

fn dedupe_preserve(items: Vec<String>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for it in items {
        if seen.insert(it.clone()) {
            out.push(it);
        }
    }
    out
}

fn dedupe_and_sort(hits: Vec<ReferenceHit>) -> Vec<ReferenceHit> {
    let mut seen: HashSet<(String, Span)> = HashSet::new();
    let mut out: Vec<ReferenceHit> = Vec::new();
    for h in hits {
        if seen.insert((h.loc.uri.clone(), h.loc.span)) {
            out.push(h);
        }
    }
    out.sort_by(|a, b| {
        a.loc
            .uri
            .cmp(&b.loc.uri)
            .then(a.loc.span.start_line.cmp(&b.loc.span.start_line))
            .then(a.loc.span.start_char.cmp(&b.loc.span.start_char))
            .then(a.loc.span.end_line.cmp(&b.loc.span.end_line))
            .then(a.loc.span.end_char.cmp(&b.loc.span.end_char))
    });
    out
}
