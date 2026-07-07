//! The production presentation-compiler dirty-buffer overlay — a behavior-
//! preserving port of the Scala `ls.core.PcOverlay` (the PCPath).
//!
//! Every URI arriving at the engine's [`DirtyBufferOverlay`] hooks is a
//! SemanticDB URI; the late-installed `to_file_uri` closure maps it onto the
//! `file://` URI the [`DocumentStore`] and the presentation compiler speak.
//! Until [`PcOverlayInner::install`] runs, nothing is ever dirty and the engine
//! stays on index truth.
//!
//! `symbol_at` is the honestly-implementable v1: the mtags PC exposes no
//! symbol-occurrence API beyond definition/prepareRename, so the SemanticDB
//! symbol string comes from PC `definition_result` (`PcDefinition::symbol`) and
//! the occurrence span from PC `prepare_rename` — falling back to the identifier
//! token under the cursor in the buffer text when prepareRename declines (dotty
//! only offers rename ranges for file-local symbols; the span is presentation-
//! only, the symbol stays PC semantic truth). When the symbol is unavailable the
//! overlay answers `None` and the query degrades to `StaleIndex` — never a guess
//! against an index that has not seen the buffer. `pc_only` is true exactly when
//! the definition resolves only into synthetic/plugin origins.
//!
//! `occurrences_of` contributes nothing in v1 (the PC has no occurrence scan to
//! back it), so `contributes_occurrences()` stays false and references over
//! dirty buffers remain index-truth only — a permitted degrade, not a guess.
//!
//! The overlay lives INSIDE the [`QueryOrchestrator`](ls_engine::QueryOrchestrator);
//! its `to_file_uri`/`is_indexed_name` closures hold a `Weak` back-reference to
//! that orchestrator, installed after bootstrap once the ready bundle exists
//! (matching the Scala `install(pc, toFileUri, isIndexedName)`).

use std::sync::{Arc, RwLock};

use ls_engine::{DirtyBufferOverlay, OverlayHit};
use ls_index_model::{Loc, Role, Span};

use crate::documents::DocumentStore;
use crate::pc::{PcDefOrigin, PcQueryService, PcSpan};

/// A top-level symbol that exists only in an open, unsaved buffer (not in the
/// persisted index): surfaced by `workspace/symbol` flagged PC-only, and
/// excluded from global references/rename.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PcOnlySymbol {
    pub name: String,
    pub keyword: String,
    pub file_uri: String,
    pub span: Span,
}

/// SemanticDB URI -> `file://` URI (the orchestrator's metadata truth). Its own
/// errors already fold to `None` inside the closure.
pub type FileUriResolver = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// Whether a display name is present in the persisted index. Fail-safe: a
/// membership check that cannot run answers `true` (so a query error never
/// spuriously refuses global references/rename), decided by the closure.
pub type IndexedNameCheck = Box<dyn Fn(&str) -> bool + Send + Sync>;

/// The late-bound overlay environment (the Scala `install(pc, toFileUri,
/// isIndexedName)`): the shared document store, the PC query seam, and the two
/// closures that reach back into the ready bundle for URI mapping and index
/// name-membership.
struct Env {
    docs: Arc<DocumentStore>,
    pc: Arc<dyn PcQueryService>,
    to_file_uri: FileUriResolver,
    is_indexed_name: IndexedNameCheck,
}

/// The shared overlay state, held both by the orchestrator (as the boxed
/// [`DirtyBufferOverlay`], via [`PcOverlay`]) and by the ready services (as an
/// `Arc` handle for [`install`](Self::install) and
/// [`pc_only_symbols`](Self::pc_only_symbols)).
pub struct PcOverlayInner {
    env: RwLock<Option<Env>>,
}

impl PcOverlayInner {
    fn new() -> PcOverlayInner {
        PcOverlayInner {
            env: RwLock::new(None),
        }
    }

    /// Install (or replace, after a reload) the overlay environment. Idempotent
    /// on repeat; a later install swaps in the fresh `to_file_uri`/`pc` bindings.
    pub fn install(
        &self,
        docs: Arc<DocumentStore>,
        pc: Arc<dyn PcQueryService>,
        to_file_uri: FileUriResolver,
        is_indexed_name: IndexedNameCheck,
    ) {
        *self.env.write().unwrap() = Some(Env {
            docs,
            pc,
            to_file_uri,
            is_indexed_name,
        });
    }

