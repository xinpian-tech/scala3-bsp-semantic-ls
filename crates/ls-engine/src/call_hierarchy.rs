//! Index-backed call hierarchy (`textDocument/prepareCallHierarchy`,
//! `callHierarchy/incomingCalls`, `callHierarchy/outgoingCalls`).
//!
//! v1 ships USAGE-HIERARCHY semantics (the ratified Plan C): a "call" is any
//! REFERENCE occurrence of the item's reference group — eta-expanded method
//! values, type-position references and other non-invocation uses are all
//! included — with exactly ONE noise filter: a reference sitting on an
//! `import` statement line is excluded. The index persists no call-site facts
//! (an occurrence does not know whether it is an application), so the honest
//! v1 answer is the usage graph; the precision upgrade — persisting call-site
//! facts at ingest — is recorded as a follow-up in `docs/traceability.md` and
//! deliberately NOT implemented here.
//!
//! **The import-line rule** (the one Plan C filter), applied at query time
//! against the doc's on-disk source: a reference is dropped iff the source
//! line its span STARTS on, after stripping leading whitespace, begins with
//! the token `import` — i.e. the line is exactly `import` or continues with a
//! non-identifier character (`import pkga.Core` filters; `importantUse(1)`
//! does not). Only the reference's own start line is inspected, so the
//! continuation lines of a brace-wrapped multi-line import are not filtered
//! (accepted noise). The filter is best-effort by design: a doc whose source
//! cannot be read (renamed/deleted since ingest) keeps its references —
//! noise reduction must never silently hide callers.
//!
//! **Enclosing-definition containment rule.** The index stores definition
//! NAME spans only (no body extents — `docs/index-format.md`), so attributing
//! a reference to its enclosing definition synthesizes containment from
//! source order and the SemanticDB owner chain. The doc's DEFINITION ENTRIES
//! are the `document_symbols` node set (global Term/Type/Method descriptors;
//! no locals, parameters, constructors or setters), position-sorted. For a
//! reference starting at `P`:
//!
//! 1. The candidate is the LAST entry whose name span starts at-or-before
//!    `P`; with no such entry the reference belongs to no definition
//!    (`None` — callers surface it under a synthetic file-level item).
//! 2. An entry `E` CONTAINS `P` iff `E`'s successor — the first later entry
//!    that is not `E`'s descendant by owner chain (companion-aware, so an
//!    enum case counts as its enum class's descendant) — does not exist or
//!    starts on a line strictly after `P`'s line.
//! 3. If the candidate does not contain `P` (its synthesized extent closed on
//!    or before `P`'s line), walk UP the candidate's owner chain to the
//!    nearest ancestor (or ancestor's companion) that has an entry and re-test;
//!    an exhausted chain answers `None`.
//!
//! Documented consequences, pinned by tests: a reference in a val initializer
//! attributes to the val; a reference in a nested definition attributes to
//! the nearest (deepest) definition; toplevel code after the LAST entry of a
//! class body attributes to that last entry (the name-span-only false
//! positive — no later entry exists to close the extent); a reference on the
//! same line as, but before, a later definition's name (an extension binder,
//! `extension (c: T) def ext = ...`) walks up and typically lands on the
//! file-level item, because line-granular extents cannot split that line.
//!
//! **prepareCallHierarchy** answers the DEFINITION-SIDE item of the cursor
//! symbol when it is CALLABLE — a global whose SemanticDB descriptor is a
//! method (a `def`; constructors and setters excluded), or a term whose
//! indexed kind is METHOD/MACRO (member `val`s and enum cases are METHOD-kind
//! getters in SemanticDB — "defs/vals with method descriptors"). For a
//! non-callable resolution (a class/trait/object/local under the cursor) the
//! answer is the cursor position's enclosing CALLABLE definition when one
//! exists — walking the enclosing entry's owner chain upward past
//! non-callable enclosers — else `None`. A callable whose definition lives
//! outside the workspace (`toUpperCase`) answers `None`: an item needs a
//! definition location, and answering the enclosing definition for a symbol
//! the user explicitly named would be a surprise. Cursor-resolution failures
//! are the typed references-style errors (`NoSymbolAtCursor`, `StaleIndex`,
//! `PcOnlySymbol`, ...).
//!
//! **incomingCalls** scans the item's reference group with NO target-closure
//! pruning — deliberately different from both `symbol_definition` (forward
//! closure) and `references` (reverse closure): incoming callers legitimately
//! live downstream of the definition, and v1 keeps the query snapshot-only
//! (no workspace-graph read), so even a disconnected target that redefines
//! the same symbol string contributes its callers (accepted usage-hierarchy
//! noise, pinned by test). Remaining references are grouped per doc by their
//! enclosing definition; each caller answers one edge with its call-site
//! spans (`fromRanges`, in the caller's doc).
//!
//! **outgoingCalls** approximates the item's body as the SUCCESSOR-BASED
//! extent: from the item's definition name span to the start of the next
//! non-descendant entry in its defining doc (end of doc when none). This is
//! exactly the synthetic extent `docs/architecture.md` §7.1 records as
//! REJECTED for outlines — an outline must not claim extents the index does
//! not know — but it is acceptable as a query heuristic here, and the
//! asymmetry is deliberate: outgoing calls are a best-effort projection, so
//! trailing non-definition code before the next entry may be misattributed
//! to the item (pinned by test). REFERENCE occurrences inside the extent
//! (minus import lines) resolve to their target's definition item from
//! `symbol_meta` (the `WorkspaceSymbolEntry` location source; locals,
//! parameters, packages, constructors, setters and externally-defined
//! targets are dropped), grouped by target.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;

