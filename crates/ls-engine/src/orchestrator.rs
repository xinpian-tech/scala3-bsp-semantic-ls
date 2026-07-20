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
    occ_flags, Loc, LsError, NormalizedDocument, Occurrence, Role, Span, SymKind, TargetBitset,
};
use ls_semanticdb::symbols::Descriptor;
use ls_semanticdb::{md5, normalize, parse_file, SdbDocument, SemanticdbLocator};
use ls_store::{
    GroupRecord, SearchIndex, SegmentReader, Snapshot, Store, StoreResult, WorkspaceSymbolHit,
};

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

/// A workspace method-search hit resolved to what the PC `SymbolSearch.
/// searchMethods` visitor needs: the defining absolute `file://` uri, the raw
/// SemanticDB symbol string (the PC resolves it back to a compiler symbol), the
/// SemanticDB kind code, and the definition span. The engine-side carrier
/// behind the island's `search_methods` callback.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodHit {
    pub uri: String,
    pub symbol: String,
    pub kind: i32,
    pub span: Span,
}

/// One `textDocument/documentSymbol` outline node: the display name, the index
/// [`SymKind`], the NAME span, and the children nested by SemanticDB owner
/// chain.
///
/// The index stores only definition NAME spans (`docs/index-format.md` — no
/// full declaration extents), so the node carries a single span: the LSP
/// mapping sets `range == selectionRange`. This is the documented outline
/// limitation: spec-legal (`selectionRange` must be CONTAINED in `range`, and
/// equality is containment), and outline/breadcrumb clients tolerate equal
/// ranges — enclosure detection merely degrades to the name line. A synthetic
/// extent (name span to the next same-or-shallower sibling) was considered and
/// rejected as LESS honest: it would claim source extents the index does not
/// know, misattributing trailing non-definition code (comments, expressions
/// after the last member) to the preceding symbol in breadcrumbs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocSymbolEntry {
    pub name: String,
    pub kind: SymKind,
    /// The definition NAME span — both the LSP `range` and `selectionRange`.
    pub span: Span,
    pub children: Vec<DocSymbolEntry>,
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
    /// Serializes full-generation ingests so a background reingest (the build-job
    /// scheduler) never runs concurrently with a message-loop ingest (the explicit
    /// reindex command, the build-target reload, or bootstrap). Enforces the
    /// single-writer store contract regardless of the calling thread — the port's
    /// stand-in for the Scala single `indexExecutor` all ingests run on.
    ingest_lock: Mutex<()>,
}

impl QueryOrchestrator {
    pub fn new(store: Store, overlay: BoxOverlay, sync_write_through: bool) -> Self {
        QueryOrchestrator {
            store,
            overlay,
            sync_write_through,
            current_workspace: Mutex::new(None),
            last_write_through_thread: Mutex::new(None),
            ingest_lock: Mutex::new(()),
        }
    }

    /// The production orchestrator: `sync_write_through = true`, so a
    /// RawSemanticDBPath resolution heals SYNCHRONOUSLY inline on the calling
    /// thread — running the full-generation ingest and clearing `needs_reindex`
    /// before returning (the write-through parity contract; the Scala
    /// `WorkspaceState` default). The `sync_write_through = false` mode (raw path
    /// only serves and flags `needs_reindex`, healed later by a scheduled
    /// reingest) is available via [`QueryOrchestrator::new`] and exercised by the
    /// engine's async-mode tests; it is NOT the production wiring.
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

