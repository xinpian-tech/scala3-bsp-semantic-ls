//! PC live-typing diagnostics: the per-URI merge layer next to the
//! [`DiagnosticRouter`](crate::diagnostics::DiagnosticRouter) and the
//! debounced background pull that feeds it.
//!
//! # The merge policy
//!
//! BSP compile diagnostics remain PRIMARY. The presentation compiler's
//! secondary diagnostics (the island's `pc_diagnostics` op — the facade
//! `didChange` push over the mirrored buffer) publish under the distinct
//! source tag [`PC_DIAGNOSTICS_SOURCE`], and ONLY for an open, dirty buffer —
//! the state BSP knows nothing about. Because an LSP publish REPLACES a URI's
//! whole diagnostic set, both streams flow through one [`PcDiagnosticsLayer`],
//! which remembers the last BSP set and the last PC set per URI and always
//! publishes their union (BSP first):
//!
//! - a routed BSP publish ([`PcDiagnosticsLayer::bsp_published`]) records the
//!   BSP set, DROPS the URI's PC overlay (the compiler just spoke about the
//!   saved file; stale typing diagnostics must not linger next to it), and
//!   forwards the BSP set unchanged;
//! - a completed PC pull ([`PcDiagnosticsLayer::set_pc`]) replaces the URI's
//!   PC overlay and publishes `BSP ++ PC(tagged)`;
//! - `didSave` / `didClose` clear the overlay
//!   ([`PcDiagnosticsLayer::clear_pc`]) — the buffer is no longer dirty (or no
//!   longer open), so BSP truth alone applies again;
//! - an empty union publishes an empty list exactly once per non-empty
//!   predecessor (the router's clear-once discipline, applied to the merged
//!   stream).
//!
//! The router's own reset/suppression semantics are untouched: this layer sits
//! BEHIND `DiagnosticRouter::accept` and only sees the publishes the router
//! already decided to emit. (Consequence: a BSP clear the router suppresses —
//! a file it never published non-empty — does not reach the layer and so does
//! not clear a PC overlay; only a real BSP publish supersedes typing
//! diagnostics.)
//!
//! # The debounced pull
//!
//! [`PcDiagnosticsScheduler`] is the `didChange`-side producer, following the
//! [`BuildScheduler`](crate::build_scheduler::BuildScheduler) debounce
//! pattern: `on_did_change` (ready path, after the PC mirror update) schedules
//! the changed URI; a worker thread pulls `pc_diagnostics` after a fixed
//! per-URI debounce window and hands the result to the layer — the serve loop
//! never blocks on a pull. The job is per-URI last-write-wins: edits landing
//! inside an armed window coalesce into the one pending pull, which reads the
//! NEWEST mirrored text when it finally runs.
//!
//! The pull NEVER boots the embedded JVM ([`PcQueryService::booted`] gates
//! it): typing alone keeps an index-only session zero-JVM; live diagnostics
//! activate once a real PC query (hover, completion, semantic tokens, …) has
//! booted the island. A pull for a buffer that is no longer open or no longer
//! dirty clears the overlay instead of publishing; the open/dirty check runs
//! again after the (possibly slow) PC call, so a save or close racing a pull
//! at worst leaves a window of one already-in-flight publish that the next
//! save/close/BSP publish supersedes.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ls_pc_abi::payloads::PcDiagnostic;

use crate::documents::DocumentStore;
use crate::pc::PcQueryService;
use crate::protocol::{Diagnostic, DiagnosticCode, Position, PublishDiagnosticsParams, Range};

/// The source tag every PC live-typing diagnostic carries, distinguishing it
/// from BSP compile diagnostics in the editor's UI.
pub const PC_DIAGNOSTICS_SOURCE: &str = "scala3-pc (typing)";

/// The fixed debounce window between a `didChange` and its diagnostics pull.
/// Shorter than the build scheduler's 500ms save debounce — typing feedback is
/// the point — but wide enough to coalesce a burst of keystrokes.
pub(crate) const PC_DIAGNOSTICS_DEBOUNCE: Duration = Duration::from_millis(300);

/// How the merged publishes leave the layer — `main` wires the shared
/// [`OutputSink`](crate::server::OutputSink); tests capture.
pub type DiagnosticsPublisher = Arc<dyn Fn(&PublishDiagnosticsParams) + Send + Sync>;