    /// Whether the environment has been installed (the Scala `installed`).
    pub fn is_installed(&self) -> bool {
        self.env.read().unwrap().is_some()
    }

    fn is_dirty(&self, sdb_uri: &str) -> bool {
        let guard = self.env.read().unwrap();
        match guard.as_ref() {
            None => false,
            Some(env) => {
                (env.to_file_uri)(sdb_uri).is_some_and(|file_uri| env.docs.is_dirty(&file_uri))
            }
        }
    }

    fn symbol_at(&self, sdb_uri: &str, line: u32, character: u32) -> Option<OverlayHit> {
        let guard = self.env.read().unwrap();
        let env = guard.as_ref()?;
        let file_uri = (env.to_file_uri)(sdb_uri)?;
        if !env.pc.is_open(&file_uri) {
            return None;
        }
        // A top-level declaration in the dirty buffer whose name the persisted
        // index has never seen is PC-only (global references and rename refuse
        // it); otherwise fall back to the PC symbol resolution.
        pc_only_top_level_hit(env, &file_uri, line, character)
            .or_else(|| pc_hit(env, &file_uri, line, character))
    }

    /// Top-level symbols declared in open, unsaved buffers whose names the
    /// persisted index has never seen, matched (case-insensitive substring)
    /// against the `workspace/symbol` query.
    pub fn pc_only_symbols(&self, query: &str) -> Vec<PcOnlySymbol> {
        let guard = self.env.read().unwrap();
        let Some(env) = guard.as_ref() else {
            return Vec::new();
        };
        let q = query.to_lowercase();
        let mut out = Vec::new();
        for file_uri in env.docs.open_uris() {
            if !env.docs.is_dirty(&file_uri) {
                continue;
            }
            let Some(text) = env.docs.text(&file_uri) else {
                continue;
            };
            for decl in top_level_decls(&text) {
                if !q.is_empty() && !decl.name.to_lowercase().contains(&q) {
                    continue;
                }
                if (env.is_indexed_name)(&decl.name) {
                    continue;
                }
                out.push(PcOnlySymbol {
                    name: decl.name,
                    keyword: decl.keyword,
                    file_uri: file_uri.clone(),
                    span: decl.span,
                });
            }
        }
        out
    }
}

/// A PC-only overlay hit when the cursor sits on the name of a top-level
/// declaration in the dirty buffer that the persisted index has never seen. The
/// synthetic symbol string is never used: `pc_only` short-circuits the engines
/// before they read it.
fn pc_only_top_level_hit(
    env: &Env,
    file_uri: &str,
    line: u32,
    character: u32,
) -> Option<OverlayHit> {
    let text = env.docs.text(file_uri)?;
    top_level_decls(&text)
        .into_iter()
        .find(|decl| {
            decl.span.start_line == line
                && character >= decl.span.start_char
                && character <= decl.span.end_char
        })
        .filter(|decl| !(env.is_indexed_name)(&decl.name))
        .map(|decl| OverlayHit {
            semantic_symbol: format!("local/{}#", decl.name),
            span: decl.span,
            role: Role::Definition,
            pc_only: true,
        })
}

/// The normal PC symbol resolution: prepareRename span (with a buffer-token
/// fallback) plus the PC definition's symbol and origins.
fn pc_hit(env: &Env, file_uri: &str, line: u32, character: u32) -> Option<OverlayHit> {
    let span = env
        .pc
        .prepare_rename(file_uri, line, character)
        .map(span_of_pc)
        .or_else(|| token_span_at(&env.docs, file_uri, line, character))?;
    let defs = env.pc.definition_result(file_uri, line, character);
    if defs.symbol.is_empty() {
        return None;
    }
    let is_definition = defs.locations.iter().any(|dl| {
        dl.origin == PcDefOrigin::Workspace
            && dl.uri == file_uri
            && span_of_pc(dl.span.clone()) == span
    });
    // PC-only exactly when the definition resolves only into non-workspace
    // (synthetic/plugin) origins (plan 14.5).
    let pc_only = !defs.locations.is_empty()
        && defs
            .locations
            .iter()
            .all(|dl| dl.origin != PcDefOrigin::Workspace);
    Some(OverlayHit {
        semantic_symbol: defs.symbol,
        span,
        role: if is_definition {
            Role::Definition
        } else {
            Role::Reference
        },
        pc_only,
    })
}

