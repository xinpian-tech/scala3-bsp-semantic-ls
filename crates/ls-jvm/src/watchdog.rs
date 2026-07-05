//! The per-request watchdog and the dispatch-generation recovery ladder.
//!
//! PC requests run on the single loaned dispatch lane under a deadline. When a
//! request overruns, it fails typed (never deadlocking the caller) and recovery
//! escalates exactly as the plan's ladder: cancel the in-flight op, then
//! `restart_instances`; if that frees the lane the generation is kept (a
//! cooperative wedge), otherwise a fresh dispatch generation is spawned and the
//! mirrored targets/buffers are replayed into it (a non-cooperative wedge), and
//! if abandoned generations exceed the cap the island is fatal.
//!
//! The lane operations are abstracted behind [`PcBackend`] so the whole ladder
//! is deterministically unit-tested with a fake — the live backend that drives
//! the registered PC vtable over FFM is exercised end-to-end with the Java
//! island.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ls_pc_abi::payloads::TargetConfig;

use crate::dispatch::{Advance, GenerationState};
use crate::mirror::{ReplayPlan, TargetMirror};

/// The dispatch lane overran its deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Wedged;

/// A PC request routed through the supervisor. Lifecycle variants update the
/// replay mirror; `Query` carries an opaque encoded op payload.
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
    Query(Vec<u8>),
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

/// The dispatch/control lanes the supervisor drives. In production these call
/// the registered PC vtable + Rust vtable over FFM; in tests they are faked.
pub trait PcBackend {
    /// Run `request` on the dispatch lane, waiting up to `deadline`.
    fn dispatch(&mut self, request: &PcRequest, deadline: Duration) -> Result<Vec<u8>, Wedged>;
    /// Control lane: ask the in-flight op to cancel cooperatively.
    fn cancel(&mut self);
    /// Control lane: shut down and recreate the PC instances.
    fn restart_instances(&mut self) -> i32;
    /// Has the wedged dispatch lane become idle (cancellation honored)?
    fn lane_idle(&mut self) -> bool;
    /// Spawn a fresh dispatch generation and replay the mirrored state into it.
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

