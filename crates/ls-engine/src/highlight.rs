//! textDocument/documentHighlight: all same-document occurrences of the symbol
//! at the cursor, split read/write by occurrence role (Definition -> Write,
//! Reference -> Read).
//!
//! Doc postings identify the cursor symbol; the same-symbol occurrences inside
//! the document are its ref/def group postings restricted to the document's
//! ordinal. Same-document only by construction, so no target pruning is needed.

use std::collections::HashSet;

use ls_index_model::{occ_flags, LsError, Role, Span};
use ls_store::{GroupRecord, Snapshot};

use crate::orchestrator::{CursorSymbol, QueryOrchestrator, ResolutionSource};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HighlightKind {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocHighlight {
    pub span: Span,
    pub kind: HighlightKind,
}

pub struct DocumentHighlightService<'a> {
    orch: &'a QueryOrchestrator,
}

impl<'a> DocumentHighlightService<'a> {
    pub fn new(orch: &'a QueryOrchestrator) -> Self {
        DocumentHighlightService { orch }
    }

    pub fn highlights(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<DocHighlight>, LsError> {
        let cursor = self.orch.symbol_at_cursor(uri, line, character)?;
        if cursor.pc_only {
            return Err(LsError::PcOnlySymbol);
        }

        let hits = self
            .orch
            .current_snapshot()
            .and_then(|snap| self.snapshot_highlights(snap.as_ref(), &cursor));
        let hits = match hits {
            Some(hits) => hits,
            None => {
                if cursor.source == ResolutionSource::RawSemanticdb {
                    self.orch
                        .raw_doc_occurrences(&cursor.uri, &cursor.semantic_symbol)
                        .into_iter()
                        .map(|(span, role)| DocHighlight {
                            span,
                            kind: kind_of(role),
                        })
                        .collect()
                } else {
                    return Err(LsError::NotIndexed {
                        uri: uri.to_string(),
                    });
                }
            }
        };
        Ok(dedupe_and_sort(hits))
    }

    fn snapshot_highlights(
        &self,
        snap: &Snapshot,
        cursor: &CursorSymbol,
    ) -> Option<Vec<DocHighlight>> {
        let seg = snap.segment();
        let doc_ord = (0..seg.doc_count()).find(|&d| seg.uri_of(d) == cursor.uri)?;
        let ord = seg.find_symbol_ord(&cursor.encoded_symbol())?;
        let ref_group = seg.symbol_view(ord).ref_group_ord;
        if ref_group < 0 {
            return Some(Vec::new());
        }
        let group = ref_group as u32;
        let mut out: Vec<DocHighlight> = Vec::new();
        {
            let out_ref = &mut out;
            let mut sink = |rec: GroupRecord| {
                if rec.doc_ord as u32 == doc_ord {
                    out_ref.push(record_to_highlight(rec));
                }
            };
            seg.scan_ref_group(group, None, &mut sink);
            seg.scan_def_group(group, &mut sink);
        }
        Some(out)
    }
}

fn record_to_highlight(rec: GroupRecord) -> DocHighlight {
    let ps = rec.packed_start as u32;
    let pe = rec.packed_end as u32;
    let span = Span::new(
        Span::unpack_line(ps),
        Span::unpack_char(ps),
        Span::unpack_line(pe),
        Span::unpack_char(pe),
    );
    let kind = if occ_flags::has(rec.flags as u32, occ_flags::DEFINITION) {
        HighlightKind::Write
    } else {
        HighlightKind::Read
    };
    DocHighlight { span, kind }
}

fn kind_of(role: Role) -> HighlightKind {
    match role {
        Role::Definition => HighlightKind::Write,
        Role::Reference => HighlightKind::Read,
    }
}

fn dedupe_and_sort(hits: Vec<DocHighlight>) -> Vec<DocHighlight> {
    let mut seen: HashSet<DocHighlight> = HashSet::new();
    let mut out: Vec<DocHighlight> = Vec::new();
    for h in hits {
        if seen.insert(h) {
            out.push(h);
        }
    }
    out.sort_by(|a, b| {
        a.span
            .start_line
            .cmp(&b.span.start_line)
            .then(a.span.start_char.cmp(&b.span.start_char))
            .then(a.span.end_line.cmp(&b.span.end_line))
            .then(a.span.end_char.cmp(&b.span.end_char))
    });
    out
}
