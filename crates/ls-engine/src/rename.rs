//! Cross-file rename with the FreshRequired consistency level.
//!
//! `rename` drives, in order: dirty-buffer / PC-only check; new-name validation;
//! prepareRename pre-checks on the current state; compile-before-rename over the
//! definition target's reverse dependency closure; a full fresh ingest; re-
//! resolution on the FRESH snapshot with the rename group's `unsafe_reason_mask`
//! gate; the editable rename-postings scan; a shared-source consistency check
//! for every edited uri that belongs to more than one target; and an md5 re-
//! validation of every edited document immediately before emitting the plan.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::Path;

use ls_index_model::{occ_flags, unsafe_reason, LsError, Span};
use ls_semanticdb::model::sdb_role;
use ls_semanticdb::parse_file;
use ls_store::{GroupRecord, Snapshot};

use crate::identifiers;
use crate::orchestrator::{CursorSymbol, QueryOrchestrator, ResolutionSource};

/// Compile hook: the server passes the real BSP session (`buildTarget/compile`
/// over the rename domain); tests stub it.
pub enum CompileOutcome {
    Ok,
    Failed { reason: String },
}

/// `Send + Sync` so a `CoreServices` carrying a boxed compiler can be built on a
/// bootstrap worker thread and handed back to the message loop.
pub trait CompileService: Send + Sync {
    fn compile(&self, targets: &[String]) -> CompileOutcome;
}

/// One text edit inside a document, span in SemanticDB/LSP coordinates
/// (zero-based, end-exclusive character).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEditSpan {
    pub span: Span,
    pub new_text: String,
}

/// The rename result: edits grouped by SemanticDB uri (sourceroot-relative).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceEditPlan {
    pub edits: BTreeMap<String, Vec<TextEditSpan>>,
    pub occurrence_count: usize,
}

pub struct RenameEngine<'a> {
    orch: &'a QueryOrchestrator,
    compiler: &'a dyn CompileService,
}

impl<'a> RenameEngine<'a> {
    pub fn new(orch: &'a QueryOrchestrator, compiler: &'a dyn CompileService) -> Self {
        RenameEngine { orch, compiler }
    }

    /// Validates that a rename can start at this position and returns the span
    /// of the symbol occurrence under the cursor.
    pub fn prepare_rename(&self, uri: &str, line: u32, character: u32) -> Result<Span, LsError> {
        if self.orch.overlay().is_dirty(uri) {
            return match self.orch.overlay().symbol_at(uri, line, character) {
                Some(hit) if hit.pc_only => Err(LsError::PcOnlySymbol),
                Some(hit) => Ok(hit.span),
                None => Err(LsError::StaleIndex {
                    uri: uri.to_string(),
                }),
            };
        }
        let cursor = self.orch.symbol_at_cursor(uri, line, character)?;
        self.require_editable_cursor_doc(uri)?;
        Ok(cursor.span)
    }