    /// `true` iff the current snapshot holds an active document for `sdb_uri`.
    /// The mmap store keeps only active (non-superseded) documents in the live
    /// segment, so any uri the current snapshot resolves is, by construction,
    /// active. Ports `MetaStore.hasActiveDocument`; read-only against the
    /// immutable snapshot `Arc` (no writer touched), matching the Scala
    /// reader-pool read that the gate performs off the writer connection.
    pub fn has_active_document(&self, sdb_uri: &str) -> bool {
        self.current_snapshot()
            .is_some_and(|snap| self.doc_ord_of(&snap, sdb_uri).is_some())
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

    /// Whether a RawSemanticDBPath resolution heals SYNCHRONOUSLY inline —
    /// running the full-generation ingest and clearing `needs_reindex` before
    /// returning (the write-through parity contract, the production wiring). When
    /// `false`, the raw path only serves and flags `needs_reindex` for a later
    /// scheduled reingest. Lets a production-path test assert the mode is wired.
    pub fn raw_path_writes_through(&self) -> bool {
        self.sync_write_through
    }

    /// Runs a full-generation ingest and remembers the workspace description for
    /// target-graph pruning. Serialized by `ingest_lock` so concurrent ingests
    /// (background reingest vs. a message-loop ingest) never race the store's
    /// generation commit — the single-writer contract, enforced across threads.
    pub fn ingest(&self, workspace: Arc<WorkspaceTargets>) -> StoreResult<IngestReport> {
        let _writer = self.ingest_lock.lock().unwrap();
        let (report, _snap) = ingest::ingest(&self.store, &workspace)?;
        *self.current_workspace.lock().unwrap() = Some(workspace);
        Ok(report)
    }

    /// Re-ingests the CURRENT workspace, reading it INSIDE the ingest lock so a
    /// background heal always targets the latest committed model. This is what the
    /// build-job scheduler calls: capturing the workspace under the lock (not
    /// before it) guarantees a background reingest can never REVERT a concurrent
    /// `reload` that swapped `current_workspace` — the reload's newer ingest is
    /// never clobbered by a stale pre-captured workspace. Returns `None` when no
    /// workspace is set or it has no indexable target, so the heal never commits an
    /// empty segment over a live index (matches `reload`'s non-empty gate).
    pub fn reingest_current(&self) -> Option<StoreResult<IngestReport>> {
        let _writer = self.ingest_lock.lock().unwrap();
        let workspace = self.current_workspace.lock().unwrap().clone()?;
        if workspace.targets.is_empty() {
            return None;
        }
        Some(ingest::ingest(&self.store, &workspace).map(|(report, _snap)| report))
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
        let _writer = self.ingest_lock.lock().unwrap();
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

    /// Index-backed workspace method search for the presentation compiler (the
    /// callback behind `SymbolSearch.searchMethods`, member-mode workspace
    /// extension-method / implicit-class-member discovery): every snapshot
    /// symbol whose SemanticDB descriptor is a METHOD and whose display name
    /// matches `query` (the metals `Fuzzy.matches` name semantics: an empty
    /// query matches all, else a case-sensitive prefix / camel-hump
    /// subsequence), resolved to its DEFINITION occurrences exactly like
    /// [`QueryOrchestrator::symbol_definition`]'s def-group scan.
    ///
    /// Visibility: the PC hands its own build-target id, so hits are pruned to
    /// the FORWARD dependency closure of `bsp_target_id` (what that target can
    /// SEE); an empty or unknown target id answers unscoped. Recall matters
    /// more than precision here — the compiler re-filters every candidate for
    /// receiver-type applicability — so the matcher is conservative but the
    /// candidate set is never the whole index for a non-empty query.
    ///
    /// Never panics; reads only the immutable snapshot and the workspace
    /// graph, so it is safe on PC callback threads.
    pub fn search_methods(&self, query: &str, bsp_target_id: &str) -> Vec<MethodHit> {
        let Some(snap) = self.current_snapshot() else {
            return Vec::new();
        };
        let seg = snap.segment();
        // The bsp ids the requesting target can see, or None (unscoped): the
        // forward closure is empty exactly when the id is unknown to the graph.
        let allowed: Option<HashSet<String>> = if bsp_target_id.is_empty() {
            None
        } else {
            self.workspace()
                .map(|ws| ws.forward_dependency_closure(bsp_target_id))
                .filter(|closure| !closure.is_empty())
        };
        let mut out: Vec<MethodHit> = Vec::new();
        for ord in 0..seg.symbol_count() as u32 {
            let (raw, local_doc) = symbol_encoding::decode(seg.semantic_symbol_of(ord));
            // Only global method-descriptor symbols are workspace-discoverable.
            if local_doc.is_some() {
                continue;
            }
            let display = match ls_semanticdb::symbols::descriptor_of(&raw) {
                Some(ls_semanticdb::symbols::Descriptor::Method(name, _)) => name,
                _ => continue,
            };
            if !fuzzy_matches_name(query, &display) {
                continue;
            }
            let ref_group = seg.symbol_view(ord).ref_group_ord;
            if ref_group < 0 {
                continue;
            }
            let meta_kind = seg.symbol_meta(ord).kind;
            // SemanticDB kind from the symbol meta when stored; the grammar
            // already proved a method descriptor, so default to METHOD (3).
            let kind = if meta_kind != 0 { meta_kind } else { 3 };
            seg.scan_def_group(ref_group as u32, &mut |rec: GroupRecord| {
                if rec.target_ord < 0 {
                    return;
                }
                let ps = rec.packed_start as u32;
                let pe = rec.packed_end as u32;
                let sl = Span::unpack_line(ps);
                let sc = Span::unpack_char(ps);
                let doc_ord = rec.doc_ord as u32;
                // Keep only occurrences that define EXACTLY `ord`, not the
                // other members of its ref group (the same exactness filter as
                // `symbol_definition`).
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
                // percent-encoded `file://` uri (as `symbol_definition` does).
                let abs = uri::normalize(&Path::new(&meta.sourceroot).join(seg.uri_of(doc_ord)));
                out.push(MethodHit {
                    uri: uri::path_to_uri(&abs),
                    symbol: raw.clone(),
                    kind,
                    span,
                });
            });
        }
        dedupe_and_sort_method_hits(out)
    }

    /// Index-backed definition-source toplevels for the presentation compiler
    /// (the callback behind `SymbolSearch.definitionSourceToplevels`, exhaustive-
    /// match case ordering): the toplevel SemanticDB symbols of the source that
    /// DEFINES `semantic_symbol`, in source order.
    ///
    /// The parent symbol is resolved byte-for-byte with
    /// [`QueryOrchestrator::symbol_definition`]'s discipline: `find_symbol_ord`,
    /// then the ref-group def scan with the `symbol_at` exactness filter,
    /// pruned to the requesting buffer's forward closure (`source_uri` locates
    /// the requesting target; a disconnected duplicate definition never
    /// leaks). Among the visible defining occurrences the FIRST defining doc
    /// wins (lowest `target_ord`, `doc_ord` tie-break); that doc is enumerated
    /// in source order (`SegmentReader::scan_doc`), keeping DEFINITION
    /// occurrences whose decoded symbol is global and its own enclosing
    /// toplevel (`enclosing_top_level(sym) == Some(sym)`), deduped first-seen.
    ///
    /// Never panics; unknown/empty symbols answer empty. Reads only the
    /// immutable snapshot and the workspace graph, so it is safe on PC callback
    /// threads.
    pub fn definition_source_toplevels(
        &self,
        semantic_symbol: &str,
        source_uri: &str,
    ) -> Vec<String> {
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
        let allowed = self.requesting_forward_closure(source_uri);
        // First visible defining doc: lowest target_ord, doc_ord tie-break.
        let mut best: Option<(u32, u32)> = None;
        seg.scan_def_group(ref_group as u32, &mut |rec: GroupRecord| {
            if rec.target_ord < 0 {
                return;
            }
            let ps = rec.packed_start as u32;
            let sl = Span::unpack_line(ps);
            let sc = Span::unpack_char(ps);
            let doc_ord = rec.doc_ord as u32;
            // Keep only occurrences that define EXACTLY `ord`, not the other
            // members of its ref group (the `symbol_definition` exactness
            // filter).
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
            let candidate = (rec.target_ord as u32, doc_ord);
            if best.is_none_or(|b| candidate < b) {
                best = Some(candidate);
            }
        });
        let Some((_, doc_ord)) = best else {
            return Vec::new();
        };
        // Enumerate the defining doc in source order; keep global toplevel
        // DEFINITION occurrences, first-seen order.
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<String> = Vec::new();
        seg.scan_doc(doc_ord, false, &mut |rec| {
            if !occ_flags::has(rec.flags as u32, occ_flags::DEFINITION) {
                return;
            }
            let (raw, local_doc) =
                symbol_encoding::decode(seg.semantic_symbol_of(rec.symbol_ord as u32));
            // Locals never surface (they are doc-scoped, not toplevels).
            if local_doc.is_some() {
                return;
            }
            // Toplevels only: the symbol must BE its own enclosing toplevel.
            if ls_semanticdb::symbols::enclosing_top_level(&raw).as_deref() != Some(raw.as_str()) {
                return;
            }
            if seen.insert(raw.clone()) {
                out.push(raw);
            }
        });
        out
    }

    /// Index-backed `textDocument/documentSymbol`: the nested outline of an
    /// INDEXED document, from the doc's postings in source order
    /// (`SegmentReader::scan_doc` — the `definition_source_toplevels`
    /// enumeration discipline).
    ///
    /// Node selection: DEFINITION occurrences whose decoded symbol is global
    /// and whose last descriptor is a Term, Type, or Method — parameters, type
    /// parameters and packages never surface, and neither do constructors
    /// (`<init>` duplicates its class node at the same name span) nor setters
    /// (`x_=` duplicates the `var x` node — the alias-group setter/getter
    /// merge, applied to the outline). The first definition occurrence of a
    /// symbol wins; display name and kind come from `symbol_meta` (the
    /// `WorkspaceSymbolEntry` source), falling back to the descriptor name for
    /// a symbol the batch carried no info for.
    ///
    /// Nesting: by SemanticDB OWNER CHAIN (`ls_semanticdb::symbols`), a child
    /// attaching under its NEAREST enclosing symbol that has a node in the
    /// SAME document — or, when an owner has no node of its own, under that
    /// owner's COMPANION node (an enum's cases are owned by the synthetic
    /// companion object `Color.`, which has no definition occurrence; they
    /// belong under the `enum Color` class node `Color#`). With no enclosing
    /// node the symbol is a toplevel. Source order is preserved at every
    /// level.
    ///
    /// Index-truth-only, by decision: a DIRTY buffer still answers from the
    /// index — the outline lags the buffer until save (never an error, never a
    /// PC parse). A uri the current snapshot does not hold (or no snapshot at
    /// all) answers the empty outline.
    pub fn document_symbols(&self, uri: &str) -> Vec<DocSymbolEntry> {
        let Some(snap) = self.current_snapshot() else {
            return Vec::new();
        };
        let Some(doc_ord) = self.doc_ord_of(&snap, uri) else {
            return Vec::new();
        };
        let seg = snap.segment();
        // Arena of nodes in first-seen (source) order; the tree is index-linked
        // so children can be attached while later records are still streaming.
        struct Node {
            entry: DocSymbolEntry,
            children: Vec<usize>,
        }
        let mut nodes: Vec<Node> = Vec::new();
        let mut node_of: HashMap<String, usize> = HashMap::new();
        let mut roots: Vec<usize> = Vec::new();
        seg.scan_doc(doc_ord, false, &mut |rec| {
            if !occ_flags::has(rec.flags as u32, occ_flags::DEFINITION) {
                return;
            }
            let sym_ord = rec.symbol_ord as u32;
            let (raw, local_doc) = symbol_encoding::decode(seg.semantic_symbol_of(sym_ord));
            // Locals never surface in the outline (document-scoped bodies).
            if local_doc.is_some() {
                return;
            }
            let descriptor = match ls_semanticdb::symbols::descriptor_of(&raw) {
                Some(d @ (Descriptor::Term(_) | Descriptor::Type(_) | Descriptor::Method(..))) => d,
                _ => return,
            };
            if let Descriptor::Method(name, _) = &descriptor {
                if name == ls_semanticdb::symbols::CONSTRUCTOR_NAME
                    || ls_semanticdb::symbols::is_setter(&raw)
                {
                    return;
                }
            }
            if node_of.contains_key(&raw) {
                return; // first definition occurrence wins
            }
            let meta = seg.symbol_meta(sym_ord);
            let name = if meta.display.is_empty() {
                descriptor.name().to_string()
            } else {
                meta.display
            };
            let ps = rec.packed_start as u32;
            let pe = rec.packed_end as u32;
            let entry = DocSymbolEntry {
                name,
                kind: SymKind::from_code(meta.kind),
                span: Span::new(
                    Span::unpack_line(ps),
                    Span::unpack_char(ps),
                    Span::unpack_line(pe),
                    Span::unpack_char(pe),
                ),
                children: Vec::new(),
            };
            // Nearest enclosing node: walk the proper ancestors innermost-first,
            // trying each ancestor and then its companion.
            let chain = ls_semanticdb::symbols::owner_chain(&raw);
            let parent = chain[..chain.len().saturating_sub(1)]
                .iter()
                .rev()
                .find_map(|ancestor| {
                    node_of.get(ancestor).copied().or_else(|| {
                        ls_semanticdb::symbols::companion(ancestor)
                            .and_then(|c| node_of.get(&c).copied())
                    })
                });
            let idx = nodes.len();
            nodes.push(Node {
                entry,
                children: Vec::new(),
            });
            node_of.insert(raw, idx);
            match parent {
                Some(p) => nodes[p].children.push(idx),
                None => roots.push(idx),
            }
        });
        // Materialize bottom-up: children were always pushed AFTER their parent
        // (source order — an owner's name precedes its members'), so a reverse
        // walk completes every child before its parent takes it.
        for i in (0..nodes.len()).rev() {
            let children = std::mem::take(&mut nodes[i].children);
            for c in children {
                let child = std::mem::replace(
                    &mut nodes[c].entry,
                    DocSymbolEntry {
                        name: String::new(),
                        kind: SymKind::UnknownKind,
                        span: Span::new(0, 0, 0, 0),
                        children: Vec::new(),
                    },
                );
                nodes[i].entry.children.push(child);
            }
        }
        roots
            .into_iter()
            .map(|r| {
                std::mem::replace(
                    &mut nodes[r].entry,
                    DocSymbolEntry {
                        name: String::new(),
                        kind: SymKind::UnknownKind,
                        span: Span::new(0, 0, 0, 0),
                        children: Vec::new(),
                    },
                )
            })
            .collect()
    }

    /// Index-backed `textDocument/implementation`: the DEFINITION locations of
    /// the members of the cursor symbol's METHOD OVERRIDE FAMILY that override
    /// it — an abstract (or overridable) method's implementations are its
    /// overriders' def sites. Locations are SemanticDB-relative (`Loc.uri` is
    /// the sdb uri, as `references` emits), deduped and sorted.
    ///
    /// What the store supports (the honest scope): the alias groups do NOT
    /// union override families — the `overridden_symbols` edges are consumed
    /// at group build only to set the per-rename-group `has_override_family`
    /// FLAG (`ls_semanticdb::groups`), and no type-hierarchy/sealed-subtype
    /// edge exists anywhere in the index (dotty's SemanticDB carries
    /// `overridden_symbols` for METHODS only; type symbols carry none). So:
    ///
    /// - METHOD cursor: candidates are the segment's global method symbols
    ///   sharing the cursor's method NAME whose rename group is
    ///   override-flagged (the index pre-filter — an unflagged group can
    ///   belong to no family); each candidate is then VERIFIED against the
    ///   `overridden_symbols` edges read from its defining document's raw
    ///   `.semanticdb` (the RawSemanticDBPath discipline — the same files the
    ///   ingest read; dotty lists the full transitive override chain, so a
    ///   deep override still names the queried base directly). Verified
    ///   overriders answer their def sites via the `symbol_definition`
    ///   def-group scan with the `symbol_at` exactness filter, pruned to the
    ///   requesting buffer's forward closure (`requesting_forward_closure` —
    ///   a disconnected duplicate never leaks; downstream-only implementors
    ///   are invisible from an upstream buffer, the shared visibility rule).
    /// - TYPE (trait/class) cursor: the honest EMPTY — subtype edges are not
    ///   modeled, and inferring implementors from member overrides would miss
    ///   every subtype that overrides nothing.
    /// - Locals, terms, constructors: empty (nothing overrides them).
    ///
    /// Cursor-resolution failures are typed errors exactly like `references`
    /// (`NoSymbolAtCursor`, `StaleIndex`, `PcOnlySymbol`); a resolved cursor
    /// never errors — an unknown family answers `[]`. A best-effort raw read
    /// that fails (a vanished or re-compiled `.semanticdb`) skips that
    /// candidate rather than failing the query.
    pub fn implementations(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Vec<Loc>, LsError> {
        let cursor = self.symbol_at_cursor(uri, line, character)?;
        if cursor.pc_only {
            return Err(LsError::PcOnlySymbol);
        }
        if cursor.is_local() {
            return Ok(Vec::new());
        }
        let Some(snap) = self.current_snapshot() else {
            return Err(LsError::NotIndexed {
                uri: uri.to_string(),
            });
        };
        let seg = snap.segment();
        // Method override families only (see above): a non-method cursor —
        // types included, subtype edges being absent — answers the honest [].
        let method_name = match ls_semanticdb::symbols::descriptor_of(&cursor.semantic_symbol) {
            Some(Descriptor::Method(name, _))
                if name != ls_semanticdb::symbols::CONSTRUCTOR_NAME =>
            {
                name
            }
            _ => return Ok(Vec::new()),
        };
        let Some(ord) =
            seg.find_symbol_ord(&symbol_encoding::encode(&cursor.semantic_symbol, None))
        else {
            // A fresh symbol the snapshot has not seen (RawSemanticDBPath): the
            // index knows no family yet; production write-through heals inline.
            return Ok(Vec::new());
        };
        // The index pre-filter: a cursor whose rename group is not
        // override-flagged participates in NO override family — answer []
        // without touching a single `.semanticdb`.
        if !Self::group_has_override_family(seg, ord) {
            return Ok(Vec::new());
        }
        // The bsp ids the requesting buffer's target can see, or None (unscoped).
        let allowed = self
            .absolute_source_path(uri)
            .map(|p| uri::path_to_uri(&uri::normalize(&p)))
            .and_then(|file_uri| self.requesting_forward_closure(&file_uri));
        // Same-name override-flagged method candidates, grouped by defining doc
        // so each candidate doc's raw `.semanticdb` is read once.
        let mut by_doc: HashMap<u32, Vec<(u32, String)>> = HashMap::new();
        for cand in 0..seg.symbol_count() as u32 {
            if cand == ord {
                continue;
            }
            let (raw, local_doc) = symbol_encoding::decode(seg.semantic_symbol_of(cand));
            if local_doc.is_some() {
                continue;
            }
            match ls_semanticdb::symbols::descriptor_of(&raw) {
                Some(Descriptor::Method(name, _)) if name == method_name => {}
                _ => continue,
            }
            if !Self::group_has_override_family(seg, cand) {
                continue;
            }
            let def_doc = seg.symbol_meta(cand).def_doc_ord;
            if def_doc < 0 {
                continue;
            }
            by_doc.entry(def_doc as u32).or_default().push((cand, raw));
        }
        let mut out: Vec<Loc> = Vec::new();
        for (doc_ord, candidates) in by_doc {
            let Some(sdb) = self.raw_sdb_document_of(seg, doc_ord) else {
                continue; // best-effort: an unreadable doc skips its candidates
            };
            for (cand, raw) in candidates {
                let overrides_cursor =
                    sdb.symbols
                        .iter()
                        .find(|s| s.symbol == raw)
                        .is_some_and(|s| {
                            s.overridden_symbols
                                .iter()
                                .any(|o| o == &cursor.semantic_symbol)
                        });
                if !overrides_cursor {
                    continue;
                }
                let ref_group = seg.symbol_view(cand).ref_group_ord;
                if ref_group < 0 {
                    continue;
                }
                seg.scan_def_group(ref_group as u32, &mut |rec: GroupRecord| {
                    if rec.target_ord < 0 {
                        return;
                    }
                    let ps = rec.packed_start as u32;
                    let pe = rec.packed_end as u32;
                    let sl = Span::unpack_line(ps);
                    let sc = Span::unpack_char(ps);
                    let rec_doc = rec.doc_ord as u32;
                    // Keep only occurrences that define EXACTLY the candidate,
                    // not the other members of its ref group (the
                    // `symbol_definition` exactness filter).
                    if seg.symbol_at(rec_doc, sl, sc).map(|h| h.symbol_ord) != Some(cand as i32) {
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
                    out.push(Loc::new(seg.uri_of(rec_doc).to_string(), span));
                });
            }
        }
        Ok(dedupe_and_sort_locs(out))
    }

    /// Whether `sym_ord`'s rename group carries the `has_override_family`
    /// flag — the store's only override-family fact (the edges themselves are
    /// not persisted; see [`QueryOrchestrator::implementations`]).
    fn group_has_override_family(seg: &SegmentReader, sym_ord: u32) -> bool {
        let rename_group = seg.symbol_view(sym_ord).rename_group_ord;
        rename_group >= 0 && seg.rename_profile(rename_group as u32).has_override_family
    }

    /// The raw `.semanticdb` TextDocument of an indexed doc, located through
    /// the doc's OWN target (its `target_ord`, not the workspace-order
    /// primary), best-effort: `None` when the file is gone or unreadable. No
    /// md5 gate — the caller reads advisory `overridden_symbols` edges for
    /// candidates the index already names, so a stale file merely fails the
    /// edge check for the affected candidates.
    fn raw_sdb_document_of(&self, seg: &SegmentReader, doc_ord: u32) -> Option<SdbDocument> {
        let uri = seg.uri_of(doc_ord).to_string();
        let target_ord = seg.target_ord_of_doc(doc_ord);
        if target_ord < 0 {
            return None;
        }
        let root = PathBuf::from(seg.target_meta(target_ord as u32).semanticdb_root);
        let locator = SemanticdbLocator::new(root);
        let file = locator.semanticdb_file_for(&uri).ok()?;
        let docs = parse_file(&file).ok()?;
        docs.documents.into_iter().find(|d| d.uri == uri)
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
        // Exact attribution first: the ingested doc row records its owning
        // target, which is decisive when several targets share one sourceroot —
        // the mill layout, where EVERY target's `-sourceroot` is the workspace
        // root, so prefix depth ties across all targets and any tie-pick would
        // prune valid definitions through an unrelated target's closure.
        if let Some(snap) = self.current_snapshot() {
            let seg = snap.segment();
            let mut roots: Vec<_> = ws
                .targets
                .iter()
                .map(|t| uri::normalize(&t.sourceroot))
                .collect();
            roots.sort();
            roots.dedup();
            for root in roots {
                let Ok(rel) = path.strip_prefix(&root) else {
                    continue;
                };
                let rel = rel.to_string_lossy().replace('\\', "/");
                if let Some(doc) = (0..seg.doc_count()).find(|&d| seg.uri_of(d) == rel) {
                    let target_ord = seg.target_ord_of_doc(doc);
                    if target_ord >= 0 {
                        let bsp_id = seg.target_meta(target_ord as u32).bsp_id;
                        return Some(ws.forward_dependency_closure(&bsp_id));
                    }
                }
            }
        }
        // Fallback for files the snapshot does not hold (fresh/unsaved
        // sources): the deepest containing sourceroot.
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

/// Whether `query` matches the method display `name` under the metals
/// `Fuzzy.matches` single-name semantics (the PC's member-completion query is a
/// plain identifier, so only the main-name comparison of `Fuzzy.matchesName`
/// applies): an EMPTY query matches everything; otherwise a case-sensitive
/// scan where an UPPERCASE query char may skip ahead to the next matching
/// symbol char (camel-hump matching, e.g. `mE` matches `myExt`), a lowercase
/// mismatch backtracks to the last uppercase anchor, and a plain
/// case-sensitive prefix always matches. Conservative on purpose: the
/// compiler re-filters every hit for applicability, so over-matching is safe
/// while a whole-index answer for a non-empty query is not.
fn fuzzy_matches_name(query: &str, name: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let q: Vec<char> = query.chars().collect();
    let s: Vec<char> = name.chars().collect();
    // A faithful port of `Fuzzy.matchesName`'s loop: `(ql, sl)` are the last
    // (query, symbol) indices where an uppercase query char aligned, the
    // backtrack anchor for a later lowercase mismatch.
    let (mut qa, mut ql) = (0usize, None::<usize>);
    let (mut sa, mut sl) = (0usize, None::<usize>);
    loop {
        if qa >= q.len() {
            return true;
        }
        if sa >= s.len() {
            return false;
        }
        let qq = q[qa];
        let ss = s[sa];
        if qq == ss {
            if qq.is_uppercase() {
                ql = Some(qa);
            }
            if ss.is_uppercase() {
                sl = Some(sa);
            }
            qa += 1;
            sa += 1;
        } else if qq.is_lowercase() {
            // Backtrack to the anchors (the aligned uppercase pair) + 1; with
            // no anchor the lowercase query char must match in place.
            match (ql, sl) {
                (Some(qanchor), Some(sanchor)) => {
                    qa = qanchor;
                    sa = sanchor + 1;
                    ql = None;
                    sl = None;
                }
                _ => return false,
            }
        } else {
            // An uppercase (or non-letter) query char skips symbol chars until
            // it aligns — the camel-hump jump.
            sa += 1;
        }
    }
}

/// Dedupe by (symbol, uri, span) and sort the same way — a stable order for
/// the PC visitor and tests (mirrors [`dedupe_and_sort_locs`]).
fn dedupe_and_sort_method_hits(mut hits: Vec<MethodHit>) -> Vec<MethodHit> {
    hits.sort_by(|a, b| {
        a.symbol
            .cmp(&b.symbol)
            .then(a.uri.cmp(&b.uri))
            .then(a.span.start_line.cmp(&b.span.start_line))
            .then(a.span.start_char.cmp(&b.span.start_char))
            .then(a.span.end_line.cmp(&b.span.end_line))
            .then(a.span.end_char.cmp(&b.span.end_char))
    });
    hits.dedup_by(|a, b| a.symbol == b.symbol && a.uri == b.uri && a.span == b.span);
    hits
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