/// One island PC diagnostic -> the LSP shape, tagged [`PC_DIAGNOSTICS_SOURCE`].
/// The carrier's range is already zero-based UTF-16 line/character; severity
/// shares the LSP 1..=4 integers (anything else is dropped to unset, like the
/// BSP converter's defensive branch); an empty `code` omits the field.
pub fn to_lsp_diagnostic(d: &PcDiagnostic) -> Diagnostic {
    Diagnostic {
        range: Range {
            start: Position {
                line: d.range.start_line,
                character: d.range.start_character,
            },
            end: Position {
                line: d.range.end_line,
                character: d.range.end_character,
            },
        },
        severity: match d.severity {
            1..=4 => Some(d.severity as u32),
            _ => None,
        },
        code: if d.code.is_empty() {
            None
        } else {
            Some(DiagnosticCode::String(d.code.clone()))
        },
        source: Some(PC_DIAGNOSTICS_SOURCE.to_string()),
        message: d.message.clone(),
    }
}

/// The per-URI BSP/PC merge state plus the outbound publisher. Shared between
/// the BSP session's reader thread (`bsp_published`) and the scheduler's pull
/// worker (`set_pc`/`clear_pc`); the internal mutex is held across the publish
/// call so merged snapshots leave in a consistent order (the sink has its own
/// frame lock, and the publisher never calls back into the layer).
pub struct PcDiagnosticsLayer {
    publish: DiagnosticsPublisher,
    state: Mutex<LayerState>,
}

#[derive(Default)]
struct LayerState {
    /// The last BSP publish per file URI (absent == BSP last published empty
    /// or never published).
    bsp: HashMap<String, Vec<Diagnostic>>,
    /// The PC overlay per file URI — present only for an open dirty buffer.
    pc: HashMap<String, Vec<Diagnostic>>,
    /// URIs whose MERGED stream has published non-empty, so an empty union is
    /// forwarded exactly once (and a never-published URI stays silent).
    published_non_empty: HashSet<String>,
}

impl PcDiagnosticsLayer {
    pub fn new(publish: DiagnosticsPublisher) -> Arc<PcDiagnosticsLayer> {
        Arc::new(PcDiagnosticsLayer {
            publish,
            state: Mutex::new(LayerState::default()),
        })
    }

    /// A layer whose publishes go nowhere — the default for injected/test
    /// bundles that wire no client sink.
    pub fn disconnected() -> Arc<PcDiagnosticsLayer> {
        PcDiagnosticsLayer::new(Arc::new(|_| {}))
    }

    /// A publish the [`DiagnosticRouter`](crate::diagnostics::DiagnosticRouter)
    /// decided to emit: record the URI's BSP set, DROP its PC overlay (the
    /// build compiler's word supersedes typing diagnostics), and forward.
    pub fn bsp_published(&self, publish: PublishDiagnosticsParams) {
        let mut state = self.state.lock().expect("pc diagnostics layer mutex");
        if publish.diagnostics.is_empty() {
            state.bsp.remove(&publish.uri);
        } else {
            state
                .bsp
                .insert(publish.uri.clone(), publish.diagnostics.clone());
        }
        state.pc.remove(&publish.uri);
        self.emit(&mut state, &publish.uri);
    }

    /// A completed pull for an open dirty buffer: replace the URI's PC overlay
    /// (already converted + tagged) and publish the merged set.
    pub fn set_pc(&self, uri: &str, diagnostics: Vec<Diagnostic>) {
        let mut state = self.state.lock().expect("pc diagnostics layer mutex");
        if diagnostics.is_empty() {
            // No overlay before and none now: nothing changed, publish nothing.
            if state.pc.remove(uri).is_none() {
                return;
            }
        } else {
            state.pc.insert(uri.to_string(), diagnostics);
        }
        self.emit(&mut state, uri);
    }

    /// `didSave`/`didClose` (or a pull that found the buffer clean/closed):
    /// drop the URI's PC overlay, republishing the BSP-only set if an overlay
    /// was actually showing.
    pub fn clear_pc(&self, uri: &str) {
        self.set_pc(uri, Vec::new());
    }