use ls_index_model::{occ_flags, LsError, Span, SymKind};
use ls_semanticdb::symbols::{self, Descriptor};
use ls_store::SegmentReader;

use crate::orchestrator::QueryOrchestrator;
use crate::symbol_encoding;

/// One call-hierarchy item: a definition (or the synthetic file-level item)
/// with its display name, index kind, SemanticDB-relative doc uri and NAME
/// span. `range == selectionRange` at the LSP edge — the documentSymbol
/// name-span-only discipline. `symbol` is the raw SemanticDB symbol the item
/// round-trips through the LSP `data` field; it is EMPTY exactly for the
/// synthetic file-level item (whose incoming/outgoing answer empty).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallItem {
    pub name: String,
    pub kind: SymKind,
    /// SemanticDB-relative doc uri (as `references` emits).
    pub uri: String,
    /// The definition NAME span (LSP `range` and `selectionRange`).
    pub span: Span,
    /// The raw SemanticDB symbol; empty for the synthetic file-level item.
    pub symbol: String,
}

/// One incoming/outgoing edge: the caller (incoming) or callee (outgoing)
/// item plus the call-site spans (`fromRanges`) — in the CALLER's doc for
/// incoming, in the ITEM's doc for outgoing (the LSP contract).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallEdge {
    pub item: CallItem,
    pub call_sites: Vec<Span>,
}

/// One definition entry of a doc: the `document_symbols` node set, flat and
/// position-sorted, carrying what containment synthesis needs.
struct DefEntry {
    /// Raw SemanticDB symbol.
    symbol: String,
    /// Snapshot symbol ordinal.
    ord: u32,
    /// Packed name-span start (the sort key).
    start: u32,
    /// The definition NAME span.
    span: Span,
    /// Proper owner chain, outermost first (self excluded).
    owners: Vec<String>,
}

impl QueryOrchestrator {
    /// `textDocument/prepareCallHierarchy`: the definition-side [`CallItem`]
    /// under the cursor, per the module rules (callable → its own definition
    /// item; non-callable → the enclosing callable; external/unknown →
    /// `None`). Typed references-style errors for cursor-resolution failures.
    pub fn prepare_call_hierarchy(
        &self,
        uri: &str,
        line: u32,
        character: u32,
    ) -> Result<Option<CallItem>, LsError> {
        let cursor = self.symbol_at_cursor(uri, line, character)?;
        if cursor.pc_only {
            return Err(LsError::PcOnlySymbol);
        }
        let Some(snap) = self.current_snapshot() else {
            return Err(LsError::NotIndexed {
                uri: uri.to_string(),
            });
        };
        let seg = snap.segment();
        if !cursor.is_local() {
            match seg.find_symbol_ord(&symbol_encoding::encode(&cursor.semantic_symbol, None)) {
                Some(ord) => {
                    if is_callable(&cursor.semantic_symbol, seg.symbol_meta(ord).kind) {
                        // The definition-side item; None for an external
                        // callable (no workspace definition to anchor on).
                        return Ok(definition_item(seg, ord));
                    }
                }
                // A fresh symbol the snapshot has not seen (RawSemanticDBPath):
                // no item yet; production write-through heals inline, so the
                // next query answers from the healed snapshot.
                None => return Ok(None),
            }
        }
        // Non-callable (or local) resolution: the enclosing callable of the
        // cursor position in its own doc, walking up past non-callable
        // enclosers (a ref inside a class body but outside every method
        // answers None).
        let Some(doc_ord) = doc_ord_of(seg, uri) else {
            return Ok(None);
        };
        let entries = doc_def_entries(seg, doc_ord);
        let packed = Span::pack(cursor.span.start_line, cursor.span.start_char);
        let mut at = enclosing_entry_index(&entries, packed);
        while let Some(i) = at {
            let entry = &entries[i];
            if is_callable(&entry.symbol, seg.symbol_meta(entry.ord).kind) {
                return Ok(Some(entry_item(seg, doc_ord, entry)));
            }
            at = owner_entry_index(&entries, entry);
        }
        Ok(None)
    }

