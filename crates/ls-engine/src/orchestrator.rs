//! Owns the store and implements the three query paths.
//!
//! symbol-at-cursor resolution order:
//!   1. dirty file: the overlay must answer, otherwise the query degrades
//!      ([`LsError::StaleIndex`]) — the index must not pretend to know a buffer
//!      it has not seen;
//!   2. clean file whose on-disk source still matches the indexed md5: snapshot
//!      doc postings (IndexPath);
//!   3. stale or unindexed file: RawSemanticDBPath — parse the document's
//!      `.semanticdb` directly, md5-validate against the current source, serve
//!      from it, and flag `needs_reindex` (best-effort synchronous write-through
//!      republishes the segment so the next query resolves from the snapshot).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use ls_index_model::{LsError, NormalizedDocument, Occurrence, Role, Span, TargetBitset};
use ls_semanticdb::{md5, normalize, parse_file, SemanticdbLocator};
use ls_store::{SearchIndex, Snapshot, Store, StoreResult, WorkspaceSymbolHit};

use crate::hash::doc_id_for;
use crate::ingest::{self, IngestReport};
use crate::overlay::{DirtyBufferOverlay, NoopOverlay};
use crate::state::IngestState;
use crate::symbol_encoding;
use crate::targets::{TargetSpec, WorkspaceTargets};

/// Where a cursor resolution came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResolutionSource {
    Snapshot,
    RawSemanticdb,
    Overlay,
}

/// Resolved symbol under the cursor. `semantic_symbol` is the raw SemanticDB
/// string; local symbols additionally carry the persistent doc id.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CursorSymbol {
    pub uri: String,
    pub semantic_symbol: String,
    pub local_doc_id: Option<u64>,
    pub span: Span,
    pub role: Role,
    pub source: ResolutionSource,
    pub needs_reindex: bool,
    pub pc_only: bool,
}

impl CursorSymbol {
    pub fn is_local(&self) -> bool {
        self.local_doc_id.is_some()
    }
    pub fn encoded_symbol(&self) -> String {
        symbol_encoding::encode(&self.semantic_symbol, self.local_doc_id)
    }
}

type BoxOverlay = Box<dyn DirtyBufferOverlay + Send + Sync>;

pub struct QueryOrchestrator {
    store: Store,
    overlay: BoxOverlay,
    sync_write_through: bool,
    current_workspace: Mutex<Option<Arc<WorkspaceTargets>>>,
    last_write_through_thread: Mutex<Option<String>>,
}

impl QueryOrchestrator {
    pub fn new(store: Store, overlay: BoxOverlay, sync_write_through: bool) -> Self {
        QueryOrchestrator {
            store,
            overlay,
            sync_write_through,
            current_workspace: Mutex::new(None),
            last_write_through_thread: Mutex::new(None),
        }
    }

