//! The per-request watchdog and the dispatch-generation recovery ladder.
//!
//! PC requests run on the single loaned dispatch lane under a deadline. A normal
//! nonzero PC status fails the request typed (`PcError::Backend`) with no
//! recovery; a deadline overrun fails it typed (`PcError::RequestTimeout`,
//! never deadlocking the caller) and escalates the recovery ladder over the
//! control lane: `restart_instances` (the facade shutdown+recreate — the
//! cooperative lever; the 15-op vtable has no separate cancel op) and, if that
//! does not free the dispatch lane, a fresh dispatch generation via
//! `spawn_dispatch(gen+1)` with the mirrored targets/buffers replayed into it.
//! A failed control op, or exceeding the abandoned-generation cap, is
//! island-fatal.
//!
//! The lane operations are abstracted behind [`PcBackend`] so the ladder is
//! deterministically unit-tested with a fake; the production implementation is
//! [`crate::backend::VtableBackend`], driving the registered vtable over FFM.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ls_pc_abi::payloads::TargetConfig;

use crate::backend::VtableBackend;
use crate::dispatch::{Advance, GenerationState};
use crate::mirror::{ReplayPlan, TargetMirror};

/// The position-query PC ops (they share the `uri, line, character -> buffer`
/// slot shape).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryKind {
    Completion,
    Hover,
    SignatureHelp,
    Definition,
    TypeDefinition,
    PrepareRename,
}

/// A PC request routed through the supervisor onto the dispatch lane. Lifecycle
/// variants update the replay mirror; the rest are pure queries.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PcRequest {
    RegisterTarget {
        id: String,
        config: TargetConfig,
    },
    DidOpen {
        target_id: String,
        uri: String,
        text: String,
    },
    DidChange {
        uri: String,
        text: String,
    },
    DidClose {
        uri: String,
    },
    Query {
        kind: QueryKind,
        uri: String,
        line: u32,
        character: u32,
    },
    Resolve {
        target_id: String,
        symbol: String,
        item: Vec<u8>,
    },
}

/// The outcome of a single dispatch-lane attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// The op completed with `STATUS_OK` and this response payload.
    Done(Vec<u8>),
    /// The op returned a nonzero PC status (a normal error, not a wedge).
    Status(i32),
    /// The op overran its deadline; recovery is required.
    Wedged,
}

/// A failed PC request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PcError {
    /// The request overran its deadline; recovery was triggered.
    RequestTimeout,
    /// The backend returned a nonzero status.
    Backend(i32),
}

impl std::fmt::Display for PcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PcError::RequestTimeout => write!(f, "PC request timed out"),
            PcError::Backend(code) => write!(f, "PC backend error {code}"),
        }
    }
}

impl std::error::Error for PcError {}

/// The dispatch/control lanes the supervisor drives. In production these route
/// to the loaned dispatch (worker 0) and control (worker 1) threads and invoke
/// the registered `PcVtable` slots; in tests they are faked.
pub trait PcBackend {
    /// Run `request` on the dispatch lane, waiting up to `deadline`.
    fn dispatch(&mut self, request: &PcRequest, deadline: Duration) -> DispatchOutcome;
    /// Control lane: shut down and recreate the PC instances (also the
    /// cooperative cancellation lever). Returns the vtable status.
    fn restart_instances(&mut self) -> i32;
    /// Has the dispatch lane become idle (the wedged op returned)?
    fn lane_idle(&mut self) -> bool;
    /// Control lane: spawn a fresh dispatch generation (`spawn_dispatch`) and
    /// replay the mirrored state into it. Returns the vtable status.
    fn spawn_generation(&mut self, generation: u32, replay: &ReplayPlan) -> i32;
}

/// Serializes PC requests, enforces per-request deadlines, and runs the
/// generation-recovery ladder on a wedge.
pub struct Supervisor<B: PcBackend> {
    backend: B,
    mirror: TargetMirror,
    generations: GenerationState,
    request_deadline: Duration,
    cancel_grace: Duration,
    fatal: Arc<AtomicBool>,
}