    /// `callHierarchy/incomingCalls`: the item's callers under the module's
    /// usage-hierarchy rules — the full reference group, UNPRUNED (see the
    /// module doc for why no closure applies), minus import-line references,
    /// grouped per doc by enclosing definition. References enclosed by no
    /// definition surface under the synthetic file-level item. Sorted by
    /// (uri, caller span); an unknown/empty/local item symbol answers empty.
    pub fn incoming_calls(&self, item_symbol: &str) -> Vec<CallEdge> {
        if item_symbol.is_empty() || symbols::is_local(item_symbol) {
            return Vec::new();
        }
        let Some(snap) = self.current_snapshot() else {
            return Vec::new();
        };
        let seg = snap.segment();
        let Some(ord) = seg.find_symbol_ord(&symbol_encoding::encode(item_symbol, None)) else {
            return Vec::new();
        };
        let ref_group = seg.symbol_view(ord).ref_group_ord;
        if ref_group < 0 {
            return Vec::new();
        }
        let mut refs: Vec<(u32, Span)> = Vec::new();
        // `allowed = None`: the deliberate no-pruning scan (module doc).
        seg.scan_ref_group(ref_group as u32, None, &mut |rec| {
            refs.push((
                rec.doc_ord as u32,
                record_span(rec.packed_start, rec.packed_end),
            ));
        });
        let mut sources = SourceCache::default();
        let mut entries_of: HashMap<u32, Vec<DefEntry>> = HashMap::new();
        // Key: (doc_ord, entry index; -1 = the synthetic file-level item).
        let mut grouped: BTreeMap<(u32, i64), Vec<Span>> = BTreeMap::new();
        for (doc_ord, span) in refs {
            if sources.is_import_line(seg, doc_ord, span.start_line) {
                continue; // the Plan C filter
            }
            let entries = entries_of
                .entry(doc_ord)
                .or_insert_with(|| doc_def_entries(seg, doc_ord));
            let key = enclosing_entry_index(entries, Span::pack(span.start_line, span.start_char))
                .map(|i| i as i64)
                .unwrap_or(-1);
            grouped.entry((doc_ord, key)).or_default().push(span);
        }
        let mut out: Vec<CallEdge> = grouped
            .into_iter()
            .map(|((doc_ord, key), spans)| {
                let item = if key >= 0 {
                    entry_item(seg, doc_ord, &entries_of[&doc_ord][key as usize])
                } else {
                    synthetic_file_item(seg.uri_of(doc_ord))
                };
                CallEdge {
                    item,
                    call_sites: sort_spans(spans),
                }
            })
            .collect();
        sort_edges(&mut out);
        out
    }