    /// Publish the URI's current `BSP ++ PC` union, with the clear-once
    /// discipline on the merged stream.
    fn emit(&self, state: &mut LayerState, uri: &str) {
        let mut union: Vec<Diagnostic> = state.bsp.get(uri).cloned().unwrap_or_default();
        union.extend(state.pc.get(uri).iter().flat_map(|d| d.iter().cloned()));
        if !union.is_empty() {
            state.published_non_empty.insert(uri.to_string());
        } else if !state.published_non_empty.remove(uri) {
            return;
        }
        (self.publish)(&PublishDiagnosticsParams {
            uri: uri.to_string(),
            diagnostics: union,
        });
    }
}

/// Shared between the message loop (which schedules/cancels) and the pull
/// worker, behind one mutex + condvar.
struct Shared {
    /// Armed pulls: URI -> the instant its fixed debounce window elapses. A
    /// URI already armed keeps its window (edits coalesce; the pull reads the
    /// newest text when it runs).
    due: BTreeMap<String, Instant>,
    /// The open-buffer store, installed at Ready adoption
    /// (`CoreServices::install_pc_overlay`) — the dirty check's source.
    docs: Option<Arc<DocumentStore>>,
    shutting_down: bool,
    /// Completed pulls, for deterministic tests.
    pulls_completed: u64,
}

/// The debounced, per-URI last-write-wins `pc_diagnostics` pull worker. Owned
/// by the ready services; dropping it stops and joins the thread.
pub(crate) struct PcDiagnosticsScheduler {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    debounce: Duration,
    handle: Option<JoinHandle<()>>,
}

impl PcDiagnosticsScheduler {
    pub(crate) fn new(
        pc: Arc<dyn PcQueryService>,
        layer: Arc<PcDiagnosticsLayer>,
    ) -> PcDiagnosticsScheduler {
        PcDiagnosticsScheduler::with_debounce(pc, layer, PC_DIAGNOSTICS_DEBOUNCE)
    }

    pub(crate) fn with_debounce(
        pc: Arc<dyn PcQueryService>,
        layer: Arc<PcDiagnosticsLayer>,
        debounce: Duration,
    ) -> PcDiagnosticsScheduler {
        let shared = Arc::new((
            Mutex::new(Shared {
                due: BTreeMap::new(),
                docs: None,
                shutting_down: false,
                pulls_completed: 0,
            }),
            Condvar::new(),
        ));
        let worker_shared = Arc::clone(&shared);
        let handle = thread::spawn(move || worker_loop(&worker_shared, &pc, &layer));
        PcDiagnosticsScheduler {
            shared,
            debounce,
            handle: Some(handle),
        }
    }

    /// Bind the shared document store (the dirty check's source). Called at
    /// Ready adoption; a pull before this is skipped.
    pub(crate) fn install_docs(&self, docs: Arc<DocumentStore>) {
        self.shared.0.lock().unwrap().docs = Some(docs);
    }

    /// Arm (or coalesce into) the URI's debounced pull. Never blocks.
    pub(crate) fn schedule(&self, uri: &str) {
        let (lock, cvar) = &*self.shared;
        let mut shared = lock.lock().unwrap();
        if shared.shutting_down || shared.due.contains_key(uri) {
            return;
        }
        shared
            .due
            .insert(uri.to_string(), Instant::now() + self.debounce);
        cvar.notify_all();
    }

    /// Disarm a pending pull (`didSave`/`didClose` — the overlay is being
    /// cleared, so a queued pull must not re-add it).
    pub(crate) fn cancel(&self, uri: &str) {
        self.shared.0.lock().unwrap().due.remove(uri);
    }

    /// Test-only: block until at least `n` pulls completed, or `timeout`.
    #[cfg(test)]
    pub(crate) fn wait_for_pulls(&self, n: u64, timeout: Duration) -> u64 {
        let (lock, cvar) = &*self.shared;
        let mut shared = lock.lock().unwrap();
        let deadline = Instant::now() + timeout;
        while shared.pulls_completed < n {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let (guard, _) = cvar.wait_timeout(shared, remaining).unwrap();
            shared = guard;
        }
        shared.pulls_completed
    }
}