fn span_of_pc(pc: PcSpan) -> Span {
    Span::new(
        pc.start_line,
        pc.start_character,
        pc.end_line,
        pc.end_character,
    )
}

/// The identifier token covering the cursor in the OPEN BUFFER text (the overlay
/// only ever runs on dirty buffers, so the buffer is the only honest text
/// source). `None` when the cursor is not on an identifier. Offsets are UTF-16
/// code units, matching the LSP character coordinate and the Scala `charAt`
/// scan.
fn token_span_at(docs: &DocumentStore, file_uri: &str, line: u32, character: u32) -> Option<Span> {
    let text = docs.text(file_uri)?;
    let line_text = text.lines().nth(line as usize)?;
    let units: Vec<u16> = line_text.encode_utf16().collect();
    let n = units.len();
    let mut start = (character as usize).min(n);
    let mut end = start;
    while start > 0 && is_id_part(units[start - 1]) {
        start -= 1;
    }
    while end < n && is_id_part(units[end]) {
        end += 1;
    }
    (end > start).then(|| Span::new(line, start as u32, line, end as u32))
}

/// Identifier-part test on a UTF-16 code unit: `$`, or a Unicode
/// identifier-part BMP scalar (a close port of Java's
/// `Character.isUnicodeIdentifierPart`; sufficient since the span is
/// presentation-only).
fn is_id_part(unit: u16) -> bool {
    unit == b'$' as u16
        || char::from_u32(unit as u32).is_some_and(|c| c.is_alphanumeric() || c == '_')
}

/// A top-level declaration `keyword Name` at column 0 (optionally preceded by
/// modifiers): the span covers the name token only.
#[derive(Clone, Debug, PartialEq, Eq)]
struct TopLevelDecl {
    name: String,
    keyword: String,
    span: Span,
}

const MODIFIERS: &[&str] = &[
    "private",
    "protected",
    "final",
    "sealed",
    "abstract",
    "case",
    "open",
    "implicit",
    "lazy",
    "override",
    "inline",
    "transparent",
];
const KEYWORDS: &[&str] = &[
    "object", "class", "trait", "enum", "def", "val", "var", "type",
];

/// Top-level declarations in a buffer: the declaration keyword sits at column 0
/// (top-level members in Scala 3; nested members are indented). A light scan —
/// no PC round-trip — is enough to surface unsaved symbols. Ports the Scala
/// `PcOverlay.topLevelDecls` regex `^(?:(?:mod)\s+)*(keyword)\s+(ident)`.
fn top_level_decls(text: &str) -> Vec<TopLevelDecl> {
    text.lines()
        .enumerate()
        .filter_map(|(ln, line_text)| top_level_decl(ln as u32, line_text))
        .collect()
}

fn top_level_decl(line: u32, line_text: &str) -> Option<TopLevelDecl> {
    // The keyword must sit at column 0 (no leading whitespace).
    if line_text.starts_with(|c: char| c.is_whitespace()) {
        return None;
    }
    let mut rest = line_text;
    loop {
        let word_end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let word = &rest[..word_end];
        let after = &rest[word_end..];
        // A modifier/keyword must be followed by whitespace (then more tokens).
        let trimmed = after.trim_start();
        if after.len() == trimmed.len() {
            return None;
        }
        if KEYWORDS.contains(&word) {
            let name = take_identifier(trimmed)?;
            // UTF-16 offset of the name within the whole line.
            let name_byte_start = line_text.len() - trimmed.len();
            let start_u16 = utf16_len(&line_text[..name_byte_start]);
            let end_u16 = start_u16 + utf16_len(name);
            return Some(TopLevelDecl {
                name: name.to_string(),
                keyword: word.to_string(),
                span: Span::new(line, start_u16, line, end_u16),
            });
        }
        if !MODIFIERS.contains(&word) {
            return None;
        }
        rest = trimmed;
    }
}