    /// `callHierarchy/outgoingCalls`: the item's callees under the module's
    /// usage-hierarchy rules — REFERENCE occurrences inside the item's
    /// successor-based extent in its defining doc (the documented body
    /// heuristic), minus import lines, resolved to their targets' definition
    /// items and grouped by target. Sorted by (uri, target span); an
    /// unknown/empty/local/externally-defined item answers empty.
    pub fn outgoing_calls(&self, item_symbol: &str) -> Vec<CallEdge> {
        if item_symbol.is_empty() || symbols::is_local(item_symbol) {
            return Vec::new();
        }
        let Some(snap) = self.current_snapshot() else {
            return Vec::new();
        };
        let seg = snap.segment();
        let Some(ord) = seg.find_symbol_ord(&symbol_encoding::encode(item_symbol, None)) else {
            return Vec::new();
        };
        let def_doc = seg.symbol_meta(ord).def_doc_ord;
        if def_doc < 0 {
            return Vec::new();
        }
        let doc_ord = def_doc as u32;
        let entries = doc_def_entries(seg, doc_ord);
        let Some(pos) = entries.iter().position(|e| e.symbol == item_symbol) else {
            return Vec::new();
        };
        // The synthetic extent: name span start -> the next non-descendant
        // entry's start (end of doc when none). §7.1's rejected-for-outlines
        // heuristic, accepted here (module doc).
        let extent_start = entries[pos].start;
        let extent_end = successor_start(&entries, pos).unwrap_or(u32::MAX);
        let mut sources = SourceCache::default();
        let mut grouped: BTreeMap<u32, Vec<Span>> = BTreeMap::new();
        seg.scan_doc(doc_ord, false, &mut |rec| {
            if occ_flags::has(rec.flags as u32, occ_flags::DEFINITION) {
                return;
            }
            let ps = rec.packed_start as u32;
            if ps < extent_start || ps >= extent_end {
                return;
            }
            let span = record_span(rec.packed_start, rec.packed_end);
            if sources.is_import_line(seg, doc_ord, span.start_line) {
                return;
            }
            let (raw, local_doc) =
                symbol_encoding::decode(seg.semantic_symbol_of(rec.symbol_ord as u32));
            // Locals never form items; packages/parameters/type-parameters are
            // not call targets; constructors and setters are excluded exactly
            // as documentSymbol excludes their nodes.
            if local_doc.is_some() {
                return;
            }
            match symbols::descriptor_of(&raw) {
                Some(Descriptor::Term(_) | Descriptor::Type(_)) => {}
                Some(Descriptor::Method(name, _))
                    if name != symbols::CONSTRUCTOR_NAME && !symbols::is_setter(&raw) => {}
                _ => return,
            }
            grouped.entry(rec.symbol_ord as u32).or_default().push(span);
        });
        let mut out: Vec<CallEdge> = grouped
            .into_iter()
            .filter_map(|(target_ord, spans)| {
                // A target defined outside the workspace resolves to no item.
                definition_item(seg, target_ord).map(|item| CallEdge {
                    item,
                    call_sites: sort_spans(spans),
                })
            })
            .collect();
        sort_edges(&mut out);
        out
    }
}

/// Whether a global symbol is CALLABLE (module doc): a method-descriptor
/// `def` (constructors and setters excluded), or a term whose indexed kind is
/// METHOD/MACRO (member vals / enum cases — SemanticDB getters).
fn is_callable(raw: &str, meta_kind: i32) -> bool {
    match symbols::descriptor_of(raw) {
        Some(Descriptor::Method(name, _)) => {
            name != symbols::CONSTRUCTOR_NAME && !symbols::is_setter(raw)
        }
        Some(Descriptor::Term(_)) => {
            matches!(
                SymKind::from_code(meta_kind),
                SymKind::Method | SymKind::Macro
            )
        }
        _ => false,
    }
}

/// The doc ordinal of `uri` in the current snapshot (linear over the doc
/// table, as the orchestrator's own lookup does).
fn doc_ord_of(seg: &SegmentReader, uri: &str) -> Option<u32> {
    (0..seg.doc_count()).find(|&d| seg.uri_of(d) == uri)
}

fn record_span(packed_start: i32, packed_end: i32) -> Span {
    let ps = packed_start as u32;
    let pe = packed_end as u32;
    Span::new(
        Span::unpack_line(ps),
        Span::unpack_char(ps),
        Span::unpack_line(pe),
        Span::unpack_char(pe),
    )
}

