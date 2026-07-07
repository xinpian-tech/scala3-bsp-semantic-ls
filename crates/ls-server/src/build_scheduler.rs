//! The debounced, single-flight background build-job scheduler — a behavior-
//! preserving port of `ScalaLs.scheduleBuildJob`/`runBuildJob`.
//!
//! Two producers feed it, both collapsing into one debounced run:
//! - `didSave` schedules a COMPILE-FIRST job over the reverse-dependency closure
//!   of the saved file's target (`schedule(targets, compile_first=true)`).
//! - A RawSemanticDBPath reference that could not heal inline schedules a
//!   REINDEX-ONLY job (`schedule_reindex()` = `schedule(vec![], false)`).
//!
//! Rapid schedules collapse into exactly one follow-up run (queue collapse); the
//! run is delayed by a fixed debounce; the worker is the sole background writer.
//! The worker compiles the sorted target set first (when a compile is pending),
//! then full-reingests the CURRENT workspace on success — or on a reindex-only
//! run. On compile FAILURE it logs and SKIPS the reingest, leaving the previous
//! snapshot serving. The store's single-writer contract is enforced by
//! [`QueryOrchestrator`]'s ingest lock, so a background reingest never races the
//! message-loop ingest.

use std::collections::BTreeSet;
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ls_engine::{CompileOutcome, QueryOrchestrator};

use crate::services::BuildCompiler;

/// State shared between the message loop (which schedules) and the index worker
/// thread (which drains), behind one mutex + condvar.
struct Shared {
    /// A run is pending (scheduled, not yet started). Set on schedule, cleared
    /// when the worker begins a run — so schedules arriving during a run collapse
    /// into exactly one follow-up run.
    scheduled: bool,
    /// When the pending run may start: a FIXED debounce window from the schedule
    /// that armed it (matches Scala's one-shot `scheduler.schedule(delay)`, not a
    /// sliding window — a later schedule during the window does not extend it).
    due_at: Option<Instant>,
    /// Set on drop; the worker exits and skips any pending run.
    shutting_down: bool,
    /// Target ids to compile before the reingest (accumulated across coalesced
    /// schedules). Drained (sorted, since a `BTreeSet`) at run start.
    pending_targets: BTreeSet<String>,
    /// Whether a compile-first job is requested for the next run (ORed across
    /// schedules).
    pending_compile: bool,
    /// Whether a reindex-only heal is requested for the next run (ORed across
    /// schedules). Tracked INDEPENDENTLY of `pending_compile` so a `references`
    /// raw-path fallback heal that coalesces into a FAILING `didSave` compile is
    /// still honored — a compile failure skips the reingest only when no
    /// reindex-only intent shared the debounce window.
    pending_reindex: bool,
    /// Number of drained runs the worker has completed, for deterministic tests.
    runs_completed: u64,
    /// Number of runs that reached the reingest step (i.e. did NOT skip it on a
    /// compile failure), for deterministic tests.
    reingests_attempted: u64,
}

/// A background compile+reingester. Owned by the ready services; dropping it stops
/// and joins the worker thread.
pub(crate) struct BuildScheduler {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    debounce: Duration,
    handle: Option<JoinHandle<()>>,
}

impl BuildScheduler {
    /// The production debounce, matching the Scala `config.debounceMillis`.
    pub(crate) const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(500);

    pub(crate) fn new(
        orchestrator: Arc<QueryOrchestrator>,
        compiler: Arc<dyn BuildCompiler>,
    ) -> Self {
        Self::with_debounce(orchestrator, compiler, Self::DEFAULT_DEBOUNCE)
    }

    pub(crate) fn with_debounce(
        orchestrator: Arc<QueryOrchestrator>,
        compiler: Arc<dyn BuildCompiler>,
        debounce: Duration,
    ) -> Self {
        let shared = Arc::new((
            Mutex::new(Shared {
                scheduled: false,
                due_at: None,
                shutting_down: false,
                pending_targets: BTreeSet::new(),
                pending_compile: false,
                pending_reindex: false,
                runs_completed: 0,
                reingests_attempted: 0,
            }),
            Condvar::new(),
        ));
        let worker_shared = Arc::clone(&shared);
        let handle = thread::spawn(move || worker_loop(&worker_shared, &orchestrator, &compiler));
        BuildScheduler {
            shared,
            debounce,
            handle: Some(handle),
        }
    }

    /// Schedule a build job (Scala `scheduleBuildJob(targets, compileFirst)`).
    /// A compile-first schedule accumulates `pending_targets` and sets
    /// `pending_compile`; a non-compile schedule sets the INDEPENDENT
    /// `pending_reindex` intent. Both arm the single debounce if a run is not
    /// already pending (coalesce otherwise). Never blocks.
    pub(crate) fn schedule(&self, targets: Vec<String>, compile_first: bool) {
        let (lock, cvar) = &*self.shared;
        let mut shared = lock.lock().unwrap();
        if shared.shutting_down {
            return;
        }
        if compile_first {
            shared.pending_targets.extend(targets);
            shared.pending_compile = true;
        } else {
            shared.pending_reindex = true;
        }
        if !shared.scheduled {
            shared.scheduled = true;
            shared.due_at = Some(Instant::now() + self.debounce);
            cvar.notify_all();
        }
    }