    pub fn with_defaults(store: Store) -> Self {
        Self::new(store, Box::new(NoopOverlay), true)
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn overlay(&self) -> &(dyn DirtyBufferOverlay + Send + Sync) {
        self.overlay.as_ref()
    }

    pub fn current_snapshot(&self) -> Option<Arc<Snapshot>> {
        self.store.current()
    }

    pub fn workspace(&self) -> Option<Arc<WorkspaceTargets>> {
        self.current_workspace.lock().unwrap().clone()
    }

    /// Name/label of the thread that ran the most recent raw-path write-through,
    /// or `None` if none has run. Proves write-through executes inline on the
    /// calling (single index-executor) thread.
    pub fn last_write_through_thread_name(&self) -> Option<String> {
        self.last_write_through_thread.lock().unwrap().clone()
    }

    /// Runs a full-generation ingest and remembers the workspace description for
    /// target-graph pruning.
    pub fn ingest(&self, workspace: Arc<WorkspaceTargets>) -> StoreResult<IngestReport> {
        let (report, _snap) = ingest::ingest(&self.store, &workspace)?;
        *self.current_workspace.lock().unwrap() = Some(workspace);
        Ok(report)
    }

    // --- workspace symbol (BestEffort) ---

    pub fn workspace_symbol(&self, query: &str, limit: usize) -> Vec<WorkspaceSymbolHit> {
        match self.store.current() {
            Some(snap) => SearchIndex::build(snap.segment()).workspace_symbol_search(query, limit),
            None => Vec::new(),
        }
    }

    pub fn workspace_symbol_name_exists(&self, name: &str) -> bool {
        match self.store.current() {
            Some(snap) => SearchIndex::build(snap.segment()).workspace_symbol_name_exists(name),
            None => false,
        }
    }

    // --- symbol at cursor ---

    pub fn symbol_at_cursor(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<CursorSymbol, LsError> {
        if self.overlay.is_dirty(uri) {
            return match self.overlay.symbol_at(uri, line, character) {
                Some(hit) => Ok(CursorSymbol {
                    uri: uri.to_string(),
                    semantic_symbol: hit.semantic_symbol,
                    local_doc_id: None,
                    span: hit.span,
                    role: hit.role,
                    source: ResolutionSource::Overlay,
                    needs_reindex: false,
                    pc_only: hit.pc_only,
                }),
                // Dirty buffer and PC cannot answer: degrade, never guess from a
                // snapshot that has not seen the buffer.
                None => Err(LsError::StaleIndex {
                    uri: uri.to_string(),
                }),
            };
        }
        let from_snapshot = match self.store.current() {
            Some(snap) => self.snapshot_cursor(&snap, uri, line, character)?,
            None => None,
        };
        match from_snapshot {
            Some(hit) => Ok(hit),
            None => {
                let cursor = self.raw_semanticdb_cursor(uri, line, character)?;
                if self.write_through_raw_path() {
                    Ok(CursorSymbol {
                        needs_reindex: false,
                        ..cursor
                    })
                } else {
                    Ok(cursor)
                }
            }
        }
    }

    /// Synchronously heals the index after a RawSemanticDBPath resolution by
    /// reusing the full-generation ingest. Runs inline on the calling thread,
    /// preserving the single-writer contract. Best-effort: returns true only
    /// after a successful synchronous publish; an ingest failure never
    /// propagates (the raw path already answered).
    fn write_through_raw_path(&self) -> bool {
        if !self.sync_write_through {
            return false;
        }
        let Some(ws) = self.workspace() else {
            return false;
        };
        *self.last_write_through_thread.lock().unwrap() = Some(thread_label());
        ingest::ingest(&self.store, &ws).is_ok()
    }

    fn snapshot_cursor(
        &self,
        snap: &Snapshot,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Option<CursorSymbol>, LsError> {
        let Some(doc_ord) = self.doc_ord_of(snap, uri) else {
            return Ok(None);
        };
        if !self.source_is_fresh(snap, uri) {
            return Ok(None);
        }
        match snap.segment().symbol_at(doc_ord, line, character) {
            Some(hit) => {
                let encoded = snap.segment().semantic_symbol_of(hit.symbol_ord as u32);
                let (raw, local_doc) = symbol_encoding::decode(encoded);
                Ok(Some(CursorSymbol {
                    uri: uri.to_string(),
                    semantic_symbol: raw,
                    local_doc_id: local_doc,
                    span: hit.span,
                    role: hit.role,
                    source: ResolutionSource::Snapshot,
                    needs_reindex: false,
                    pc_only: false,
                }))
            }
            None => Err(LsError::NoSymbolAtCursor {
                uri: uri.to_string(),
                line,
                character,
            }),
        }
    }

    fn raw_semanticdb_cursor(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<CursorSymbol, LsError> {
        let doc = self.raw_normalized_doc(uri)?;
        match occurrence_at(&doc.occurrences, line, character) {
            Some(occ) => Ok(CursorSymbol {
                uri: uri.to_string(),
                semantic_symbol: occ.key.semantic_symbol.clone(),
                local_doc_id: occ.key.local_doc.map(|d| d.value()),
                span: occ.span,
                role: occ.role,
                source: ResolutionSource::RawSemanticdb,
                needs_reindex: true,
                pc_only: false,
            }),
            None => Err(LsError::NoSymbolAtCursor {
                uri: uri.to_string(),
                line,
                character,
            }),
        }
    }

    /// All same-document occurrences of `semantic_symbol` served straight from
    /// the raw `.semanticdb` of `uri` — the RawSemanticDBPath fallback for
    /// references when a fresh symbol is not in the snapshot yet.
    pub fn raw_doc_occurrences(&self, uri: &str, semantic_symbol: &str) -> Vec<(Span, Role)> {
        match self.raw_normalized_doc(uri) {
            Ok(doc) => doc
                .occurrences
                .iter()
                .filter(|o| o.key.semantic_symbol == semantic_symbol)
                .map(|o| (o.span, o.role))
                .collect(),
            Err(_) => Vec::new(),
        }
    }

    fn raw_normalized_doc(&self, uri: &str) -> Result<NormalizedDocument, LsError> {
        let spec = match self.primary_spec_of(uri) {
            Some(spec) => spec,
            // No `.semanticdb` for this uri. A source that IS a workspace target
            // source but produced no SemanticDB is a hard `NoSemanticdb` (no
            // fallback); a source outside every target is `NotIndexed`.
            None => {
                return Err(if self.source_target_of(uri).is_some() {
                    LsError::NoSemanticdb {
                        uri: uri.to_string(),
                    }
                } else {
                    LsError::NotIndexed {
                        uri: uri.to_string(),
                    }
                });
            }
        };
        let locator = SemanticdbLocator::new(spec.semanticdb_root.clone());
        let file = locator
            .semanticdb_file_for(uri)
            .map_err(|_| LsError::StaleIndex {
                uri: uri.to_string(),
            })?;
        let docs = parse_file(&file).map_err(|_| LsError::StaleIndex {
            uri: uri.to_string(),
        })?;
        let sdb = docs
            .documents
            .into_iter()
            .find(|d| d.uri == uri)
            .ok_or_else(|| LsError::StaleIndex {
                uri: uri.to_string(),
            })?;
        let text = self
            .source_text_of(uri)
            .ok_or_else(|| LsError::StaleIndex {
                uri: uri.to_string(),
            })?;
        if !md5::validate_doc(&text, &sdb).is_fresh() {
            return Err(LsError::StaleIndex {
                uri: uri.to_string(),
            });
        }
        Ok(normalize(&sdb, doc_id_for(uri)))
    }

    // --- freshness / target plumbing shared with the engines ---

    fn doc_ord_of(&self, snap: &Snapshot, uri: &str) -> Option<u32> {
        let seg = snap.segment();
        (0..seg.doc_count()).find(|&d| seg.uri_of(d) == uri)
    }

    /// The first target in workspace order whose SemanticDB output contains
    /// `uri` (the postings primary).
    pub fn primary_spec_of(&self, uri: &str) -> Option<TargetSpec> {
        let ws = self.workspace()?;
        for spec in &ws.targets {
            let locator = SemanticdbLocator::new(spec.semanticdb_root.clone());
            if let Ok(file) = locator.semanticdb_file_for(uri) {
                if file.is_file() {
                    return Some(spec.clone());
                }
            }
        }
        None
    }

    pub fn primary_bsp_of(&self, uri: &str) -> Option<String> {
        self.primary_spec_of(uri).map(|s| s.bsp_id)
    }

    /// The first target in workspace order whose sourceroot actually contains
    /// `uri` as a regular file — workspace membership regardless of whether the
    /// target produced any `.semanticdb` output. Distinguishes a
    /// no-SemanticDB workspace source from a truly outside-workspace uri.
    pub fn source_target_of(&self, uri: &str) -> Option<TargetSpec> {
        let ws = self.workspace()?;
        ws.targets
            .iter()
            .find(|spec| spec.sourceroot.join(uri).is_file())
            .cloned()
    }

    /// Every target's `.semanticdb` file that contains `uri` (workspace order),
    /// for shared-source consistency checks.
    pub fn semanticdb_files_for(&self, uri: &str) -> Vec<PathBuf> {
        let Some(ws) = self.workspace() else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for spec in &ws.targets {
            let locator = SemanticdbLocator::new(spec.semanticdb_root.clone());
            if let Ok(file) = locator.semanticdb_file_for(uri) {
                if file.is_file() {
                    out.push(file);
                }
            }
        }
        out
    }

    pub fn absolute_source_path(&self, uri: &str) -> Option<PathBuf> {
        self.primary_spec_of(uri).map(|s| s.sourceroot.join(uri))
    }

    fn source_text_of(&self, uri: &str) -> Option<String> {
        let path = self.absolute_source_path(uri)?;
        if path.is_file() {
            std::fs::read_to_string(&path).ok()
        } else {
            None
        }
    }

    fn ingested_md5(snap: &Snapshot, uri: &str) -> Option<String> {
        IngestState::decode(&snap.state().payload)
            .get(uri)
            .map(|d| d.md5.clone())
    }

    fn source_is_fresh(&self, snap: &Snapshot, uri: &str) -> bool {
        let Some(md5v) = Self::ingested_md5(snap, uri) else {
            return false;
        };
        match self.source_text_of(uri) {
            Some(text) => md5::validate(&text, &md5v).is_fresh(),
            None => false,
        }
    }

    /// True when the on-disk source still matches the md5 recorded at ingest for
    /// `uri` in the current snapshot.
    pub fn source_is_fresh_uri(&self, uri: &str) -> bool {
        match self.store.current() {
            Some(snap) => self.source_is_fresh(&snap, uri),
            None => false,
        }
    }

    // --- target graph pruning ---

    /// Exact allowed-target set for references of `sym_ord`: the reverse
    /// dependency closure of its definition target, mapped to snapshot ordinals.
    /// Falls back to all targets when the definition target is unknown.
    pub fn allowed_targets_for(&self, snap: &Snapshot, sym_ord: u32) -> TargetBitset {
        let seg = snap.segment();
        let target_count = seg.target_count() as u32;
        let all = TargetBitset::all(target_count);
        let def_ord = seg.symbol_view(sym_ord).def_target_ord;
        if def_ord < 0 {
            return all;
        }
        let def_ord = def_ord as u32;
        let def_bsp = seg.target_meta(def_ord).bsp_id;
        let Some(ws) = self.workspace() else {
            return all;
        };
        let closure = ws.reverse_dependency_closure(&def_bsp);
        if closure.is_empty() {
            return all;
        }
        let mut bsp_to_ord: HashMap<String, u32> = HashMap::new();
        for ord in 0..target_count {
            bsp_to_ord.insert(seg.target_meta(ord).bsp_id, ord);
        }
        let mut ords: Vec<u32> = closure
            .iter()
            .filter_map(|b| bsp_to_ord.get(b).copied())
            .collect();
        ords.push(def_ord);
        ords.sort_unstable();
        ords.dedup();
        TargetBitset::of(target_count, ords)
    }

    /// The bsp id of the target defining `sym_ord` in `snap`.
    pub fn definition_bsp_of(&self, snap: &Snapshot, sym_ord: u32) -> Option<String> {
        let seg = snap.segment();
        let def_ord = seg.symbol_view(sym_ord).def_target_ord;
        if def_ord < 0 {
            None
        } else {
            Some(seg.target_meta(def_ord as u32).bsp_id)
        }
    }
}

/// Exact occurrence covering the position: smallest packed span wins, then
/// earliest start — the same rule as the segment reader.
fn occurrence_at(occurrences: &[Occurrence], line: u32, character: u32) -> Option<&Occurrence> {
    let q = Span::pack(line, character);
    let mut best: Option<&Occurrence> = None;
    let mut best_size = u32::MAX;
    let mut best_start = u32::MAX;
    for o in occurrences {
        let ps = Span::pack(o.span.start_line, o.span.start_char);
        let pe = Span::pack(o.span.end_line, o.span.end_char);
        if ps <= q && q <= pe {
            let size = pe - ps;
            if size < best_size || (size == best_size && ps < best_start) {
                best = Some(o);
                best_size = size;
                best_start = ps;
            }
        }
    }
    best
}

fn thread_label() -> String {
    std::thread::current()
        .name()
        .map(str::to_string)
        .unwrap_or_else(|| format!("{:?}", std::thread::current().id()))
}

/// Public helper mirroring [`thread_label`] so callers can assert the
/// write-through ran on their own thread.
pub fn current_thread_label() -> String {
    thread_label()
}