/// The doc's definition entries: the `document_symbols` node-selection filter
/// (global Term/Type/Method; no locals/constructors/setters; first definition
/// occurrence wins), flat and position-sorted.
fn doc_def_entries(seg: &SegmentReader, doc_ord: u32) -> Vec<DefEntry> {
    let mut seen: HashSet<String> = HashSet::new();
    let mut out: Vec<DefEntry> = Vec::new();
    seg.scan_doc(doc_ord, false, &mut |rec| {
        if !occ_flags::has(rec.flags as u32, occ_flags::DEFINITION) {
            return;
        }
        let sym_ord = rec.symbol_ord as u32;
        let (raw, local_doc) = symbol_encoding::decode(seg.semantic_symbol_of(sym_ord));
        if local_doc.is_some() {
            return;
        }
        match symbols::descriptor_of(&raw) {
            Some(Descriptor::Term(_) | Descriptor::Type(_)) => {}
            Some(Descriptor::Method(name, _))
                if name != symbols::CONSTRUCTOR_NAME && !symbols::is_setter(&raw) => {}
            _ => return,
        }
        if !seen.insert(raw.clone()) {
            return;
        }
        let chain = symbols::owner_chain(&raw);
        let owners = chain[..chain.len().saturating_sub(1)].to_vec();
        out.push(DefEntry {
            symbol: raw,
            ord: sym_ord,
            start: rec.packed_start as u32,
            span: record_span(rec.packed_start, rec.packed_end),
            owners,
        });
    });
    out.sort_by_key(|e| e.start);
    out
}

/// Whether `entry` is a descendant of `ancestor_symbol` by owner chain,
/// companion-aware (an enum case owned by the synthetic companion object
/// counts as the enum class's descendant — the documentSymbol fallback).
fn is_descendant(entry: &DefEntry, ancestor_symbol: &str) -> bool {
    if entry.owners.iter().any(|o| o == ancestor_symbol) {
        return true;
    }
    symbols::companion(ancestor_symbol).is_some_and(|c| entry.owners.iter().any(|o| o == &c))
}

/// The packed start of the entry's SUCCESSOR: the first later entry that is
/// not its descendant — where the entry's synthesized extent closes. `None`
/// when the entry is the last non-descendant-covered one (extent runs to end
/// of doc).
fn successor_start(entries: &[DefEntry], idx: usize) -> Option<u32> {
    let symbol = &entries[idx].symbol;
    entries[idx + 1..]
        .iter()
        .find(|e| !is_descendant(e, symbol))
        .map(|e| e.start)
}

/// The entry index of the nearest ancestor (or ancestor's companion) of
/// `entry` that has an entry in this doc, walking the owner chain
/// innermost-first.
fn owner_entry_index(entries: &[DefEntry], entry: &DefEntry) -> Option<usize> {
    entry.owners.iter().rev().find_map(|ancestor| {
        entries
            .iter()
            .position(|e| &e.symbol == ancestor)
            .or_else(|| {
                symbols::companion(ancestor)
                    .and_then(|c| entries.iter().position(|e| e.symbol == c))
            })
    })
}

/// The module-doc containment rule: last entry at-or-before `packed`, walked
/// up the owner chain past entries whose synthesized extent closed on or
/// before `packed`'s line.
fn enclosing_entry_index(entries: &[DefEntry], packed: u32) -> Option<usize> {
    let mut at = entries.iter().rposition(|e| e.start <= packed)?;
    loop {
        let contained = match successor_start(entries, at) {
            None => true,
            Some(succ) => Span::unpack_line(succ) > Span::unpack_line(packed),
        };
        if contained {
            return Some(at);
        }
        at = owner_entry_index(entries, &entries[at])?;
    }
}

/// The [`CallItem`] of a definition entry, located at the entry's own name
/// span in `doc_ord` (its definition site by construction). Display name and
/// kind come from `symbol_meta` (the `WorkspaceSymbolEntry` source), the
/// descriptor name backing an empty display.
fn entry_item(seg: &SegmentReader, doc_ord: u32, entry: &DefEntry) -> CallItem {
    let meta = seg.symbol_meta(entry.ord);
    CallItem {
        name: display_name(&meta.display, &entry.symbol),
        kind: SymKind::from_code(meta.kind),
        uri: seg.uri_of(doc_ord).to_string(),
        span: entry.span,
        symbol: entry.symbol.clone(),
    }
}

/// The [`CallItem`] at a symbol's recorded definition (`symbol_meta`'s
/// def doc + def name span), or `None` for a symbol defined outside the
/// workspace (or a local).
fn definition_item(seg: &SegmentReader, sym_ord: u32) -> Option<CallItem> {
    let meta = seg.symbol_meta(sym_ord);
    if meta.def_doc_ord < 0 {
        return None;
    }
    let (raw, local_doc) = symbol_encoding::decode(seg.semantic_symbol_of(sym_ord));
    if local_doc.is_some() {
        return None;
    }
    Some(CallItem {
        name: display_name(&meta.display, &raw),
        kind: SymKind::from_code(meta.kind),
        uri: seg.uri_of(meta.def_doc_ord as u32).to_string(),
        span: record_span(meta.def_packed_start, meta.def_packed_end),
        symbol: raw,
    })
}