    /// Enqueue a reindex-only background job (Scala `scheduleBuildJob(Vector.empty,
    /// compileFirst = false)`) — the RawSemanticDBPath heal fallback.
    pub(crate) fn schedule_reindex(&self) {
        self.schedule(Vec::new(), false);
    }

    /// Test-only: block until at least `n` runs have completed, or `timeout`
    /// elapses; returns the observed run count.
    #[cfg(test)]
    pub(crate) fn wait_for_runs(&self, n: u64, timeout: Duration) -> u64 {
        let (lock, cvar) = &*self.shared;
        let mut shared = lock.lock().unwrap();
        let deadline = Instant::now() + timeout;
        while shared.runs_completed < n {
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            let (guard, _) = cvar.wait_timeout(shared, remaining).unwrap();
            shared = guard;
        }
        shared.runs_completed
    }

    #[cfg(test)]
    pub(crate) fn runs_completed(&self) -> u64 {
        self.shared.0.lock().unwrap().runs_completed
    }

    #[cfg(test)]
    pub(crate) fn reingests_attempted(&self) -> u64 {
        self.shared.0.lock().unwrap().reingests_attempted
    }
}

impl Drop for BuildScheduler {
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
    orchestrator: &QueryOrchestrator,
    compiler: &Arc<dyn BuildCompiler>,
) {
    let (lock, cvar) = &**shared;
    loop {
        let mut guard = lock.lock().unwrap();
        // Wait for a scheduled run (or shutdown).
        while !guard.shutting_down && !guard.scheduled {
            guard = cvar.wait(guard).unwrap();
        }
        if guard.shutting_down {
            return;
        }
        // Debounce: wait until `due_at`, waking early on shutdown or a notify. A
        // schedule arriving during the window does not extend it (fixed window).
        loop {
            if guard.shutting_down {
                return;
            }
            let now = Instant::now();
            let due = guard.due_at.unwrap_or(now);
            match due.checked_duration_since(now) {
                None => break,
                Some(remaining) => {
                    let (next, _) = cvar.wait_timeout(guard, remaining).unwrap();
                    guard = next;
                }
            }
        }
        // Begin the run: snapshot + clear ALL pending intents so schedules during
        // the job arm exactly one follow-up. `BTreeSet::into_iter` yields sorted
        // ids (Scala `toVector.sorted`). `reindex` is tracked independently of the
        // compile so a coalesced heal survives a compile failure.
        let targets: Vec<String> = std::mem::take(&mut guard.pending_targets)
            .into_iter()
            .collect();
        let compile = guard.pending_compile;
        let reindex = guard.pending_reindex;
        guard.pending_compile = false;
        guard.pending_reindex = false;
        guard.scheduled = false;
        guard.due_at = None;
        drop(guard);

        // Compile first when a compile-first job is pending; on failure log and
        // leave the previous snapshot serving. `compile_ok` is true when no compile
        // was attempted or the compile succeeded.
        let compile_ok = if compile && !targets.is_empty() {
            match compiler.compile(&targets) {
                CompileOutcome::Ok => true,
                CompileOutcome::Failed { reason } => {
                    eprintln!(
                        "scala3-bsp-semantic-ls: background compile of {} failed: {reason}",
                        targets.join(", ")
                    );
                    false
                }
            }
        } else {
            true
        };
        // Reingest on a successful (or no-op) compile, OR whenever a reindex-only
        // intent shared this run — so a `references` heal is never dropped by a
        // failing `didSave` compile. Skip the reingest ONLY for a failing
        // compile-first run that carried no reindex-only intent.
        let should_reingest = compile_ok || reindex;
        if should_reingest {
            if let Some(Err(error)) = orchestrator.reingest_current() {
                eprintln!("scala3-bsp-semantic-ls: background reingest failed: {error}");
            }
        }

        let mut guard = lock.lock().unwrap();
        guard.runs_completed += 1;
        if should_reingest {
            guard.reingests_attempted += 1;
        }
        cvar.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ls_bsp::model::BspProjectModel;
    use ls_engine::CompileService;
    use ls_store::Store;
    use std::sync::Mutex as StdMutex;
    use tempfile::TempDir;

    /// Records every compile call and returns a scripted outcome. `refetch_model`
    /// is unused by the scheduler (the reload flow does not go through it).
    struct FakeCompiler {
        fail: bool,
        calls: Arc<StdMutex<Vec<Vec<String>>>>,
    }

    impl CompileService for FakeCompiler {
        fn compile(&self, targets: &[String]) -> CompileOutcome {
            self.calls.lock().unwrap().push(targets.to_vec());
            if self.fail {
                CompileOutcome::Failed {
                    reason: "scripted failure".to_string(),
                }
            } else {
                CompileOutcome::Ok
            }
        }
    }

    impl BuildCompiler for FakeCompiler {
        fn refetch_model(&self) -> Result<BspProjectModel, String> {
            Err("no reload in the scheduler test".to_string())
        }
    }

    /// A scheduler over a fresh (empty, unindexed) store and a scripted compiler.
    /// The `TempDir` is returned so the caller binds it BEFORE the scheduler:
    /// reverse-drop-order joins the worker before deleting the directory.
    fn scheduler(
        debounce: Duration,
        compile_fails: bool,
    ) -> (TempDir, Arc<StdMutex<Vec<Vec<String>>>>, BuildScheduler) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        let orchestrator = Arc::new(QueryOrchestrator::with_defaults(store));
        let calls = Arc::new(StdMutex::new(Vec::new()));
        let compiler: Arc<dyn BuildCompiler> = Arc::new(FakeCompiler {
            fail: compile_fails,
            calls: Arc::clone(&calls),
        });
        let sched = BuildScheduler::with_debounce(orchestrator, compiler, debounce);
        (dir, calls, sched)
    }

    #[test]
    fn a_reindex_only_schedule_drains_one_run_and_reingests_without_compiling() {
        let (_dir, calls, sched) = scheduler(Duration::ZERO, false);
        sched.schedule_reindex();
        assert_eq!(sched.wait_for_runs(1, Duration::from_secs(5)), 1);
        assert_eq!(sched.reingests_attempted(), 1);
        assert!(
            calls.lock().unwrap().is_empty(),
            "reindex-only must not compile"
        );
    }

    #[test]
    fn a_compile_first_schedule_compiles_sorted_targets_then_reingests() {
        let (_dir, calls, sched) = scheduler(Duration::ZERO, false);
        // Deliberately unsorted; the worker drains a BTreeSet so compile sees sorted.
        sched.schedule(vec!["b".to_string(), "a".to_string()], true);
        assert_eq!(sched.wait_for_runs(1, Duration::from_secs(5)), 1);
        assert_eq!(
            *calls.lock().unwrap(),
            vec![vec!["a".to_string(), "b".to_string()]]
        );
        assert_eq!(sched.reingests_attempted(), 1, "compile Ok reingests");
    }

    #[test]
    fn a_compile_failure_skips_the_reingest_leaving_the_old_snapshot() {
        let (_dir, calls, sched) = scheduler(Duration::ZERO, true);
        sched.schedule(vec!["a".to_string()], true);
        assert_eq!(sched.wait_for_runs(1, Duration::from_secs(5)), 1);
        assert_eq!(calls.lock().unwrap().len(), 1, "compile was attempted");
        assert_eq!(
            sched.reingests_attempted(),
            0,
            "compile failure must skip the reingest"
        );
    }

    // Rapid compile-first schedules while a run is pending collapse into ONE run,
    // accumulating the merged sorted target set (Scala queue collapse).
    #[test]
    fn rapid_compile_first_schedules_collapse_and_accumulate_targets() {
        let (_dir, calls, sched) = scheduler(Duration::from_millis(80), false);
        sched.schedule(vec!["a".to_string()], true);
        sched.schedule(vec!["b".to_string()], true);
        sched.schedule_reindex();
        assert_eq!(sched.wait_for_runs(1, Duration::from_secs(5)), 1);
        // One run, compiling the merged sorted target set once.
        assert_eq!(
            *calls.lock().unwrap(),
            vec![vec!["a".to_string(), "b".to_string()]]
        );
        assert_eq!(sched.runs_completed(), 1);
    }

    // The regression this round fixes: a reindex-only heal (the `references`
    // fallback) that coalesces into a FAILING `didSave` compile must NOT be
    // dropped — the run still attempts exactly one reingest, even though the
    // compile failed. `pending_reindex` is tracked independently of the compile.
    #[test]
    fn a_reindex_only_heal_survives_a_coalesced_failing_compile() {
        // A window long enough to hold the run pending while both producers arrive.
        let (_dir, calls, sched) = scheduler(Duration::from_millis(80), true);
        sched.schedule(vec!["a".to_string()], true); // failing compile-first save
        sched.schedule_reindex(); // references raw-path heal, same window
        assert_eq!(sched.wait_for_runs(1, Duration::from_secs(5)), 1);
        assert_eq!(calls.lock().unwrap().len(), 1, "the compile was attempted");
        assert_eq!(
            sched.reingests_attempted(),
            1,
            "the coalesced reindex-only heal must still reingest despite the compile failure"
        );
    }

    #[test]
    fn a_schedule_after_a_run_arms_one_follow_up() {
        let (_dir, _calls, sched) = scheduler(Duration::ZERO, false);
        sched.schedule_reindex();
        assert_eq!(sched.wait_for_runs(1, Duration::from_secs(5)), 1);
        sched.schedule_reindex();
        assert_eq!(sched.wait_for_runs(2, Duration::from_secs(5)), 2);
    }

    // Dropping the scheduler stops and joins the worker promptly even with a long
    // debounce (shutdown wakes the debounce wait rather than sleeping it out).
    #[test]
    fn drop_joins_the_worker_without_waiting_out_the_debounce() {
        let (_dir, _calls, sched) = scheduler(Duration::from_secs(3600), false);
        sched.schedule_reindex();
        drop(sched);
    }
}