    /// Route a request: mirror its lifecycle effect, then dispatch under the
    /// deadline. A wedge fails the request typed and triggers recovery so
    /// subsequent requests succeed.
    pub fn request(&mut self, request: PcRequest) -> Result<Vec<u8>, PcError> {
        self.observe(&request);
        match self.backend.dispatch(&request, self.request_deadline) {
            Ok(reply) => Ok(reply),
            Err(Wedged) => {
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
            PcRequest::Query(_) => {}
        }
    }

    fn recover(&mut self) {
        // Rungs 1-2: cancel the in-flight op, then reset the PC instances.
        self.backend.cancel();
        self.backend.restart_instances();
        if self.wait_lane_idle() {
            // Cooperative wedge: cancel + restart freed the lane; keep the
            // generation.
            return;
        }
        // Non-cooperative wedge: abandon this generation for a fresh one and
        // replay the mirrored targets/buffers so open buffers need not be
        // reopened by the editor.
        match self.generations.advance() {
            Advance::Spawned(generation) => {
                let plan = self.mirror.replay_plan();
                self.backend.spawn_generation(generation, &plan);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// A scripted backend: each `dispatch` pops the next outcome; `cooperative`
    /// controls whether the lane frees after cancel + restart.
    struct FakeBackend {
        outcomes: VecDeque<Result<Vec<u8>, Wedged>>,
        cooperative: bool,
        recovered: bool,
        cancels: u32,
        restarts: u32,
        spawns: Vec<(u32, ReplayPlan)>,
    }

    impl FakeBackend {
        fn new(outcomes: Vec<Result<Vec<u8>, Wedged>>, cooperative: bool) -> FakeBackend {
            FakeBackend {
                outcomes: outcomes.into(),
                cooperative,
                recovered: false,
                cancels: 0,
                restarts: 0,
                spawns: Vec::new(),
            }
        }
    }

    impl PcBackend for FakeBackend {
        fn dispatch(
            &mut self,
            _request: &PcRequest,
            _deadline: Duration,
        ) -> Result<Vec<u8>, Wedged> {
            self.outcomes.pop_front().unwrap_or(Ok(Vec::new()))
        }
        fn cancel(&mut self) {
            self.cancels += 1;
        }
        fn restart_instances(&mut self) -> i32 {
            self.restarts += 1;
            if self.cooperative {
                self.recovered = true;
            }
            0
        }
        fn lane_idle(&mut self) -> bool {
            self.recovered
        }
        fn spawn_generation(&mut self, generation: u32, replay: &ReplayPlan) -> i32 {
            self.spawns.push((generation, replay.clone()));
            0
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
        let backend = FakeBackend::new(vec![Ok(b"ok".to_vec())], true);
        let mut sup = supervisor(backend, 4);
        assert_eq!(sup.request(PcRequest::Query(vec![1])), Ok(b"ok".to_vec()));
        assert_eq!(sup.generation(), 0);
        assert_eq!(sup.backend.cancels, 0);
        assert_eq!(sup.backend.restarts, 0);
        assert!(sup.backend.spawns.is_empty());
    }

    #[test]
    fn cooperative_wedge_recovers_via_cancel_and_restart() {
        let backend = FakeBackend::new(vec![Err(Wedged)], true);
        let mut sup = supervisor(backend, 4);
        assert_eq!(
            sup.request(PcRequest::Query(vec![1])),
            Err(PcError::RequestTimeout)
        );
        // Cancel + restart freed the lane; no new generation.
        assert_eq!(sup.backend.cancels, 1);
        assert_eq!(sup.backend.restarts, 1);
        assert!(sup.backend.spawns.is_empty());
        assert_eq!(sup.generation(), 0);
        assert!(!sup.is_fatal());
    }

    #[test]
    fn non_cooperative_wedge_spawns_new_generation_and_replays_buffers() {
        // A registered target + open buffer, then a non-cooperative wedge.
        let backend = FakeBackend::new(vec![Ok(Vec::new()), Ok(Vec::new()), Err(Wedged)], false);
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

        assert_eq!(
            sup.request(PcRequest::Query(vec![9])),
            Err(PcError::RequestTimeout)
        );

        // A fresh generation was spawned and the mirror was replayed, so the
        // buffer need not be reopened by the editor (E7).
        assert_eq!(sup.generation(), 1);
        assert_eq!(sup.backend.restarts, 1);
        assert_eq!(sup.backend.spawns.len(), 1);
        let (generation, replay) = &sup.backend.spawns[0];
        assert_eq!(*generation, 1);
        assert_eq!(replay.targets.len(), 1);
        assert_eq!(replay.targets[0].0, "a");
        assert_eq!(replay.buffers.len(), 1);
        assert_eq!(replay.buffers[0].uri, "file:///a.scala");
        assert_eq!(replay.buffers[0].text, "package a");
        assert!(!sup.is_fatal());
    }

    #[test]
    fn abandoned_generation_cap_exceeded_is_fatal() {
        // Cap of 1: the second non-cooperative wedge exceeds it.
        let backend = FakeBackend::new(vec![Err(Wedged), Err(Wedged)], false);
        let mut sup = supervisor(backend, 1);

        assert_eq!(
            sup.request(PcRequest::Query(vec![1])),
            Err(PcError::RequestTimeout)
        );
        assert_eq!(sup.generation(), 1);
        assert!(!sup.is_fatal());

        assert_eq!(
            sup.request(PcRequest::Query(vec![2])),
            Err(PcError::RequestTimeout)
        );
        // Second abandonment exceeds the cap → island-fatal.
        assert!(sup.is_fatal());
        let fatal = sup.fatal_flag();
        assert!(fatal.load(Ordering::SeqCst));
    }

    #[test]
    fn a_wedged_request_returns_promptly_and_the_next_request_succeeds() {
        // After a non-cooperative wedge recovers via a new generation, the next
        // request is served (the fake's default outcome is healthy).
        let backend = FakeBackend::new(vec![Err(Wedged)], false);
        let mut sup = supervisor(backend, 4);
        assert_eq!(
            sup.request(PcRequest::Query(vec![1])),
            Err(PcError::RequestTimeout)
        );
        assert_eq!(sup.request(PcRequest::Query(vec![2])), Ok(Vec::new()));
        assert_eq!(sup.generation(), 1);
    }
}
