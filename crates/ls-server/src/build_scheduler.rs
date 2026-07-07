//! The debounced, single-flight background reingest scheduler — a behavior-
//! preserving port of `ScalaLs.scheduleBuildJob`/`runBuildJob`.
//!
//! A RawSemanticDBPath reference that finds the index stale (`needs_reindex`)
//! enqueues a full reingest that runs ASYNCHRONOUSLY on one index worker thread,
//! so the reference response is served immediately (not blocked on the ingest).
//! Rapid enqueues collapse into exactly one follow-up run (queue collapse), the
//! run is delayed by a fixed debounce, and the worker is the sole background
//! writer. The store's single-writer contract is also enforced independently by
//! [`QueryOrchestrator::ingest`]'s internal lock, so a background reingest never
//! races the message-loop ingest (the explicit reindex command / model reload /
//! bootstrap).
//!
//! Only the reindex-only path (`compileFirst = false`) is wired here; the
//! compile-first save path reuses this scheduler when `didSave` lands.

use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use ls_engine::QueryOrchestrator;

/// State shared between the message loop (which schedules) and the index worker
/// thread (which drains), behind one mutex + condvar.
struct Shared {
    /// A reingest is pending (scheduled, not yet started). Set on schedule, cleared
    /// when the worker begins a run — so schedules arriving during a run collapse
    /// into exactly one follow-up run.
    scheduled: bool,
    /// When the pending run may start: a FIXED debounce window from the schedule
    /// that armed it (matches Scala's one-shot `scheduler.schedule(delay)`, not a
    /// sliding window — a later schedule during the window does not extend it).
    due_at: Option<Instant>,
    /// Set on drop; the worker exits and skips any pending run.
    shutting_down: bool,
    /// Number of drained runs the worker has completed, for deterministic tests.
    runs_completed: u64,
}

/// A background reingester. Owned by the ready services; dropping it stops and
/// joins the worker thread.
pub(crate) struct BuildScheduler {
    shared: Arc<(Mutex<Shared>, Condvar)>,
    debounce: Duration,
    handle: Option<JoinHandle<()>>,
}

impl BuildScheduler {
    /// The production debounce, matching the Scala `config.debounceMillis`.
    pub(crate) const DEFAULT_DEBOUNCE: Duration = Duration::from_millis(500);

    pub(crate) fn new(orchestrator: Arc<QueryOrchestrator>) -> Self {
        Self::with_debounce(orchestrator, Self::DEFAULT_DEBOUNCE)
    }

    pub(crate) fn with_debounce(orchestrator: Arc<QueryOrchestrator>, debounce: Duration) -> Self {
        let shared = Arc::new((
            Mutex::new(Shared {
                scheduled: false,
                due_at: None,
                shutting_down: false,
                runs_completed: 0,
            }),
            Condvar::new(),
        ));
        let worker_shared = Arc::clone(&shared);
        let handle = thread::spawn(move || worker_loop(&worker_shared, &orchestrator));
        BuildScheduler {
            shared,
            debounce,
            handle: Some(handle),
        }
    }

    /// Enqueue a background reingest (Scala `scheduleBuildJob(Vector.empty,
    /// compileFirst = false)`). Coalesces: if a run is already pending, this just
    /// joins it. Never blocks on the ingest.
    pub(crate) fn schedule_reindex(&self) {
        let (lock, cvar) = &*self.shared;
        let mut shared = lock.lock().unwrap();
        if shared.shutting_down || shared.scheduled {
            return;
        }
        shared.scheduled = true;
        shared.due_at = Some(Instant::now() + self.debounce);
        cvar.notify_all();
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

fn worker_loop(shared: &Arc<(Mutex<Shared>, Condvar)>, orchestrator: &QueryOrchestrator) {
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
        // Begin the run: clear `scheduled` so schedules during the ingest arm a new
        // run (one follow-up), and release the lock across the ingest.
        guard.scheduled = false;
        guard.due_at = None;
        drop(guard);

        // Best-effort full reingest of the CURRENT workspace. `reingest_current`
        // reads `current_workspace` INSIDE the orchestrator's ingest lock, so a
        // heal that overlaps a concurrent `reload` re-ingests the reload's newer
        // workspace instead of reverting it (a workspace with no indexable target
        // is a no-op). A failure never propagates — the raw path already answered.
        if let Some(Err(error)) = orchestrator.reingest_current() {
            eprintln!("scala3-bsp-semantic-ls: background reingest failed: {error}");
        }

        let mut guard = lock.lock().unwrap();
        guard.runs_completed += 1;
        cvar.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ls_store::Store;
    use tempfile::TempDir;

    // A fresh (empty, unindexed) store. The `TempDir` is returned so the caller can
    // bind it BEFORE the scheduler: reverse-drop-order then joins the worker (drops
    // the scheduler) before deleting the directory. Its workspace has no indexable
    // target, so a drained run is a no-op ingest — enough to observe run counting
    // and coalescing without a real corpus.
    fn empty_orchestrator() -> (TempDir, Arc<QueryOrchestrator>) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        // The scheduler's reingest path (`reingest_current`) is independent of the
        // write-through mode, so the production `with_defaults` orchestrator is used.
        (dir, Arc::new(QueryOrchestrator::with_defaults(store)))
    }

    #[test]
    fn a_single_schedule_drains_exactly_one_run() {
        let (_dir, orchestrator) = empty_orchestrator();
        let scheduler = BuildScheduler::with_debounce(orchestrator, Duration::ZERO);
        scheduler.schedule_reindex();
        assert_eq!(scheduler.wait_for_runs(1, Duration::from_secs(5)), 1);
        // No further schedule: the count stays at exactly one.
        assert_eq!(scheduler.runs_completed(), 1);
    }

    // Rapid schedules while a run is pending collapse into ONE run (Scala queue
    // collapse: `scheduled` gates re-arming).
    #[test]
    fn rapid_schedules_collapse_into_one_run() {
        let (_dir, orchestrator) = empty_orchestrator();
        // A long debounce keeps the first run pending while the burst arrives.
        let scheduler = BuildScheduler::with_debounce(orchestrator, Duration::from_millis(80));
        for _ in 0..50 {
            scheduler.schedule_reindex();
        }
        assert_eq!(scheduler.wait_for_runs(1, Duration::from_secs(5)), 1);
        // The burst collapsed: no second run was armed while the first was pending.
        assert_eq!(scheduler.runs_completed(), 1);
    }

    // A schedule arriving AFTER a run has started arms exactly one follow-up run.
    #[test]
    fn a_schedule_after_a_run_arms_one_follow_up() {
        let (_dir, orchestrator) = empty_orchestrator();
        let scheduler = BuildScheduler::with_debounce(orchestrator, Duration::ZERO);
        scheduler.schedule_reindex();
        assert_eq!(scheduler.wait_for_runs(1, Duration::from_secs(5)), 1);
        scheduler.schedule_reindex();
        assert_eq!(scheduler.wait_for_runs(2, Duration::from_secs(5)), 2);
    }

    // Dropping the scheduler stops and joins the worker promptly even with a long
    // debounce (shutdown wakes the debounce wait rather than sleeping it out).
    #[test]
    fn drop_joins_the_worker_without_waiting_out_the_debounce() {
        let (_dir, orchestrator) = empty_orchestrator();
        let scheduler = BuildScheduler::with_debounce(orchestrator, Duration::from_secs(3600));
        scheduler.schedule_reindex();
        // Would hang for an hour if drop waited out the debounce.
        drop(scheduler);
    }
}