/// The leading identifier of `s` (`[A-Za-z_][A-Za-z0-9_$]*`), or `None`.
fn take_identifier(s: &str) -> Option<&str> {
    let mut end = 0;
    for (i, c) in s.char_indices() {
        let ok = if i == 0 {
            c.is_ascii_alphabetic() || c == '_'
        } else {
            c.is_ascii_alphanumeric() || c == '_' || c == '$'
        };
        if ok {
            end = i + c.len_utf8();
        } else {
            break;
        }
    }
    (end > 0).then(|| &s[..end])
}

fn utf16_len(s: &str) -> u32 {
    s.encode_utf16().count() as u32
}

/// The boxed [`DirtyBufferOverlay`] installed into the orchestrator. A thin
/// newtype over the shared [`PcOverlayInner`] so the ready services can retain a
/// second `Arc` handle to the same environment.
pub struct PcOverlay {
    inner: Arc<PcOverlayInner>,
}

impl PcOverlay {
    /// A fresh, uninstalled overlay. `handle()` yields the `Arc` the ready
    /// services keep for [`install`](PcOverlayInner::install) and
    /// [`pc_only_symbols`](PcOverlayInner::pc_only_symbols).
    pub fn new() -> PcOverlay {
        PcOverlay {
            inner: Arc::new(PcOverlayInner::new()),
        }
    }

    pub fn handle(&self) -> Arc<PcOverlayInner> {
        Arc::clone(&self.inner)
    }
}

impl Default for PcOverlay {
    fn default() -> Self {
        PcOverlay::new()
    }
}

impl DirtyBufferOverlay for PcOverlay {
    fn is_dirty(&self, uri: &str) -> bool {
        self.inner.is_dirty(uri)
    }

    fn symbol_at(&self, uri: &str, line: u32, character: u32) -> Option<OverlayHit> {
        self.inner.symbol_at(uri, line, character)
    }

    fn occurrences_of(&self, _semantic_symbol: &str) -> Option<Vec<Loc>> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use serde_json::Value;

    use crate::pc::{PcDefLocation, PcDefinition, PcLocation, PcSpan};

    /// A fake PC seam: a set of open buffers, a scripted prepareRename span, and a
    /// scripted definition result.
    #[derive(Default)]
    struct FakePc {
        open: HashSet<String>,
        prepare: Option<PcSpan>,
        definition: PcDefinition,
    }

    impl PcQueryService for FakePc {
        fn did_open(&self, _target_id: &str, _uri: &str, _text: &str) {}
        fn did_change(&self, _uri: &str, _text: &str) {}
        fn did_close(&self, _uri: &str) {}
        fn is_open(&self, uri: &str) -> bool {
            self.open.contains(uri)
        }
        fn definition(&self, _uri: &str, _line: u32, _character: u32) -> Vec<PcLocation> {
            Vec::new()
        }
        fn type_definition(&self, _uri: &str, _line: u32, _character: u32) -> Vec<PcLocation> {
            Vec::new()
        }
        fn completion(&self, _uri: &str, _line: u32, _character: u32) -> Value {
            Value::Null
        }
        fn hover(&self, _uri: &str, _line: u32, _character: u32) -> Value {
            Value::Null
        }
        fn signature_help(&self, _uri: &str, _line: u32, _character: u32) -> Value {
            Value::Null
        }
        fn prepare_rename(&self, _uri: &str, _line: u32, _character: u32) -> Option<PcSpan> {
            self.prepare.clone()
        }
        fn definition_result(&self, _uri: &str, _line: u32, _character: u32) -> PcDefinition {
            self.definition.clone()
        }
        fn is_registered(&self, _target_id: &str) -> bool {
            true
        }
        fn resolve_completion_item(&self, _target_id: &str, _symbol: &str, item: &Value) -> Value {
            item.clone()
        }
    }

    fn pc_span(sl: u32, sc: u32, el: u32, ec: u32) -> PcSpan {
        PcSpan {
            start_line: sl,
            start_character: sc,
            end_line: el,
            end_character: ec,
        }
    }

    fn def_loc(uri: &str, span: PcSpan, origin: PcDefOrigin) -> PcDefLocation {
        PcDefLocation {
            uri: uri.to_string(),
            span,
            origin,
        }
    }