    pub fn rename(
        &self,
        uri: &str,
        line: u32,
        character: u32,
        new_name: &str,
    ) -> Result<WorkspaceEditPlan, LsError> {
        let workspace = self.orch.workspace().ok_or_else(|| LsError::NotIndexed {
            uri: uri.to_string(),
        })?;

        // 1. dirty buffer / PC-only
        if self.orch.overlay().is_dirty(uri) {
            return match self.orch.overlay().symbol_at(uri, line, character) {
                Some(hit) if hit.pc_only => Err(LsError::PcOnlySymbol),
                _ => Err(LsError::RenameRejected {
                    reasons: vec![format!(
                        "{uri} has unsaved changes; save the file before renaming"
                    )],
                }),
            };
        }

        // 2. new-name validation
        let new_text = match identifiers::encode(new_name) {
            Ok(t) => t,
            Err(msg) => return Err(LsError::RenameRejected { reasons: vec![msg] }),
        };

        // 3. prepareRename pre-checks on current state
        let pre = self.orch.symbol_at_cursor(uri, line, character)?;
        if pre.pc_only {
            return Err(LsError::PcOnlySymbol);
        }
        self.require_editable_cursor_doc(uri)?;

        // 4. compile-before-rename over the exact affected domain
        let def_bsp = self
            .current_definition_bsp(&pre)
            .or_else(|| self.orch.primary_bsp_of(uri));
        let domain: Vec<String> = match &def_bsp {
            Some(b) => {
                let mut v: Vec<String> = workspace
                    .reverse_dependency_closure(b)
                    .into_iter()
                    .collect();
                v.sort();
                v
            }
            None => workspace.targets.iter().map(|t| t.bsp_id.clone()).collect(),
        };
        if let CompileOutcome::Failed { .. } = self.compiler.compile(&domain) {
            return Err(LsError::CompileFailed {
                target: def_bsp.unwrap_or_else(|| domain.join(", ")),
            });
        }

        // 5. fresh ingest -> fresh snapshot (FreshRequired)
        self.orch
            .ingest(workspace.clone())
            .map_err(|_| LsError::StaleIndex {
                uri: uri.to_string(),
            })?;

        // 6. resolve on the FRESH snapshot
        let cursor = self.orch.symbol_at_cursor(uri, line, character)?;
        if cursor.source != ResolutionSource::Snapshot {
            return Err(LsError::StaleIndex {
                uri: uri.to_string(),
            });
        }

        let snap = self
            .orch
            .current_snapshot()
            .ok_or_else(|| LsError::NotIndexed {
                uri: uri.to_string(),
            })?;
        let raw_edits = self.collect_edits(&snap, &cursor)?;

        if raw_edits.is_empty() {
            return Err(LsError::RenameRejected {
                reasons: vec!["rename found no editable occurrences".to_string()],
            });
        }
        if raw_edits
            .iter()
            .all(|(_, _, flags)| occ_flags::has(*flags as u32, occ_flags::SYNTHETIC))
        {
            return Err(LsError::RenameRejected {
                reasons: explain(unsafe_reason::SYNTHETIC_ONLY),
            });
        }

        let mut edits_by_uri: BTreeMap<String, Vec<Span>> = BTreeMap::new();
        for (u, span, _flags) in &raw_edits {
            edits_by_uri.entry(u.clone()).or_default().push(*span);
        }
        for spans in edits_by_uri.values_mut() {
            spans.sort_by(span_order);
            spans.dedup();
        }

        // 8. shared-source consistency: every target compiling an edited uri must
        // see the same symbols at every edit span.
        for (edited_uri, spans) in &edits_by_uri {
            self.check_shared_source_consistency(edited_uri, spans)?;
        }

        // 9. md5 re-validation of every edited doc right before emitting.
        for edited_uri in edits_by_uri.keys() {
            if self.orch.primary_spec_of(edited_uri).is_none() {
                return Err(LsError::NotIndexed {
                    uri: edited_uri.clone(),
                });
            }
            if !self.orch.source_is_fresh_uri(edited_uri) {
                return Err(LsError::StaleIndex {
                    uri: edited_uri.clone(),
                });
            }
        }

        let occurrence_count = edits_by_uri.values().map(Vec::len).sum();
        let edits = edits_by_uri
            .into_iter()
            .map(|(u, spans)| {
                let text_edits = spans
                    .into_iter()
                    .map(|s| TextEditSpan {
                        span: s,
                        new_text: new_text.clone(),
                    })
                    .collect();
                (u, text_edits)
            })
            .collect();
        Ok(WorkspaceEditPlan {
            edits,
            occurrence_count,
        })
    }

    fn collect_edits(
        &self,
        snap: &Snapshot,
        cursor: &CursorSymbol,
    ) -> Result<Vec<(String, Span, i32)>, LsError> {
        let seg = snap.segment();
        let ord = seg
            .find_symbol_ord(&cursor.encoded_symbol())
            .ok_or_else(|| LsError::StaleIndex {
                uri: cursor.uri.clone(),
            })?;
        let rg = seg.symbol_view(ord).rename_group_ord;
        if rg < 0 {
            return Err(LsError::RenameRejected {
                reasons: vec!["symbol has no rename group".to_string()],
            });
        }
        let group = rg as u32;
        let profile = seg.rename_profile(group);
        if profile.unsafe_reason_mask != 0 {
            return Err(LsError::RenameRejected {
                reasons: explain(profile.unsafe_reason_mask as u64),
            });
        }
        let mut out: Vec<(String, Span, i32)> = Vec::new();
        {
            let out_ref = &mut out;
            let mut sink = |rec: GroupRecord| {
                let ps = rec.packed_start as u32;
                let pe = rec.packed_end as u32;
                out_ref.push((
                    seg.uri_of(rec.doc_ord as u32).to_string(),
                    Span::new(
                        Span::unpack_line(ps),
                        Span::unpack_char(ps),
                        Span::unpack_line(pe),
                        Span::unpack_char(pe),
                    ),
                    rec.flags,
                ));
            };
            seg.scan_rename_group(group, &mut sink);
        }
        Ok(out)
    }