/// The synthetic file-level item for references enclosed by no definition:
/// named after the doc's file name, kind unknown (mapped to `File` at the LSP
/// edge), zero span, EMPTY symbol (incoming/outgoing on it answer empty).
fn synthetic_file_item(doc_uri: &str) -> CallItem {
    let name = doc_uri.rsplit('/').next().unwrap_or(doc_uri).to_string();
    CallItem {
        name,
        kind: SymKind::UnknownKind,
        uri: doc_uri.to_string(),
        span: Span::new(0, 0, 0, 0),
        symbol: String::new(),
    }
}

fn display_name(display: &str, raw: &str) -> String {
    if !display.is_empty() {
        display.to_string()
    } else {
        symbols::descriptor_of(raw)
            .map(|d| d.name().to_string())
            .unwrap_or_else(|| raw.to_string())
    }
}

fn sort_spans(mut spans: Vec<Span>) -> Vec<Span> {
    spans.sort_by_key(|s| (s.start_line, s.start_char, s.end_line, s.end_char));
    spans.dedup();
    spans
}

fn sort_edges(edges: &mut [CallEdge]) {
    edges.sort_by(|a, b| {
        a.item
            .uri
            .cmp(&b.item.uri)
            .then(a.item.span.start_line.cmp(&b.item.span.start_line))
            .then(a.item.span.start_char.cmp(&b.item.span.start_char))
            .then(a.item.name.cmp(&b.item.name))
    });
}

/// Per-query source-line cache for the import-line filter. `None` = the
/// source could not be read — the filter FAILS OPEN (references are kept):
/// noise reduction must never hide callers behind a vanished file.
#[derive(Default)]
struct SourceCache {
    lines: HashMap<u32, Option<Vec<String>>>,
}

impl SourceCache {
    /// Whether `line` of `doc_ord`'s on-disk source is an `import` statement
    /// line per the module's exact rule. Out-of-range lines (a source shorter
    /// than the indexed one — stale) answer `false` (kept).
    fn is_import_line(&mut self, seg: &SegmentReader, doc_ord: u32, line: u32) -> bool {
        let lines = self
            .lines
            .entry(doc_ord)
            .or_insert_with(|| read_doc_lines(seg, doc_ord));
        match lines {
            Some(lines) => lines.get(line as usize).is_some_and(|l| line_is_import(l)),
            None => false,
        }
    }
}

/// Reads the doc's source through its OWN target's sourceroot (its
/// `target_ord`, not the workspace-order primary — the `raw_sdb_document_of`
/// discipline), best-effort.
fn read_doc_lines(seg: &SegmentReader, doc_ord: u32) -> Option<Vec<String>> {
    let target_ord = seg.target_ord_of_doc(doc_ord);
    if target_ord < 0 {
        return None;
    }
    let root = PathBuf::from(seg.target_meta(target_ord as u32).sourceroot);
    let text = std::fs::read_to_string(root.join(seg.uri_of(doc_ord))).ok()?;
    Some(text.lines().map(str::to_string).collect())
}

/// The exact import-line predicate (module doc): leading whitespace stripped,
/// the line starts with the TOKEN `import` — `import` alone, or `import`
/// followed by a non-identifier character. `importantUse(1)` is not an
/// import line.
fn line_is_import(line: &str) -> bool {
    match line.trim_start().strip_prefix("import") {
        None => false,
        Some(rest) => !rest
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric() || c == '_' || c == '$'),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // The exact Plan C rule: `import` as a leading token filters; an
    // identifier merely starting with "import" does not.
    #[test]
    fn the_import_line_rule_matches_the_token_not_the_prefix() {
        assert!(line_is_import("import pkga.Core"));
        assert!(line_is_import("  import pkga.*"));
        assert!(line_is_import("\timport a.b"));
        assert!(line_is_import("import"));
        assert!(line_is_import("import{a}"));
        assert!(!line_is_import("importantUse(1)"));
        assert!(!line_is_import("import_x.y"));
        assert!(!line_is_import("val import2 = 1"));
        assert!(!line_is_import("  Core.make(\"a\")"));
    }
}