    /// Build an installed overlay. `to_file_uri` is the identity (in these tests
    /// the SemanticDB URI and the file URI coincide); `indexed` names answer the
    /// `is_indexed_name` membership check.
    fn installed(
        docs: Arc<DocumentStore>,
        pc: FakePc,
        indexed: Vec<&str>,
    ) -> (PcOverlay, Arc<PcOverlayInner>) {
        let overlay = PcOverlay::new();
        let handle = overlay.handle();
        let indexed: HashSet<String> = indexed.into_iter().map(str::to_string).collect();
        handle.install(
            docs,
            Arc::new(pc),
            Box::new(|sdb: &str| Some(sdb.to_string())),
            Box::new(move |name: &str| indexed.contains(name)),
        );
        (overlay, handle)
    }

    // A `file://` URI with no file on disk reads dirty (disk text is `None`).
    const URI: &str = "file:///ws/A.scala";

    #[test]
    fn uninstalled_overlay_is_never_dirty_and_answers_nothing() {
        let overlay = PcOverlay::new();
        assert!(!overlay.is_dirty(URI));
        assert_eq!(overlay.symbol_at(URI, 0, 0), None);
        assert!(!overlay.handle().is_installed());
    }

    #[test]
    fn a_dirty_open_buffer_is_dirty_a_closed_one_is_not() {
        let docs = Arc::new(DocumentStore::new());
        docs.open(URI, "object A");
        let pc = FakePc {
            open: [URI.to_string()].into_iter().collect(),
            ..FakePc::default()
        };
        let (overlay, _h) = installed(Arc::clone(&docs), pc, vec!["A"]);
        assert!(
            overlay.is_dirty(URI),
            "an absent-on-disk open buffer is dirty"
        );
        assert!(
            !overlay.is_dirty("file:///ws/Other.scala"),
            "an unopened uri is never dirty"
        );
    }

    #[test]
    fn pc_hit_reads_the_symbol_from_definition_and_span_from_prepare_rename() {
        let docs = Arc::new(DocumentStore::new());
        docs.open(URI, "  foo.bar\n");
        let pc = FakePc {
            open: [URI.to_string()].into_iter().collect(),
            prepare: Some(pc_span(0, 6, 0, 9)),
            definition: PcDefinition {
                symbol: "pkg/Bar#bar().".to_string(),
                locations: vec![def_loc(
                    "file:///ws/Bar.scala",
                    pc_span(3, 6, 3, 9),
                    PcDefOrigin::Workspace,
                )],
            },
        };
        let (overlay, _h) = installed(docs, pc, vec![]);
        let hit = overlay.symbol_at(URI, 0, 7).expect("a PC hit");
        assert_eq!(hit.semantic_symbol, "pkg/Bar#bar().");
        assert_eq!(hit.span, Span::new(0, 6, 0, 9));
        // The def resolves elsewhere (not this span in this file), so Reference.
        assert_eq!(hit.role, Role::Reference);
        assert!(!hit.pc_only);
    }

    #[test]
    fn pc_hit_is_a_definition_when_the_def_resolves_to_the_same_span_here() {
        let docs = Arc::new(DocumentStore::new());
        docs.open(URI, "def foo = 1\n");
        let pc = FakePc {
            open: [URI.to_string()].into_iter().collect(),
            prepare: Some(pc_span(0, 4, 0, 7)),
            definition: PcDefinition {
                symbol: "pkg/A.foo().".to_string(),
                locations: vec![def_loc(URI, pc_span(0, 4, 0, 7), PcDefOrigin::Workspace)],
            },
        };
        // `foo` is indexed, so the PC-only top-level branch is suppressed and the
        // normal PC hit resolves.
        let (overlay, _h) = installed(docs, pc, vec!["foo"]);
        let hit = overlay.symbol_at(URI, 0, 5).expect("a PC hit");
        assert_eq!(hit.role, Role::Definition);
        assert!(!hit.pc_only);
    }