    fn require_editable_cursor_doc(&self, uri: &str) -> Result<(), LsError> {
        let Some(workspace) = self.orch.workspace() else {
            return Ok(());
        };
        let Some(bsp) = self.orch.primary_bsp_of(uri) else {
            return Ok(());
        };
        let facts = workspace.facts_for(&bsp, uri);
        if !facts.editable() {
            let mut mask = 0u64;
            if facts.generated {
                mask |= unsafe_reason::GENERATED_OCCURRENCE;
            }
            if facts.readonly {
                mask |= unsafe_reason::READONLY_OCCURRENCE;
            }
            if facts.is_dependency_source {
                mask |= unsafe_reason::DEPENDENCY_SOURCE;
            }
            return Err(LsError::RenameRejected {
                reasons: explain(mask),
            });
        }
        Ok(())
    }

    fn current_definition_bsp(&self, cursor: &CursorSymbol) -> Option<String> {
        let snap = self.orch.current_snapshot()?;
        let ord = snap.segment().find_symbol_ord(&cursor.encoded_symbol())?;
        self.orch.definition_bsp_of(&snap, ord)
    }

    /// All targets that compile `uri` must agree on the symbols at every edit
    /// span, otherwise the same textual edit would rename different rename groups
    /// in different targets.
    fn check_shared_source_consistency(&self, uri: &str, spans: &[Span]) -> Result<(), LsError> {
        let files = self.orch.semanticdb_files_for(uri);
        if files.len() <= 1 {
            return Ok(());
        }
        let per_target: Vec<HashMap<Span, HashSet<String>>> = files
            .iter()
            .map(|f| symbols_at_spans(f, uri, spans))
            .collect();
        let reference = &per_target[0];
        let disagree = per_target
            .iter()
            .any(|m| spans.iter().any(|s| m.get(s) != reference.get(s)));
        let empty_ref = spans.iter().any(|s| match reference.get(s) {
            None => true,
            Some(set) => set.is_empty(),
        });
        if disagree || empty_ref {
            return Err(LsError::RenameRejected {
                reasons: explain(unsafe_reason::SHARED_SOURCE_DISAGREEMENT),
            });
        }
        Ok(())
    }
}

fn symbols_at_spans(file: &Path, uri: &str, spans: &[Span]) -> HashMap<Span, HashSet<String>> {
    let Ok(docs) = parse_file(file) else {
        return HashMap::new();
    };
    let Some(doc) = docs.documents.into_iter().find(|d| d.uri == uri) else {
        return HashMap::new();
    };
    let wanted: HashSet<Span> = spans.iter().copied().collect();
    let mut acc: HashMap<Span, HashSet<String>> = HashMap::new();
    for o in &doc.occurrences {
        if o.role_code != sdb_role::REFERENCE && o.role_code != sdb_role::DEFINITION {
            continue;
        }
        if let Some(r) = &o.range {
            let span = Span::new(
                r.start_line as u32,
                r.start_character as u32,
                r.end_line as u32,
                r.end_character as u32,
            );
            if wanted.contains(&span) {
                acc.entry(span).or_default().insert(o.symbol.clone());
            }
        }
    }
    acc
}

fn span_order(a: &Span, b: &Span) -> std::cmp::Ordering {
    a.start_line
        .cmp(&b.start_line)
        .then(a.start_char.cmp(&b.start_char))
        .then(a.end_line.cmp(&b.end_line))
        .then(a.end_char.cmp(&b.end_char))
}

fn explain(mask: u64) -> Vec<String> {
    unsafe_reason::explain(mask)
        .into_iter()
        .map(|s| s.to_string())
        .collect()
}