impl Drop for PcDiagnosticsScheduler {
    fn drop(&mut self) {
        {
            let (lock, cvar) = &*self.shared;
            let mut shared = lock.lock().unwrap();
            shared.shutting_down = true;
            cvar.notify_all();
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn worker_loop(
    shared: &Arc<(Mutex<Shared>, Condvar)>,
    pc: &Arc<dyn PcQueryService>,
    layer: &Arc<PcDiagnosticsLayer>,
) {
    let (lock, cvar) = &**shared;
    loop {
        let mut guard = lock.lock().unwrap();
        // Wait for an armed pull (or shutdown).
        while !guard.shutting_down && guard.due.is_empty() {
            guard = cvar.wait(guard).unwrap();
        }
        if guard.shutting_down {
            return;
        }
        // The earliest-due URI; sleep out its remaining window (waking early on
        // shutdown or a cancel emptying the map).
        let Some((uri, due)) = guard
            .due
            .iter()
            .min_by_key(|(_, due)| **due)
            .map(|(uri, due)| (uri.clone(), *due))
        else {
            continue;
        };
        if let Some(remaining) = due.checked_duration_since(Instant::now()) {
            let (next, _) = cvar.wait_timeout(guard, remaining).unwrap();
            guard = next;
            continue; // re-evaluate: shutdown/cancel/an earlier arrival
        }
        // A cancel may have raced the wake; only a still-armed URI is pulled.
        if guard.due.remove(&uri).is_none() {
            continue;
        }
        let docs = guard.docs.clone();
        drop(guard);

        pull(pc, layer, docs.as_deref(), &uri);

        let mut guard = lock.lock().unwrap();
        guard.pulls_completed += 1;
        cvar.notify_all();
    }
}

/// One pull: gate (no boot, no docs, closed or clean buffer clears instead),
/// query, re-check the gate (a save/close may have raced the query), publish.
fn pull(
    pc: &Arc<dyn PcQueryService>,
    layer: &Arc<PcDiagnosticsLayer>,
    docs: Option<&DocumentStore>,
    uri: &str,
) {
    // NEVER boots the island: typing alone keeps a session zero-JVM; live
    // diagnostics start once a real PC query has booted it.
    if !pc.booted() {
        return;
    }
    let Some(docs) = docs else {
        return;
    };
    let dirty_and_open = |docs: &DocumentStore| pc.is_open(uri) && docs.is_dirty(uri);
    if !dirty_and_open(docs) {
        layer.clear_pc(uri);
        return;
    }
    let diagnostics = pc.pc_diagnostics(uri);
    if !dirty_and_open(docs) {
        layer.clear_pc(uri);
        return;
    }
    layer.set_pc(uri, diagnostics.iter().map(to_lsp_diagnostic).collect());
}

#[cfg(test)]
mod tests {
    use super::*;
    use ls_pc_abi::payloads::Rng;
    use serde_json::json;
    use std::sync::Mutex as StdMutex;

    fn pc_diag(msg: &str, severity: i32, code: &str) -> Diagnostic {
        to_lsp_diagnostic(&PcDiagnostic {
            range: Rng {
                start_line: 1,
                start_character: 2,
                end_line: 1,
                end_character: 7,
            },
            severity,
            code: code.to_string(),
            message: msg.to_string(),
        })
    }

    fn bsp_diag(msg: &str) -> Diagnostic {
        Diagnostic {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: 0,
                    character: 4,
                },
            },
            severity: Some(1),
            code: None,
            source: Some("sc".to_string()),
            message: msg.to_string(),
        }
    }

    /// A layer over a capturing publisher; returns the layer and the captured
    /// publish log.
    fn capturing_layer() -> (
        Arc<PcDiagnosticsLayer>,
        Arc<StdMutex<Vec<PublishDiagnosticsParams>>>,
    ) {
        let published = Arc::new(StdMutex::new(Vec::new()));
        let sink = Arc::clone(&published);
        let layer = PcDiagnosticsLayer::new(Arc::new(move |publish: &PublishDiagnosticsParams| {
            sink.lock().unwrap().push(publish.clone());
        }));
        (layer, published)
    }

    fn messages(publish: &PublishDiagnosticsParams) -> Vec<(String, Option<String>)> {
        publish
            .diagnostics
            .iter()
            .map(|d| (d.message.clone(), d.source.clone()))
            .collect()
    }

    const URI: &str = "file:///ws/A.scala";

    // The carrier conversion: UTF-16 range verbatim, shared severity ints,
    // string code, and ALWAYS the distinct source tag.
    #[test]
    fn pc_diagnostics_convert_with_the_typing_source_tag() {
        let d = pc_diag("boom", 1, "E007");
        assert_eq!(
            serde_json::to_value(&d).unwrap(),
            json!({
                "range": {
                    "start": { "line": 1, "character": 2 },
                    "end": { "line": 1, "character": 7 }
                },
                "severity": 1,
                "code": "E007",
                "source": "scala3-pc (typing)",
                "message": "boom"
            })
        );
        // An out-of-enum severity drops to unset; an empty code omits.
        let odd = pc_diag("m", 0, "");
        assert_eq!(odd.severity, None);
        assert_eq!(odd.code, None);
        assert_eq!(odd.source.as_deref(), Some(PC_DIAGNOSTICS_SOURCE));
    }

    // The core merge: a PC set publishes on its own; a BSP set then publishes
    // FIRST in the union — but a BSP publish also DROPS the overlay, so the
    // union after a BSP publish is the BSP set alone until the next pull.
    #[test]
    fn pc_overlay_merges_after_bsp_and_bsp_publish_supersedes_it() {
        let (layer, published) = capturing_layer();
        layer.set_pc(URI, vec![pc_diag("typing", 1, "")]);
        layer.bsp_published(PublishDiagnosticsParams {
            uri: URI.to_string(),
            diagnostics: vec![bsp_diag("compile")],
        });
        // A fresh pull after the BSP publish merges BSP first, PC after.
        layer.set_pc(URI, vec![pc_diag("typing2", 2, "")]);
        let log = published.lock().unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(
            messages(&log[0]),
            vec![(
                "typing".to_string(),
                Some(PC_DIAGNOSTICS_SOURCE.to_string())
            )]
        );
        // The BSP publish dropped the PC overlay: BSP alone.
        assert_eq!(
            messages(&log[1]),
            vec![("compile".to_string(), Some("sc".to_string()))]
        );
        assert_eq!(
            messages(&log[2]),
            vec![
                ("compile".to_string(), Some("sc".to_string())),
                (
                    "typing2".to_string(),
                    Some(PC_DIAGNOSTICS_SOURCE.to_string())
                ),
            ]
        );
    }

    // clear_pc (didSave/didClose): republishes the BSP-only set when an overlay
    // was showing, and the clear-once discipline holds on the merged stream —
    // a second clear publishes nothing, and a never-published URI stays silent.
    #[test]
    fn clear_pc_republishes_bsp_only_once_and_a_clean_uri_stays_silent() {
        let (layer, published) = capturing_layer();
        layer.clear_pc(URI); // nothing showing: silent
        layer.set_pc(URI, vec![pc_diag("typing", 1, "")]);
        layer.clear_pc(URI); // overlay was showing: one empty publish
        layer.clear_pc(URI); // already clear: silent
        let log = published.lock().unwrap();
        assert_eq!(log.len(), 2, "{log:?}");
        assert_eq!(log[1].uri, URI);
        assert!(log[1].diagnostics.is_empty());
    }

    // With a BSP set standing, clearing the PC overlay republishes the BSP set
    // (not an empty list): compile truth stays on screen.
    #[test]
    fn clear_pc_keeps_the_standing_bsp_set() {
        let (layer, published) = capturing_layer();
        layer.bsp_published(PublishDiagnosticsParams {
            uri: URI.to_string(),
            diagnostics: vec![bsp_diag("compile")],
        });
        layer.set_pc(URI, vec![pc_diag("typing", 1, "")]);
        layer.clear_pc(URI);
        let log = published.lock().unwrap();
        assert_eq!(log.len(), 3);
        assert_eq!(
            messages(&log[2]),
            vec![("compile".to_string(), Some("sc".to_string()))]
        );
    }

    // An empty pull result over an empty overlay publishes nothing (the
    // steady-state of clean typing must not spam empty publishes).
    #[test]
    fn an_empty_pull_over_an_empty_overlay_is_silent() {
        let (layer, published) = capturing_layer();
        layer.set_pc(URI, Vec::new());
        layer.set_pc(URI, Vec::new());
        assert!(published.lock().unwrap().is_empty());
    }

    // A BSP clear (empty routed publish) also drops the overlay and forwards
    // the clear once.
    #[test]
    fn a_bsp_clear_drops_the_overlay_and_forwards_once() {
        let (layer, published) = capturing_layer();
        layer.set_pc(URI, vec![pc_diag("typing", 1, "")]);
        layer.bsp_published(PublishDiagnosticsParams {
            uri: URI.to_string(),
            diagnostics: Vec::new(),
        });
        let log = published.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert!(log[1].diagnostics.is_empty());
    }

    // --- the scheduler ------------------------------------------------------

    use ls_pc_abi::payloads::TargetConfig;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    /// A scriptable PC seam for the scheduler: booted-ness is togglable, the
    /// buffer set is explicit, and every `pc_diagnostics` call is counted.
    #[derive(Default)]
    struct SchedFakePc {
        booted: AtomicBool,
        open: StdMutex<HashSet<String>>,
        pulls: AtomicUsize,
    }

    impl PcQueryService for SchedFakePc {
        fn did_open(&self, _t: &str, uri: &str, _x: &str) {
            self.open.lock().unwrap().insert(uri.to_string());
        }
        fn did_change(&self, _u: &str, _x: &str) {}
        fn did_close(&self, uri: &str) {
            self.open.lock().unwrap().remove(uri);
        }
        fn is_open(&self, uri: &str) -> bool {
            self.open.lock().unwrap().contains(uri)
        }
        fn booted(&self) -> bool {
            self.booted.load(Ordering::SeqCst)
        }
        fn definition(&self, _u: &str, _l: u32, _c: u32) -> Vec<crate::pc::PcLocation> {
            Vec::new()
        }
        fn type_definition(&self, _u: &str, _l: u32, _c: u32) -> Vec<crate::pc::PcLocation> {
            Vec::new()
        }
        fn completion(&self, _u: &str, _l: u32, _c: u32) -> serde_json::Value {
            serde_json::Value::Null
        }
        fn hover(&self, _u: &str, _l: u32, _c: u32) -> serde_json::Value {
            serde_json::Value::Null
        }
        fn signature_help(&self, _u: &str, _l: u32, _c: u32) -> serde_json::Value {
            serde_json::Value::Null
        }
        fn is_registered(&self, _t: &str) -> bool {
            true
        }
        fn resolve_completion_item(
            &self,
            _t: &str,
            _s: &str,
            item: &serde_json::Value,
        ) -> serde_json::Value {
            item.clone()
        }
        fn reconfigure_targets(&self, _targets: Vec<TargetConfig>) {}
        fn pc_diagnostics(&self, uri: &str) -> Vec<PcDiagnostic> {
            self.pulls.fetch_add(1, Ordering::SeqCst);
            vec![PcDiagnostic {
                range: Rng::default(),
                severity: 1,
                code: String::new(),
                message: format!("typing diagnostic for {uri}"),
            }]
        }
    }

    /// A dirty open buffer: the store holds text for a URI whose file does not
    /// exist on disk (a missing file reads dirty by definition).
    fn dirty_docs(uri: &str) -> Arc<DocumentStore> {
        let docs = Arc::new(DocumentStore::new());
        docs.open(uri, "class A\n");
        docs
    }

    fn sched(
        pc: &Arc<SchedFakePc>,
        layer: &Arc<PcDiagnosticsLayer>,
        docs: Option<Arc<DocumentStore>>,
    ) -> PcDiagnosticsScheduler {
        let scheduler = PcDiagnosticsScheduler::with_debounce(
            Arc::clone(pc) as Arc<dyn PcQueryService>,
            Arc::clone(layer),
            Duration::ZERO,
        );
        if let Some(docs) = docs {
            scheduler.install_docs(docs);
        }
        scheduler
    }

    // The happy path: booted island + open dirty buffer -> the debounced pull
    // publishes the tagged overlay.
    #[test]
    fn a_scheduled_pull_publishes_the_tagged_overlay() {
        let pc = Arc::new(SchedFakePc::default());
        pc.booted.store(true, Ordering::SeqCst);
        pc.did_open("t", URI, "x");
        let (layer, published) = capturing_layer();
        let scheduler = sched(&pc, &layer, Some(dirty_docs(URI)));
        scheduler.schedule(URI);
        assert_eq!(scheduler.wait_for_pulls(1, Duration::from_secs(5)), 1);
        let log = published.lock().unwrap();
        assert_eq!(log.len(), 1);
        assert_eq!(log[0].diagnostics.len(), 1);
        assert_eq!(
            log[0].diagnostics[0].source.as_deref(),
            Some(PC_DIAGNOSTICS_SOURCE)
        );
    }

    // The no-boot gate: a cold island's pull is skipped entirely — no PC call,
    // no publish. Typing alone never boots the JVM.
    #[test]
    fn a_pull_against_a_cold_island_is_skipped_without_querying() {
        let pc = Arc::new(SchedFakePc::default());
        pc.did_open("t", URI, "x");
        let (layer, published) = capturing_layer();
        let scheduler = sched(&pc, &layer, Some(dirty_docs(URI)));
        scheduler.schedule(URI);
        assert_eq!(scheduler.wait_for_pulls(1, Duration::from_secs(5)), 1);
        assert_eq!(pc.pulls.load(Ordering::SeqCst), 0);
        assert!(published.lock().unwrap().is_empty());
    }

    // Rapid schedules for one URI coalesce into the one armed pull
    // (last-write-wins: the pull reads the newest mirror when it runs).
    #[test]
    fn rapid_schedules_for_one_uri_coalesce() {
        let pc = Arc::new(SchedFakePc::default());
        pc.booted.store(true, Ordering::SeqCst);
        pc.did_open("t", URI, "x");
        let (layer, _published) = capturing_layer();
        let scheduler = PcDiagnosticsScheduler::with_debounce(
            Arc::clone(&pc) as Arc<dyn PcQueryService>,
            Arc::clone(&layer),
            Duration::from_millis(80),
        );
        scheduler.install_docs(dirty_docs(URI));
        scheduler.schedule(URI);
        scheduler.schedule(URI);
        scheduler.schedule(URI);
        assert_eq!(scheduler.wait_for_pulls(1, Duration::from_secs(5)), 1);
        assert_eq!(pc.pulls.load(Ordering::SeqCst), 1);
        // No second pull is armed.
        assert_eq!(scheduler.wait_for_pulls(2, Duration::from_millis(300)), 1);
    }

    // A pull whose buffer is no longer dirty (or no longer open) CLEARS the
    // overlay instead of publishing typing diagnostics for a clean buffer.
    #[test]
    fn a_pull_over_a_clean_buffer_clears_the_overlay() {
        let pc = Arc::new(SchedFakePc::default());
        pc.booted.store(true, Ordering::SeqCst);
        pc.did_open("t", URI, "x");
        let (layer, published) = capturing_layer();
        layer.set_pc(URI, vec![pc_diag("stale", 1, "")]);
        // The docs store does NOT hold the buffer: is_dirty(uri) is false.
        let scheduler = sched(&pc, &layer, Some(Arc::new(DocumentStore::new())));
        scheduler.schedule(URI);
        assert_eq!(scheduler.wait_for_pulls(1, Duration::from_secs(5)), 1);
        assert_eq!(
            pc.pulls.load(Ordering::SeqCst),
            0,
            "no PC query for a clean buffer"
        );
        let log = published.lock().unwrap();
        assert_eq!(log.len(), 2, "{log:?}");
        assert!(log[1].diagnostics.is_empty(), "the stale overlay cleared");
    }

    // cancel() disarms a pending pull: after a didSave/didClose clear, the
    // queued pull must not fire and re-add the overlay.
    #[test]
    fn cancel_disarms_a_pending_pull() {
        let pc = Arc::new(SchedFakePc::default());
        pc.booted.store(true, Ordering::SeqCst);
        pc.did_open("t", URI, "x");
        let (layer, published) = capturing_layer();
        let scheduler = PcDiagnosticsScheduler::with_debounce(
            Arc::clone(&pc) as Arc<dyn PcQueryService>,
            Arc::clone(&layer),
            Duration::from_secs(3600),
        );
        scheduler.install_docs(dirty_docs(URI));
        scheduler.schedule(URI);
        scheduler.cancel(URI);
        // The worker never runs the pull; drop joins promptly despite the huge
        // debounce (shutdown wakes the wait).
        drop(scheduler);
        assert_eq!(pc.pulls.load(Ordering::SeqCst), 0);
        assert!(published.lock().unwrap().is_empty());
    }

    // Before install_docs (pre-Ready), a pull is skipped: no publish, no query.
    #[test]
    fn a_pull_without_installed_docs_is_skipped() {
        let pc = Arc::new(SchedFakePc::default());
        pc.booted.store(true, Ordering::SeqCst);
        pc.did_open("t", URI, "x");
        let (layer, published) = capturing_layer();
        let scheduler = sched(&pc, &layer, None);
        scheduler.schedule(URI);
        assert_eq!(scheduler.wait_for_pulls(1, Duration::from_secs(5)), 1);
        assert_eq!(pc.pulls.load(Ordering::SeqCst), 0);
        assert!(published.lock().unwrap().is_empty());
    }
}