    #[test]
    fn pc_hit_falls_back_to_the_buffer_token_span_when_prepare_rename_declines() {
        let docs = Arc::new(DocumentStore::new());
        docs.open(URI, "val alpha = beta\n");
        let pc = FakePc {
            open: [URI.to_string()].into_iter().collect(),
            prepare: None, // dotty declines: fall back to the identifier token
            definition: PcDefinition {
                symbol: "pkg/A.beta.".to_string(),
                locations: vec![def_loc(
                    "file:///ws/B.scala",
                    pc_span(1, 0, 1, 4),
                    PcDefOrigin::Workspace,
                )],
            },
        };
        let (overlay, _h) = installed(docs, pc, vec![]);
        // Cursor on `beta` (columns 12..16).
        let hit = overlay.symbol_at(URI, 0, 13).expect("a token-span PC hit");
        assert_eq!(hit.span, Span::new(0, 12, 0, 16));
        assert_eq!(hit.semantic_symbol, "pkg/A.beta.");
    }

    #[test]
    fn pc_only_when_every_definition_origin_is_non_workspace() {
        let docs = Arc::new(DocumentStore::new());
        docs.open(URI, "given x = 1\n");
        let pc = FakePc {
            open: [URI.to_string()].into_iter().collect(),
            prepare: Some(pc_span(0, 6, 0, 7)),
            definition: PcDefinition {
                symbol: "synthetic/x.".to_string(),
                locations: vec![
                    def_loc(
                        "file:///ws/S.scala",
                        pc_span(0, 0, 0, 1),
                        PcDefOrigin::Synthetic,
                    ),
                    def_loc(
                        "file:///ws/P.scala",
                        pc_span(0, 0, 0, 1),
                        PcDefOrigin::Plugin,
                    ),
                ],
            },
        };
        let (overlay, _h) = installed(docs, pc, vec![]);
        let hit = overlay.symbol_at(URI, 0, 6).expect("a PC hit");
        assert!(hit.pc_only, "all origins non-workspace => pc_only");
    }

    #[test]
    fn an_empty_symbol_degrades_to_none() {
        let docs = Arc::new(DocumentStore::new());
        docs.open(URI, "foo\n");
        let pc = FakePc {
            open: [URI.to_string()].into_iter().collect(),
            prepare: Some(pc_span(0, 0, 0, 3)),
            definition: PcDefinition::default(), // empty symbol
        };
        let (overlay, _h) = installed(docs, pc, vec![]);
        assert_eq!(
            overlay.symbol_at(URI, 0, 1),
            None,
            "empty symbol => degrade"
        );
    }

    #[test]
    fn a_buffer_the_pc_does_not_hold_answers_nothing() {
        let docs = Arc::new(DocumentStore::new());
        docs.open(URI, "foo\n");
        // PC `is_open` is false (not in the set), so symbol_at short-circuits.
        let (overlay, _h) = installed(docs, FakePc::default(), vec![]);
        assert_eq!(overlay.symbol_at(URI, 0, 1), None);
    }

    #[test]
    fn a_top_level_declaration_the_index_has_not_seen_is_a_pc_only_hit() {
        let docs = Arc::new(DocumentStore::new());
        docs.open(URI, "object Fresh:\n  def x = 1\n");
        let pc = FakePc {
            open: [URI.to_string()].into_iter().collect(),
            ..FakePc::default()
        };
        // `Fresh` is NOT in the index.
        let (overlay, _h) = installed(docs, pc, vec![]);
        // Cursor on `Fresh` (columns 7..12).
        let hit = overlay
            .symbol_at(URI, 0, 9)
            .expect("a PC-only top-level hit");
        assert!(hit.pc_only);
        assert_eq!(hit.role, Role::Definition);
        assert_eq!(hit.span, Span::new(0, 7, 0, 12));
    }

    #[test]
    fn a_top_level_declaration_already_in_the_index_falls_through_to_the_pc_hit() {
        let docs = Arc::new(DocumentStore::new());
        docs.open(URI, "object Known:\n");
        let pc = FakePc {
            open: [URI.to_string()].into_iter().collect(),
            prepare: Some(pc_span(0, 7, 0, 12)),
            definition: PcDefinition {
                symbol: "pkg/Known.".to_string(),
                locations: vec![def_loc(URI, pc_span(0, 7, 0, 12), PcDefOrigin::Workspace)],
            },
        };
        // `Known` IS indexed, so the PC-only branch is suppressed.
        let (overlay, _h) = installed(docs, pc, vec!["Known"]);
        let hit = overlay.symbol_at(URI, 0, 9).expect("the PC hit");
        assert!(!hit.pc_only);
        assert_eq!(hit.semantic_symbol, "pkg/Known.");
    }