impl<B: PcBackend> Supervisor<B> {
    pub fn new(
        backend: B,
        max_abandoned_generations: u32,
        request_deadline: Duration,
        cancel_grace: Duration,
    ) -> Supervisor<B> {
        Supervisor {
            backend,
            mirror: TargetMirror::new(),
            generations: GenerationState::new(max_abandoned_generations),
            request_deadline,
            cancel_grace,
            fatal: Arc::new(AtomicBool::new(false)),
        }
    }

    /// A shared flag the driver watches: once set, the island is unrecoverable
    /// and the process must exit in an orderly way.
    pub fn fatal_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.fatal)
    }

    pub fn is_fatal(&self) -> bool {
        self.fatal.load(Ordering::SeqCst)
    }

    /// The active dispatch generation.
    pub fn generation(&self) -> u32 {
        self.generations.current()
    }

    /// Route a request: dispatch under the deadline, then mirror its lifecycle
    /// effect only if the backend accepted it. A nonzero status is a normal
    /// typed error and must NOT poison the replay mirror; a wedge fails the
    /// request typed and triggers recovery so subsequent requests succeed.
    pub fn request(&mut self, request: PcRequest) -> Result<Vec<u8>, PcError> {
        match self.backend.dispatch(&request, self.request_deadline) {
            DispatchOutcome::Done(reply) => {
                // Apply the lifecycle effect only after a successful status.
                self.observe(&request);
                Ok(reply)
            }
            // A normal PC error must not mutate the replay mirror.
            DispatchOutcome::Status(code) => Err(PcError::Backend(code)),
            DispatchOutcome::Wedged => {
                // The editor's notification is a fact regardless of the wedge, so
                // mirror it before recovery replays state into the new generation.
                self.observe(&request);
                self.recover();
                Err(PcError::RequestTimeout)
            }
        }
    }

    fn observe(&mut self, request: &PcRequest) {
        match request {
            PcRequest::RegisterTarget { id, config } => {
                self.mirror.register_target(id, config.clone())
            }
            PcRequest::DidOpen {
                target_id,
                uri,
                text,
            } => self.mirror.did_open(target_id, uri, text),
            PcRequest::DidChange { uri, text } => self.mirror.did_change(uri, text),
            PcRequest::DidClose { uri } => self.mirror.did_close(uri),
            PcRequest::Query { .. } | PcRequest::Resolve { .. } => {}
        }
    }

    fn recover(&mut self) {
        // Rung 1: reset the PC instances (also cancels the in-flight op). A
        // failed control op means the island cannot be recovered.
        if self.backend.restart_instances() != 0 {
            self.fatal.store(true, Ordering::SeqCst);
            return;
        }
        if self.wait_lane_idle() {
            // Cooperative wedge: the op returned once its facade was reset; keep
            // the generation.
            return;
        }
        // Non-cooperative wedge: abandon this generation for a fresh one and
        // replay the mirrored targets/buffers so open buffers need not be
        // reopened by the editor.
        match self.generations.advance() {
            Advance::Spawned(generation) => {
                let plan = self.mirror.replay_plan();
                if self.backend.spawn_generation(generation, &plan) != 0 {
                    self.fatal.store(true, Ordering::SeqCst);
                }
            }
            Advance::Fatal => self.fatal.store(true, Ordering::SeqCst),
        }
    }

    fn wait_lane_idle(&mut self) -> bool {
        let deadline = Instant::now() + self.cancel_grace;
        loop {
            if self.backend.lane_idle() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
}

/// Control-lane accessors for the production backend. These serve the doctor /
/// server control ops (`plugin_status`, `restart_instances`, `shutdown`) on the
/// control lane (worker 1) so they run even while the dispatch lane is busy;
/// they are generic-free because only the real vtable backend implements them.
impl Supervisor<VtableBackend> {
    /// Fetch the island's plugin-status report over the control lane.
    pub fn plugin_status(&self) -> Result<Vec<u8>, i32> {
        self.backend.plugin_status()
    }

    /// Shut down and recreate the PC instances — the cooperative recovery lever,
    /// also the doctor's explicit restart. Returns the vtable status. The Rust
    /// replay mirror is untouched, so registered targets and open buffers remain
    /// valid without the editor re-registering or reopening them.
    pub fn restart_instances(&mut self) -> i32 {
        self.backend.restart_instances()
    }

    /// Orderly PC shutdown before process exit. Returns the vtable status.
    pub fn shutdown(&self) -> i32 {
        self.backend.shutdown()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A scripted backend: each `dispatch` pops the next outcome; `cooperative`
    /// controls whether the lane frees after `restart_instances`.
    struct FakeBackend {
        outcomes: VecDeque<DispatchOutcome>,
        cooperative: bool,
        recovered: bool,
        restart_status: i32,
        spawn_status: i32,
        restarts: u32,
        spawns: Vec<(u32, ReplayPlan)>,
    }

    impl FakeBackend {
        fn new(outcomes: Vec<DispatchOutcome>, cooperative: bool) -> FakeBackend {
            FakeBackend {
                outcomes: outcomes.into(),
                cooperative,
                recovered: false,
                restart_status: 0,
                spawn_status: 0,
                restarts: 0,
                spawns: Vec::new(),
            }
        }
    }

    impl PcBackend for FakeBackend {
        fn dispatch(&mut self, _request: &PcRequest, _deadline: Duration) -> DispatchOutcome {
            self.outcomes
                .pop_front()
                .unwrap_or(DispatchOutcome::Done(Vec::new()))
        }
        fn restart_instances(&mut self) -> i32 {
            self.restarts += 1;
            if self.cooperative {
                self.recovered = true;
            }
            self.restart_status
        }
        fn lane_idle(&mut self) -> bool {
            self.recovered
        }
        fn spawn_generation(&mut self, generation: u32, replay: &ReplayPlan) -> i32 {
            self.spawns.push((generation, replay.clone()));
            self.spawn_status
        }
    }

    fn config(bsp_id: &str) -> TargetConfig {
        TargetConfig {
            bsp_id: bsp_id.to_string(),
            scala_version: "3.8.4".to_string(),
            classpath: vec![],
            scalac_options: vec![],
            source_dirs: vec![],
        }
    }

    fn query() -> PcRequest {
        PcRequest::Query {
            kind: QueryKind::Completion,
            uri: "file:///a.scala".to_string(),
            line: 1,
            character: 2,
        }
    }

    fn supervisor(backend: FakeBackend, max_abandoned: u32) -> Supervisor<FakeBackend> {
        Supervisor::new(
            backend,
            max_abandoned,
            Duration::from_millis(50),
            Duration::from_millis(5),
        )
    }

    #[test]
    fn healthy_request_returns_reply_without_recovery() {
        let backend = FakeBackend::new(vec![DispatchOutcome::Done(b"ok".to_vec())], true);
        let mut sup = supervisor(backend, 4);
        assert_eq!(sup.request(query()), Ok(b"ok".to_vec()));
        assert_eq!(sup.generation(), 0);
        assert_eq!(sup.backend.restarts, 0);
        assert!(sup.backend.spawns.is_empty());
    }

    #[test]
    fn nonzero_status_is_a_typed_error_without_recovery() {
        let backend = FakeBackend::new(vec![DispatchOutcome::Status(-4)], true);
        let mut sup = supervisor(backend, 4);
        assert_eq!(sup.request(query()), Err(PcError::Backend(-4)));
        // A normal PC error does not trigger the recovery ladder.
        assert_eq!(sup.backend.restarts, 0);
        assert!(sup.backend.spawns.is_empty());
        assert!(!sup.is_fatal());
    }

    #[test]
    fn cooperative_wedge_recovers_via_restart() {
        let backend = FakeBackend::new(vec![DispatchOutcome::Wedged], true);
        let mut sup = supervisor(backend, 4);
        assert_eq!(sup.request(query()), Err(PcError::RequestTimeout));
        // restart freed the lane; no new generation.
        assert_eq!(sup.backend.restarts, 1);
        assert!(sup.backend.spawns.is_empty());
        assert_eq!(sup.generation(), 0);
        assert!(!sup.is_fatal());
    }

    #[test]
    fn non_cooperative_wedge_spawns_new_generation_and_replays_buffers() {
        let backend = FakeBackend::new(
            vec![
                DispatchOutcome::Done(Vec::new()),
                DispatchOutcome::Done(Vec::new()),
                DispatchOutcome::Wedged,
            ],
            false,
        );
        let mut sup = supervisor(backend, 4);
        sup.request(PcRequest::RegisterTarget {
            id: "a".to_string(),
            config: config("a"),
        })
        .unwrap();
        sup.request(PcRequest::DidOpen {
            target_id: "a".to_string(),
            uri: "file:///a.scala".to_string(),
            text: "package a".to_string(),
        })
        .unwrap();

        assert_eq!(sup.request(query()), Err(PcError::RequestTimeout));

        // A fresh generation was spawned and the mirror replayed, so the buffer
        // need not be reopened by the editor.
        assert_eq!(sup.generation(), 1);
        assert_eq!(sup.backend.restarts, 1);
        assert_eq!(sup.backend.spawns.len(), 1);
        let (generation, replay) = &sup.backend.spawns[0];
        assert_eq!(*generation, 1);
        assert_eq!(replay.targets.len(), 1);
        assert_eq!(replay.buffers.len(), 1);
        assert_eq!(replay.buffers[0].uri, "file:///a.scala");
        assert_eq!(replay.buffers[0].text, "package a");
        assert!(!sup.is_fatal());
    }

    #[test]
    fn abandoned_generation_cap_exceeded_is_fatal() {
        let backend = FakeBackend::new(
            vec![DispatchOutcome::Wedged, DispatchOutcome::Wedged],
            false,
        );
        let mut sup = supervisor(backend, 1);
        assert_eq!(sup.request(query()), Err(PcError::RequestTimeout));
        assert_eq!(sup.generation(), 1);
        assert!(!sup.is_fatal());
        assert_eq!(sup.request(query()), Err(PcError::RequestTimeout));
        // The second abandonment exceeds the cap of 1.
        assert!(sup.is_fatal());
    }

    #[test]
    fn failed_restart_instances_is_fatal() {
        let mut backend = FakeBackend::new(vec![DispatchOutcome::Wedged], false);
        backend.restart_status = -6;
        let mut sup = supervisor(backend, 4);
        assert_eq!(sup.request(query()), Err(PcError::RequestTimeout));
        // A control-lane failure means the island cannot recover.
        assert!(sup.is_fatal());
        assert!(sup.backend.spawns.is_empty());
    }

    #[test]
    fn failed_spawn_generation_is_fatal() {
        let mut backend = FakeBackend::new(vec![DispatchOutcome::Wedged], false);
        backend.spawn_status = -6;
        let mut sup = supervisor(backend, 4);
        assert_eq!(sup.request(query()), Err(PcError::RequestTimeout));
        assert_eq!(sup.backend.spawns.len(), 1);
        // spawn_dispatch/replay failed → fatal.
        assert!(sup.is_fatal());
    }

    #[test]
    fn a_wedged_request_returns_promptly_and_the_next_request_succeeds() {
        let backend = FakeBackend::new(vec![DispatchOutcome::Wedged], false);
        let mut sup = supervisor(backend, 4);
        assert_eq!(sup.request(query()), Err(PcError::RequestTimeout));
        assert_eq!(sup.request(query()), Ok(Vec::new()));
        assert_eq!(sup.generation(), 1);
    }

    #[test]
    fn a_failed_lifecycle_op_is_not_replayed_into_a_new_generation() {
        // register_target succeeds, did_open returns a normal error, then a
        // query wedges non-cooperatively.
        let backend = FakeBackend::new(
            vec![
                DispatchOutcome::Done(Vec::new()),
                DispatchOutcome::Status(-5),
                DispatchOutcome::Wedged,
            ],
            false,
        );
        let mut sup = supervisor(backend, 4);
        sup.request(PcRequest::RegisterTarget {
            id: "a".to_string(),
            config: config("a"),
        })
        .unwrap();
        assert_eq!(
            sup.request(PcRequest::DidOpen {
                target_id: "a".to_string(),
                uri: "file:///a.scala".to_string(),
                text: "package a".to_string(),
            }),
            Err(PcError::Backend(-5))
        );

        assert_eq!(sup.request(query()), Err(PcError::RequestTimeout));
        assert_eq!(sup.generation(), 1);

        // The replay carries the successful target but NOT the failed did_open.
        let (_, replay) = &sup.backend.spawns[0];
        assert_eq!(replay.targets.len(), 1);
        assert!(
            replay.buffers.is_empty(),
            "a did_open that returned a nonzero status must not be replayed"
        );
    }
}
