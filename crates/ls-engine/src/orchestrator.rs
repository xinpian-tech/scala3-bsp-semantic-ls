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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ls_index_model::uri;
use ls_index_model::{
    Loc, LsError, NormalizedDocument, Occurrence, Role, Span, SymKind, TargetBitset,
};
use ls_semanticdb::{md5, normalize, parse_file, SemanticdbLocator};
use ls_store::{GroupRecord, SearchIndex, Snapshot, Store, StoreResult, WorkspaceSymbolHit};

use crate::hash::doc_id_for;
use crate::ingest::{self, IngestReport};
use crate::overlay::{DirtyBufferOverlay, NoopOverlay};
use crate::state::IngestState;
use crate::symbol_encoding;
use crate::targets::{TargetSpec, WorkspaceTargets};

/// A workspace-symbol search hit resolved to what a `WorkspaceSymbol` needs: the
/// display name, kind, optional container (owner, else package), and defining
/// `file://` location. Mirrors what `ScalaLs.workspaceSymbolOf` builds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkspaceSymbolEntry {
    pub display: String,
    pub kind: SymKind,
    pub container: Option<String>,
    pub location: Loc,
}

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

    /// Workspace-symbol search resolved to [`WorkspaceSymbolEntry`]s: the index
    /// hits with their defining location built from the segment (def span from
    /// `symbol_meta`, doc URI from `uri_of`, sourceroot from `target_meta`), as
    /// `ScalaLs.workspaceSymbolOf` does. A hit whose defining doc/target/symbol
    /// is unknown or out of range is dropped, matching the Scala
    /// for-comprehension that yields `None`. Search + resolution run against one
    /// snapshot so the hit ordinals stay valid.
    pub fn workspace_symbols(&self, query: &str, limit: usize) -> Vec<WorkspaceSymbolEntry> {
        let Some(snap) = self.store.current() else {
            return Vec::new();
        };
        let seg = snap.segment();
        let doc_count = seg.doc_count();
        let target_count = seg.target_count();
        let symbol_count = seg.symbol_count();
        SearchIndex::build(seg)
            .workspace_symbol_search(query, limit)
            .iter()
            .filter_map(|hit| {
                if hit.def_doc_ord < 0 || hit.def_target_ord < 0 {
                    return None;
                }
                let doc_ord = hit.def_doc_ord as u32;
                let target_ord = hit.def_target_ord as u32;
                if doc_ord >= doc_count
                    || target_ord as usize >= target_count
                    || hit.symbol_ord as usize >= symbol_count
                {
                    return None;
                }
                let meta = seg.symbol_meta(hit.symbol_ord);
                let span = Span::new(
                    Span::unpack_line(meta.def_packed_start as u32),
                    Span::unpack_char(meta.def_packed_start as u32),
                    Span::unpack_line(meta.def_packed_end as u32),
                    Span::unpack_char(meta.def_packed_end as u32),
                );
                let sourceroot = seg.target_meta(target_ord).sourceroot;
                let abs = uri::normalize(&Path::new(&sourceroot).join(seg.uri_of(doc_ord)));
                let container = if !hit.owner.is_empty() {
                    Some(hit.owner.clone())
                } else if !hit.package_name.is_empty() {
                    Some(hit.package_name.clone())
                } else {
                    None
                };
                Some(WorkspaceSymbolEntry {
                    display: hit.display.clone(),
                    kind: SymKind::from_code(hit.kind),
                    container,
                    location: Loc::new(uri::path_to_uri(&abs), span),
                })
            })
            .collect()
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

    /// Index-backed cross-file go-to-definition for the presentation compiler
    /// (the callback behind `SymbolSearch.definition`): a SemanticDB symbol to
    /// its workspace definition `file://` locations.
    ///
    /// The same SemanticDB string can be DEFINED in more than one disconnected
    /// target (two modules reusing a package/class name). A buffer in target T
    /// must only reach a definition T can SEE — a target in T's forward
    /// dependency closure — otherwise editors jump to an unrelated duplicate.
    /// `from_uri` (a `file://` uri) locates the requesting target by its deepest
    /// containing sourceroot; when no target owns it, results are unscoped.
    ///
    /// Never panics; unknown/local symbols answer empty. Reads only the immutable
    /// snapshot and the workspace graph, so it is safe on PC callback threads.
    pub fn symbol_definition(&self, semantic_symbol: &str, from_uri: &str) -> Vec<Loc> {
        if semantic_symbol.is_empty() {
            return Vec::new();
        }
        let Some(snap) = self.current_snapshot() else {
            return Vec::new();
        };
        let seg = snap.segment();
        let Some(ord) = seg.find_symbol_ord(&symbol_encoding::encode(semantic_symbol, None)) else {
            return Vec::new();
        };
        let ref_group = seg.symbol_view(ord).ref_group_ord;
        if ref_group < 0 {
            return Vec::new();
        }
        // The bsp ids the requesting buffer's target can see, or None (unscoped).
        let allowed = self.requesting_forward_closure(from_uri);
        let mut out: Vec<Loc> = Vec::new();
        seg.scan_def_group(ref_group as u32, &mut |rec: GroupRecord| {
            if rec.target_ord < 0 {
                return;
            }
            let ps = rec.packed_start as u32;
            let pe = rec.packed_end as u32;
            let sl = Span::unpack_line(ps);
            let sc = Span::unpack_char(ps);
            let doc_ord = rec.doc_ord as u32;
            // Keep only occurrences that define EXACTLY `ord`, not the other
            // members of its ref group (a class + its companion object, a
            // getter/setter pair, ...).
            if seg.symbol_at(doc_ord, sl, sc).map(|h| h.symbol_ord) != Some(ord as i32) {
                return;
            }
            let meta = seg.target_meta(rec.target_ord as u32);
            let visible = allowed
                .as_ref()
                .map(|ids| ids.contains(&meta.bsp_id))
                .unwrap_or(true);
            if !visible {
                return;
            }
            let span = Span::new(sl, sc, Span::unpack_line(pe), Span::unpack_char(pe));
            // Absolute source path = sourceroot / sdb-uri, emitted as a
            // percent-encoded `file://` uri (mirrors `Uris.fromSdbUri` + `toUri`).
            let abs = uri::normalize(&Path::new(&meta.sourceroot).join(seg.uri_of(doc_ord)));
            out.push(Loc::new(uri::path_to_uri(&abs), span));
        });
        dedupe_and_sort_locs(out)
    }

    /// Forward dependency closure (bsp ids) of the target owning `from_uri`,
    /// found by its deepest containing sourceroot, or `None` when no target owns
    /// the buffer (definitions are then unscoped). Reads only the immutable
    /// workspace graph.
    ///
    /// `from_uri` is a `file://` uri: it is percent-decoded and lexically
    /// normalized, and each candidate sourceroot is normalized too, so an
    /// encoded path (a sourceroot with a space) still matches its owning target
    /// rather than falling through to an unscoped (leaky) lookup.
    fn requesting_forward_closure(&self, from_uri: &str) -> Option<HashSet<String>> {
        let ws = self.workspace()?;
        let path = uri::normalize(&uri::uri_to_path(from_uri).ok()?);
        let spec = ws
            .targets
            .iter()
            .filter_map(|t| {
                let root = uri::normalize(&t.sourceroot);
                path.starts_with(&root)
                    .then_some((t, root.components().count()))
            })
            .max_by_key(|(_, depth)| *depth)
            .map(|(t, _)| t)?;
        Some(ws.forward_dependency_closure(&spec.bsp_id))
    }
}

/// Dedupe by (uri, span) and sort by (uri, start, end) — a stable order for
/// editors and tests.
fn dedupe_and_sort_locs(mut locs: Vec<Loc>) -> Vec<Loc> {
    locs.sort_by(|a, b| {
        a.uri
            .cmp(&b.uri)
            .then(a.span.start_line.cmp(&b.span.start_line))
            .then(a.span.start_char.cmp(&b.span.start_char))
            .then(a.span.end_line.cmp(&b.span.end_line))
            .then(a.span.end_char.cmp(&b.span.end_char))
    });
    locs.dedup_by(|a, b| a.uri == b.uri && a.span == b.span);
    locs
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