    #[test]
    fn pc_only_symbols_lists_unindexed_dirty_top_level_decls_matching_the_query() {
        let docs = Arc::new(DocumentStore::new());
        docs.open(URI, "final case class Widget(x: Int)\nobject Known\n");
        let (_overlay, handle) = installed(Arc::clone(&docs), FakePc::default(), vec!["Known"]);
        // `Widget` is unindexed and matches; `Known` is indexed and suppressed.
        let hits = handle.pc_only_symbols("wid");
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Widget");
        assert_eq!(hits[0].keyword, "class");
        assert_eq!(hits[0].span, Span::new(0, 17, 0, 23));
        // A non-matching query returns nothing.
        assert!(handle.pc_only_symbols("zzz").is_empty());
        // An empty query returns every unindexed decl.
        assert_eq!(handle.pc_only_symbols("").len(), 1);
    }

    #[test]
    fn pc_only_symbols_ignores_clean_buffers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("A.scala");
        std::fs::write(&path, "object Saved").unwrap();
        let uri = ls_index_model::uri::path_to_uri(&path);
        let docs = Arc::new(DocumentStore::new());
        docs.open(&uri, "object Saved"); // buffer equals disk => clean
        let (_overlay, handle) = installed(docs, FakePc::default(), vec![]);
        assert!(
            handle.pc_only_symbols("Saved").is_empty(),
            "a clean buffer contributes no PC-only symbols"
        );
    }

    #[test]
    fn top_level_decls_parses_column_zero_declarations_with_modifiers() {
        let text =
            "private final class Foo\n  def nested = 1\nobject Bar\ntype T = Int\nplain text\n";
        let decls = top_level_decls(text);
        let got: Vec<(&str, &str, u32, u32)> = decls
            .iter()
            .map(|d| {
                (
                    d.keyword.as_str(),
                    d.name.as_str(),
                    d.span.start_char,
                    d.span.end_char,
                )
            })
            .collect();
        assert_eq!(
            got,
            vec![
                ("class", "Foo", 20, 23), // after "private final "
                ("object", "Bar", 7, 10),
                ("type", "T", 5, 6),
            ],
            "indented members and non-declarations are skipped"
        );
    }

    // The overlay lives INSIDE the orchestrator: a dirty buffer routes
    // `symbol_at_cursor` through `is_dirty` -> `symbol_at`, so a PC-only hit
    // reaches the engine as a `pc_only` `CursorSymbol` (which references/rename
    // then exclude/reject). Proves the production wiring, not just the SPI.
    #[test]
    fn an_installed_overlay_drives_symbol_at_cursor_over_a_dirty_buffer() {
        use ls_engine::QueryOrchestrator;
        use ls_store::Store;

        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let overlay = PcOverlay::new();
        let handle = overlay.handle();
        let orchestrator = QueryOrchestrator::new(store, Box::new(overlay), true);

        let docs = Arc::new(DocumentStore::new());
        docs.open(URI, "object Fresh:\n  def x = 1\n"); // absent on disk => dirty
        let pc = FakePc {
            open: [URI.to_string()].into_iter().collect(),
            ..FakePc::default()
        };
        // `Fresh` is unindexed, so the cursor on it is a PC-only definition.
        handle.install(
            docs,
            Arc::new(pc),
            Box::new(|sdb: &str| Some(sdb.to_string())),
            Box::new(|_name: &str| false),
        );

        let cursor = orchestrator
            .symbol_at_cursor(URI, 0, 9)
            .expect("the dirty overlay answers the cursor");
        assert!(cursor.pc_only, "an unindexed top-level decl is PC-only");
        assert_eq!(cursor.role, Role::Definition);
        assert_eq!(cursor.span, Span::new(0, 7, 0, 12));
    }

    #[test]
    fn token_span_at_finds_the_identifier_under_the_cursor() {
        let docs = DocumentStore::new();
        docs.open(URI, "  val name = 1\n");
        // Cursor inside `name` (columns 6..10).
        assert_eq!(
            token_span_at(&docs, URI, 0, 8),
            Some(Span::new(0, 6, 0, 10))
        );
        // Cursor on whitespace flanked by `=` and `1`: no identifier token.
        assert_eq!(token_span_at(&docs, URI, 0, 12), None);
    }
}
