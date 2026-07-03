# scala3-bsp-semantic-ls — TDD Completion Plan (RLCR)

## Goal Description

Close every gap between `plan.md` (the architectural design, §1–§23) and the actual
tree, driven strictly test-first (RED before GREEN), so that a re-run of the
section-by-section adversarial audit finds no `missing` mandate and no undocumented
deviation. The deliverable includes a **mill-based real-BSP end-to-end suite** that
exercises the whole language server against a real `mill --bsp` server, plus a
per-module test-coverage bar. The baseline is already green (1249 Mill tasks, `nix
flake check`, offline package build, and a 6-test real-BSP foundation at
`modules/ls-core/test/src/ls/core/RealBspIntegrationTest.scala`).

Per the resolved user decisions, this completion targets **full, literal plan.md
compliance with no deviations**: synchronous RawSemanticDBPath write-through, a
PC-only workspace-symbol overlay, a real SQLite reader-connection pool, and the full
fuzzy-search + AOT-training + all-benchmark-tier scope (the 100k ingest tier runs only
under `mill bench.run --full`).

Every task is defined by its failing test first; implementation code carries no
plan-document terminology (no "AC-", "Milestone", "Phase", "Step" markers in source or
comments).

---

## Acceptance Criteria

Each criterion lists positive tests (must PASS when the criterion is met) and negative
tests (must FAIL / be rejected when the implementation is correct). Test file names are
targets to create or extend; case names are indicative.

- **AC-1: BSP build diagnostics are forwarded to the LSP client, target-scoped.**
  A `DiagnosticRouter` in `ls-core` keeps per-`(uri, buildTargetId)` state, merges all
  targets' diagnostics into one `textDocument/publishDiagnostics` per uri, and honors
  the bsp4j `reset` flag per target.
  - Positive Tests:
    - `LsEndToEndTest` "compile failure publishes a diagnostic": fake BSP `compile`
      emits `build/publishDiagnostics(uri, target A, Error, range 1:2–1:7, reset=true)`;
      after `executeCommand scala3SemanticLs.compile` the test client received exactly
      one `textDocument/publishDiagnostics` for `file://…` with that range and severity
      Error.
    - "two targets on one uri merge": targets A and B both publish for the same uri; the
      client receives one publish whose list is the union.
    - "fix clears the uri": a second compile emits an empty list with `reset=true` for
      target A; the client receives an empty-list publish for that uri.
  - Negative Tests:
    - "sibling target not cleared": clearing target A (`reset=true`, empty list) while
      target B still has a diagnostic on the same uri MUST leave B's diagnostic visible.
    - "no spurious publish": a compile that reports nothing for an already-clean uri
      produces no publish for that uri.

- **AC-2: Superseded postings segments are reclaimed in production.**
  - AC-2.1 publish-time cleanup:
    - Positive: after three ingests with no retained readers (`IngestJanitorSuite` in
      `ls-rename` + an `ls-core` two-`didSave` e2e), exactly one `segment-*` dir remains
      on disk and doctor `compactionPending == 0`.
    - Negative: while an old snapshot is retained across a publish, its segment dir
      survives and `compactionPending > 0`.
  - AC-2.2 startup janitor:
    - Positive: startup removes `tmp-*` debris and non-active segment dirs; the janitor
      runs only after startup recovery has identified the manifest-active segment.
    - Negative: the manifest-active segment path is never deleted, even when recovery
      failed or the manifest diverged from disk.

- **AC-3: `buildTarget/didChange` reloads the model and re-ingests, safely around bootstrap.**
  - Positive: a `didChange` after initialization triggers a `workspaceBuildTargets`
    refetch and a re-ingest (observable as a segment-generation bump in doctor).
  - Negative: a `didChange` arriving before `CoreServices` is Ready is buffered and
    applied after bootstrap — never dropped, never a crash.

- **AC-4: Export forwarders are grouped and rename-rejected (characterization-first).**
  - Positive:
    - `ScalacIntegrationSuite` "export SemanticDB shape" records the real scalac
      occurrence/symbol shape for an `export` clause (characterization input for the rule).
    - "references include the forwarder": references on the original symbol include the
      `Api.work(1)` forwarder call site (exact uri+span).
    - `RenameSuite` "export rename rejected": rename of the exported symbol yields
      `LsError.RenameRejected` whose message contains "exported symbol".
  - Negative:
    - Rename MUST NOT silently edit only the original and miss the forwarder (it rejects,
      it does not partially edit).

- **AC-5: A synthetic-only symbol is rejected with the concrete synthetic-only reason.**
  - Positive: a fixture that provably yields a synthetic-only occurrence shape in
    SemanticDB; rename yields a reason containing "synthetic"; `UnsafeReason.SyntheticOnly`
    is set at ingest for groups whose editable non-synthetic definition count is zero.
  - Negative: a symbol with any editable non-synthetic definition is NOT flagged
    synthetic-only (falls through to normal rename).

- **AC-6: The references dirty-buffer overlay is keyed by the whole alias group.**
  - Positive: `ReferencesAndQuerySuite` "group-keyed overlay": a stub overlay returning
    an occurrence for a companion-object member is included in references on the class.
  - Negative: the production `PcOverlay.occurrencesOf` stays a documented no-op (mtags
    1.6.7 has no occurrence scan); the group-keyed query is exercised via the SPI stub,
    not silently skipped.

- **AC-7: PC process isolation is the production default (sequenced opt-in first).**
  A `PcBackend` abstraction routes all PC calls through `PcWorkerApi`; `--forked-pc`
  selects the forked backend, `--in-process-pc` the in-process one; the default flips to
  forked only after the real-BSP forked test (AC-16 E7) is stable.
  - Positive:
    - `"--forked-pc boots a child PC JVM"`: completion/hover work; doctor reports
      "forked worker alive".
    - Origin marking (pcOnly detection) survives the worker protocol
      (`WorkerProtocolSuite` "definition origins round-trip").
  - Negative (concrete injection):
    - Obtain the worker pid via a test hook and kill it: the LS process stays alive and
      the very next completion returns the expected item after respawn + buffer replay.
    - A service plugin that throws in `afterCompletion` disables only that plugin (listed
      in `pluginStatus` with the throwable) and the completion still returns.

- **AC-8: SQLite WAL checkpoint scheduling that never blocks the writer.**
  After each publish, run `PRAGMA wal_checkpoint(PASSIVE)` on the index executor; attempt
  `TRUNCATE` only when PASSIVE reports all WAL frames checkpointed (`log == checkpointed`,
  neither `-1`); TRUNCATE is non-blocking and tolerates BUSY as "skip".
  - Positive: `DbSuite` "checkpoint truncates when idle": after a large ingest with no
    reader, a scheduled checkpoint drives `meta.sqlite-wal` to ~0.
  - Negative: `"held reader does not block"`: with a read statement open on another `Db`,
    the PASSIVE checkpoint does not throw, LS writes proceed, and TRUNCATE is skipped
    (or returns BUSY) without blocking.

- **AC-9: A doc→editable-occurrence scan view.**
  - Positive: `HandBuiltCorpusTest` "scanDocEditable == filter": `IndexSnapshot.scanDocEditable`
    equals the brute-force editable filter of `scanDocOccurrences`.
  - Negative: generated/readonly/dependency doc occurrences are excluded from the view.

- **AC-10: Doctor completeness — generated-source status and per-target staleness.**
  - Positive: `RenderTest` asserts a `generated source status: N` line (from
    `documents.generated`) and a `stale targets: …` line; `StoreSectionsTest` matches the
    counts for a store with one generated and one stale-md5 doc.
  - Negative: no `null`/`unavailable` leak when the data is present.

- **AC-11: Fuzzy workspace-symbol matching (camel-hump / subsequence), with a schema migration.**
  Implemented via a normalized-name/initials sidecar (column or companion table) + a
  bounded candidate pull (cap 5000) + in-memory subsequence/camel-hump ranking — NOT
  FTS5 trigram. Requires a SQLite schema v1→v2 migration.
  - AC-11.1 migration:
    - Positive: `MetaStoreSuite` "v1→v2 migration": open a pre-populated `user_version=1`
      DB, migrate (create the sidecar + backfill from `workspace_symbol_rows`, bump to 2),
      idempotent on re-run; a fuzzy search then works.
    - Negative: opening a DB with `user_version > 2` is refused (as v1 already refuses
      unknown versions).
  - AC-11.2 search:
    - Positive: `workspaceSymbol("wSy")` ranks `workspaceSymbol` above weaker matches;
      exact/prefix remains the primary path.
    - Negative: the fuzzy fallback respects the limit and pulls a bounded candidate set
      (no unbounded full scan) on a large corpus.

- **AC-12: Every plan §18.1 correctness case is named by ≥1 test with EXACT expected output.**
  Fixtures compiled by real scalac (`-Xsemanticdb`); each case asserts references AND
  rename (spans or rejection reason). No "pin whatever passes today".
  - Positive (representative exact expectations):
    - inline (B2): `inline def twice` — references == {def site, call in A, call in B};
      rename edits all three tokens (assert spans).
    - case-class synthetic (B3a): cursor on `.copy` receiver — references resolve to the
      copy symbol; rename → `RenameRejected` reason contains "synthetic".
    - derives (B3b): `derives CanEqual` — references on the derived given include the
      derives-clause occurrence (exact uri+span).
    - private member (B4): `private def helper` — references stay in the defining file;
      rename edits def + those uses only.
    - local def (B5): nested `def loop` — references stay document-local.
    - val member (B6): `val label` referenced cross-file — rename edits def + all uses.
    - given/using (B7): `def render(using Core)` — the implicit-argument site yields an
      occurrence of the given and references include it.
    - top-level def/val (B8): cross-file references + rename.
    - opaque type (B9): per the resolved conservative-reject policy — rename →
      `RenameRejected`; references find type and companion uses.
    - extension-method rename (B10): edits definition + `x.doubled` call sites.
    - external reject (B11): cursor on a `scala.collection.immutable.List` reference →
      `RenameRejected` reason contains "outside the workspace".
    - fresh-snapshot StaleIndex (B12): mutating the cursor doc between the compile stub
      and ingest → `LsError.StaleIndex`.
    - manifest→missing-segment recovery (B13): boot a store whose manifest points at a
      deleted segment dir → graceful NotReady degrade + doctor note + heal on next ingest.
  - Negative: each case's opposite (e.g. a private member reference from another file is
    NOT returned; an inline call in an unrelated target without a dependency edge is NOT
    returned).

- **AC-13: Every plan §18.3 benchmark exists with a ground-truth consistency check.**
  Rows: SemanticDB ingest at 1k / 10k / 100k (real `.semanticdb` corpus; 100k under
  `--full` only), cold vs warm start, BSP import (fake server, N targets), rename
  small/large, PC completion P50/P95/P99 + plugin overhead, references rare/medium/hot,
  SQLite FFM call-overhead microbench, fuzzy vs prefix, and the occurrence-set-preservation
  gate. (mmap-scan records/sec is already emitted by `doc scan (full)` + the reference
  rows.)
  - Positive: `mill bench.smoke` (< 60 s) includes the ingest-smoke, cold/warm, rename,
    and fuzzy rows; `mill bench.run --full` executes the 10k/100k tiers.
  - Negative: any row whose measured result disagrees with the generator's ground truth
    exits non-zero (no silent pass).

- **AC-14: AOT cache training mode covering the plan §16.3 workload.**
  - Positive: `AotTrainIntegrationTest` (gated `LS_AOT_IT=1`) + `scripts/aot-train.sh`
    produce a non-empty cache; a follow-up `Main --version` run with `-XX:AOTCache`
    succeeds and doctor reports "AOT cache: loaded". Flags verified against the flake's
    actual JDK 25 first (an `analyze` task).
  - Negative: the training run exits cleanly with no `.bsp` present (fake-BSP fallback),
    with no hang.

- **AC-15: Remaining runtime/ops mandates.**
  - AC-15.1 JFR named preset: `BenchSuite` "--jfr uses a preset": running with
    `--jfr out.jfr --jfr-preset profile` yields a recording containing JVM profiling
    events; default preset is `default`.
  - AC-15.2 `snapshots/current.json`: written atomically on publish, containing
    `{segmentId, path, publishedAtMs, generation}`; recovery cross-checks it against the
    SQLite manifest and doctor reports `snapshot file: consistent|divergent`. Negative: a
    divergent file is reported, not silently trusted.
  - AC-15.3 Java-25 guards: `sqliteFfm` and `postings` test roots assert
    `Runtime.version().feature() == 25`; a wrong-JDK env fails those modules loudly.
  - AC-15.4 offline-CI compile guard: `scripts/check-offline-compile.sh` seeds a temp
    coursier cache from the flake `ivyCache` and runs `mill --no-daemon __.compile`
    offline; `--self-test` appends an unlocked dep to a scratch copy and expects failure.

- **AC-16: The mill-based real-BSP end-to-end suite (LS-wide acceptance).**
  All tests drive the real `mill --bsp` server through the LSP protocol; the sample
  workspace under `it/sample-workspace` is extended as needed (< 10 files). Suites split
  into `RealBspCoreTest`, `RealBspLifecycleTest`, `RealBspIsolationTest`, sharing one
  lazily-initialized workspace fixture; budget ≤ 5 min warm. Gated by `LS_REAL_BSP_IT=1`.
  - Positive (each an e2e case): E1 IndexUnavailable target (module `c` without
    `-Xsemanticdb`; doctor lists exactly {mill-build, c}; PC completion in `c` still
    works); E2 diagnostics forwarding on a real compile error (needs AC-1); E3
    `didSave`→compile→reingest reflects new token positions with no explicit reindex; E5
    hover/signatureHelp/definition/documentHighlight; E6 shared source across targets
    unifies references and passes rename consistency; E7 forked PC over real BSP survives
    a worker kill (needs AC-7); E8 segment hygiene + warm restart serves references before
    any BSP compile completes (needs AC-2, AC-15.2); E9 AOT-trained boot (gated
    `LS_AOT_IT=1`, needs AC-14).
  - Negative (concrete injections): E4a rename to an invalid identifier (`class`) over the
    real server → exact reject message; E4b rename in the no-semanticdb module `c` →
    IndexUnavailable; E2b a fixed-and-saved file publishes an empty diagnostic list.
  - Suite-level: E10 CI wiring — `.github/workflows/ci.yml` gains a `real-bsp-e2e` job
    running `scripts/it-real-bsp.sh`; two consecutive green runs establish flake-freedom
    (any flaky test gets a deflake fix, never a retry loop).

- **AC-17: Documentation reconciled with a mechanical checker.**
  - Positive: F1 purges stale/false claims (nix-build "Current gaps", `.#mill` export
    contradiction, three-vs-four checks, `mill -i` vs `--no-daemon`, AOT overclaim until
    AC-14 lands; architecture storage layout, "SemanticDB watcher"→"full rescan (v1)",
    `contentless_delete=1` note, the resolved deviations); F2 adds
    `docs/traceability.md` mapping every §13.1 rule / §18.1 case / §18.3 bench to a test
    file + case name; F3 `scripts/check-docs.sh` passes.
  - Negative: `check-docs.sh` FAILS on today's docs before F1 (RED), and FAILS if any
    test file/case named in `traceability.md` does not exist.

- **AC-18: Synchronous per-doc write-through on the RawSemanticDBPath (resolved DEC-1).**
  When symbol-at-cursor falls to the RawSemanticDBPath (stale/missing index), the parsed
  and md5-validated document is written into the index synchronously on the index
  executor (intern symbols, update metadata + FTS, rebuild and publish a segment
  reflecting the refreshed doc) before the request returns.
  - Positive: `RawPathSuite` "write-through updates the index": after a raw-path
    reference query for a doc, a subsequent symbol-at-cursor for the same uri resolves via
    `ResolutionSource.Snapshot` with `needsReindex == false`, and no debounced job is
    required to have run.
  - Negative: a raw-path hit for a doc whose on-disk source md5 does not match its
    `.semanticdb` does NOT write through (it is served/flagged, and the index is not
    corrupted); write-through never runs off the index executor (single-writer contract
    preserved).

- **AC-19: PC-only workspace-symbol dirty-buffer overlay (resolved DEC-4, plan §11).**
  `workspace/symbol` merges SQLite-FTS results with symbols from open-but-unsaved buffers,
  the latter marked PC-only (excluded from global references/rename).
  - Positive: `PcOverlaySuite`/`LsEndToEndTest` "unsaved symbol surfaces PC-only": opening
    a buffer that adds a top-level `object NewThing` makes `workspace/symbol("NewThing")`
    return it flagged PC-only; a global references/rename request on it is refused with
    `LsError.PcOnlySymbol`.
  - Negative: after save + ingest, the same symbol appears as a normal (non-PC-only)
    result; a symbol present only in the persisted index is never marked PC-only.

- **AC-20: SQLite reader-connection pool (resolved DEC-5, plan Phase 4).**
  A bounded pool of read-only `Db` connections serves concurrent read paths; the writer
  `Db` remains single-threaded and is never borrowed from the pool.
  - Positive: `DbSuite`/`MetaStoreSuite` "concurrent readers": N threads borrow read
    connections and run FTS queries with correct results; the pool caps at its max size
    (excess borrowers queue); returned connections are reused; `close()` frees every
    arena (post-close use throws).
  - Negative: a borrowed connection is never handed to two threads at once; the writer
    connection is never served from the reader pool.

---

## Path Boundaries

### Upper Bound (Maximum Acceptable Scope)
All of AC-1 … AC-20 implemented and tested, with: the real-BSP end-to-end suite (AC-16)
wired into CI; forked PC as the production default after E7 stabilizes (AC-7); fuzzy
workspace-symbol search with its schema migration (AC-11); synchronous RawSemanticDBPath
write-through (AC-18); the PC-only workspace-symbol overlay (AC-19); a real SQLite
reader-connection pool (AC-20); AOT training (AC-14); every §18.3 benchmark tier
including 100k under `--full` (AC-13); and docs reconciled with the mechanical checker
(AC-17). A re-run of the six-auditor audit reports no `missing` and no undocumented
deviation.

### Lower Bound (Minimum Acceptable Scope)
Because the goal is audit-clean completion and the user selected full compliance, the
lower bound is the **completion-convergence minimum**: every AC-1 … AC-20 satisfied at
its minimum test form (single representative fixture per §18.1 case; smoke-tier
benchmarks present with the 10k/100k tiers gated behind `--full`; the real-BSP suite's
core cases E1/E4/E5 plus the lifecycle/isolation cases their dependencies unlock). No
acceptance criterion may be dropped; the only latitude is depth of parametrization (e.g.
number of fuzzy ranking fixtures, benchmark corpus sizes at the smoke tier) so long as
each AC's positive and negative tests pass.

A useful intermediate **minimum shippable milestone** (not the completion bar) is:
AC-1, AC-2, AC-3, AC-4, AC-5, AC-8, AC-9, AC-10, AC-18, AC-20 (production-correctness
wiring) + AC-12 (§18.1 coverage) + AC-16 core (E1/E4/E5) + AC-17 (docs) — shippable value
before the heavier AC-11/AC-13-full/AC-14/AC-19 land.

### Allowed Choices
- Can use: a new `ls-core` `DiagnosticRouter`; a SQLite sidecar column or companion table
  plus a v1→v2 migration for fuzzy; an in-memory camel-hump/subsequence ranker; a JFR
  `.jfc` preset resource; a bounded read-only `Db` pool with per-connection confined
  arenas; a `PcBackend` seam over the existing `PcWorkerApi`; scalac-generated fixtures
  and the existing in-process fake BSP server for unit/e2e tests; the real `mill --bsp`
  server for the gated end-to-end suite.
- Cannot use: FTS5 trigram as the primary fuzzy mechanism; any global-truth source other
  than scalac SemanticDB (no grep, Bloom filter, or syntax approximation for correctness);
  a PC-derived persistent index; time-based (non-drain-aware) segment deletion; retry
  loops that mask end-to-end flakes; a blocking `TRUNCATE` checkpoint on the writer under
  a held reader; any implementation code or comment containing plan-document markers
  ("AC-", "Milestone", "Phase", "Step").

> **Note on Deterministic Designs**: `plan.md` fixes the core architecture (SemanticDB as
> the only global-truth source; SQLite + mmap postings as materializations; Nix + Mill +
> mill-ivy-fetcher; Java 25; Scala 3; strict TDD). Within those fixed constraints the
> above choices are the remaining latitude; the four previously-open decisions were
> resolved to the strict/literal option in every case, so the plan implements plan.md
> without deviation.

---

## Feasibility Hints and Suggestions

> Reference only — conceptual, not prescriptive.

### Conceptual Approach
- **AC-1 DiagnosticRouter**: keep `Map[(uri, targetId), List[Diagnostic]]`; on each BSP
  publish, replace the entry for `(uri, target)` (or clear it on `reset` with an empty
  list), recompute the union for that uri, and push one LSP publish. Convert bsp4j ranges
  in `LspConvert`.
- **AC-2 janitor split**: call `SnapshotManager.deleteSuperseded()` at the tail of a
  successful `IngestPipeline` publish; add a separate startup pass that, after recovery
  has resolved the manifest-active segment, deletes only other `segment-*` dirs and
  `tmp-*` debris, guarding the active path unconditionally.
- **AC-8 checkpoint**: `PRAGMA wal_checkpoint(PASSIVE)` returns `(busy, log, checkpointed)`;
  treat "all frames done" as `log == checkpointed` and `log != -1`; only then issue
  `TRUNCATE`, tolerating a BUSY row as skip. Run on the single index executor so it never
  races the writer.
- **AC-11 fuzzy**: store a normalized name + camel-hump initials alongside each workspace
  symbol row; on a non-prefix query pull a bounded candidate set via the sidecar, then
  rank in memory by subsequence match + camel-hump bonus; migrate with an idempotent
  v1→v2 step guarded by `PRAGMA user_version`.
- **AC-18 write-through**: reuse the ingest building blocks to persist a single refreshed
  document and republish a segment on the index executor, so the next query resolves via
  the snapshot rather than the raw path.
- **AC-19 overlay**: extract top-level symbols from open unsaved buffers (via the PC or a
  light top-level scan), tag them PC-only, and merge into `workspace/symbol`; refuse global
  references/rename on PC-only symbols with `LsError.PcOnlySymbol`.
- **AC-20 reader pool**: a fixed-size pool of `Db` connections opened read-only, each with
  its own confined arena; borrow/return with a semaphore; the writer stays separate.
- **AC-16 real-BSP suite**: extend `it/sample-workspace` and split
  `RealBspIntegrationTest` into core/lifecycle/isolation suites sharing one workspace
  fixture; account for mill's separate `.bsp/out` output dir and the `mill-build`
  meta-target already documented in the foundation.

### Relevant References
- `modules/ls-core/src/ls/core/WorkspaceState.scala` — bootstrap, BSP handlers, recovery.
- `modules/ls-core/src/ls/core/ScalaLs.scala` — capabilities, request dispatch, client.
- `modules/ls-rename/src/ls/rename/{QueryOrchestrator,ReferencesEngine,RenameEngine,IngestPipeline}.scala`.
- `modules/ls-postings/src/ls/postings/SnapshotManager.scala` — `deleteSuperseded`.
- `modules/ls-sqlite-ffm/src/ls/sqlite/{Db,MetaStore,Schema}.scala` — checkpoint, pool, migration.
- `modules/ls-semanticdb/src/ls/semanticdb/groups.scala` — alias grouping (export/opaque).
- `modules/ls-pc/src/ls/pc/{PcFacade,ForkedPcWorker,PcWorkerApi,worker}.scala` — PcBackend.
- `modules/ls-bench/src/ls/bench/{BenchMain,Corpus}.scala` — benchmark harness.
- `modules/ls-core/test/src/ls/core/RealBspIntegrationTest.scala`, `it/sample-workspace/`,
  `scripts/it-real-bsp.sh` — real-BSP foundation to extend.
- `docs/{architecture,index-format,nix-build}.md`, `plan.md §5.1/§9.3/§10/§11/§16.3/§18/§20`.

---

## Dependencies and Sequence

### Milestones
1. **Production-correctness wiring** (independent, start first)
   - Phase A: AC-1 (diagnostics), AC-2 (janitor split), AC-3 (didChange), AC-8
     (checkpoint), AC-9 (editable scan), AC-10 (doctor), AC-18 (write-through), AC-20
     (reader pool), AC-15.2/15.3 (current.json, JDK guards).
   - These share no ordering constraints except AC-18 depends on the ingest building
     blocks and AC-2's publish hook.
2. **Semantic layer** (depends on Milestone 1's ingest touches where noted)
   - Phase A: AC-4 characterization (`analyze`) → AC-4 grouping + AC-5 synthetic flag.
   - Phase B: AC-6 group-keyed overlay; then AC-12 §18.1 cases in fixture batches
     (B2–B6 / B7–B10 / B11–B13).
3. **Backends, search, and benchmarks**
   - Phase A: AC-7 (PcBackend + forked, after AC-6); AC-11 (fuzzy: migration → search),
     AC-19 (overlay).
   - Phase B: AC-13 benchmarks (fuzzy row after AC-11); AC-14 AOT (after an `analyze`
     flag-verification against the flake JDK 25); AC-15.1/15.4 (JFR preset, offline guard).
4. **End-to-end and documentation**
   - Phase A: AC-16 real-BSP suite in dependency order (E1/E4/E5, then E2/E3/E6/E8 after
     AC-1/AC-2/AC-15.2, then E7/E9 after AC-7/AC-14), E10 CI last.
   - Phase B: AC-17 docs (F1 purge, F2 traceability updated by every B/C task, F3 checker).

Relative dependencies: AC-18 → ingest hooks; AC-2.1 → publish tail; AC-7 → AC-6 + the
worker origin protocol; AC-11.2 → AC-11.1 migration; AC-13 fuzzy row → AC-11; AC-14 →
flag verification; E2/E3 → AC-1/AC-2; E7 → AC-7; E8 → AC-2/AC-15.2; E9 → AC-14; F3 → F1/F2.

---

## Task Breakdown

Each task carries exactly one routing tag (`coding` or `analyze`).

| Task ID | Description | Target AC | Tag | Depends On |
|---------|-------------|-----------|-----|------------|
| T-A1 | DiagnosticRouter + BSP→LSP publishDiagnostics, target-scoped | AC-1 | coding | - |
| T-A2a | Call deleteSuperseded at publish tail | AC-2.1 | coding | - |
| T-A2b | Startup manifest-aware janitor (guards active segment) | AC-2.2 | coding | T-A2a |
| T-A3 | Wire buildTarget/didChange: buffer + reload + reingest | AC-3 | coding | - |
| T-A4c | Characterize real scalac SemanticDB for `export` | AC-4 | analyze | - |
| T-A4 | Export-forwarder grouping + UnsupportedSymbolFamily flag | AC-4 | coding | T-A4c |
| T-A5 | Set SyntheticOnly at ingest; reachable reject | AC-5 | coding | - |
| T-A6 | References overlay keyed by alias group | AC-6 | coding | - |
| T-A7 | PcBackend seam; forked backend; sequenced default flip | AC-7 | coding | T-A6 |
| T-A8 | Db.checkpoint(PASSIVE/TRUNCATE) scheduling | AC-8 | coding | - |
| T-A9 | IndexSnapshot.scanDocEditable view | AC-9 | coding | - |
| T-A10 | Doctor generated-source + per-target staleness lines | AC-10 | coding | - |
| T-A11m | SQLite v1→v2 migration for fuzzy sidecar | AC-11.1 | coding | - |
| T-A11 | Fuzzy candidate pull + camel-hump ranking; wire workspace/symbol | AC-11.2 | coding | T-A11m |
| T-A12..A23 | §18.1 fixture cases (inline, synthetic, derives, private, local def, val, given/using, top-level, opaque, extension rename, external reject, stale-branch, missing-segment recovery) | AC-12 | coding | T-A4,T-A5 |
| T-A18 | Synchronous RawSemanticDBPath write-through | AC-18 | coding | T-A2a |
| T-A19 | PC-only workspace-symbol overlay | AC-19 | coding | - |
| T-A20 | Bounded read-only Db connection pool | AC-20 | coding | - |
| T-C1..C8 | §18.3 benchmark rows + consistency gate | AC-13 | coding | T-A11 |
| T-D1v | Verify AOT flags against the flake's JDK 25 | AC-14 | analyze | - |
| T-D1 | Main --aot-train + scripts/aot-train.sh | AC-14 | coding | T-D1v |
| T-D2 | JFR named preset + .jfc resource | AC-15.1 | coding | - |
| T-D3 | snapshots/current.json atomic write + divergence check | AC-15.2 | coding | - |
| T-D4 | Java-25 guards in sqliteFfm/postings test roots | AC-15.3 | coding | - |
| T-D5 | scripts/check-offline-compile.sh (+ --self-test) | AC-15.4 | coding | - |
| T-Bsp | Optional buildTarget/dependencySources + outputPaths calls | AC-3 | coding | - |
| T-E1..E10 | Real-BSP end-to-end suite (split core/lifecycle/isolation) + CI job | AC-16 | coding | T-A1,T-A2b,T-A7,T-D1,T-D3 |
| T-F1 | Purge stale/false docs claims | AC-17 | coding | - |
| T-F2 | docs/traceability.md mapping tables | AC-17 | coding | (all B/C) |
| T-F3 | scripts/check-docs.sh mechanical checker | AC-17 | coding | T-F1,T-F2 |

---

## Claude-Codex Deliberation

### Agreements
- The audit's nine major gaps are real; the completion must be strictly test-first.
- The mill-based real-BSP suite is the LS-wide acceptance spine.
- Global truth stays SemanticDB-only; SQLite + mmap are materializations.
- Plan §18.3 "mmap scan records/sec" is already satisfied by the existing `doc scan (full)`
  and reference-scan bench rows (recorded in traceability, no new task).

### Resolved Disagreements
- **Diagnostics routing (AC-1)**: Claude initially proposed per-uri last-published state;
  Codex showed shared-source/multi-target uris need `(uri, targetId)` scoping (bsp4j
  `PublishDiagnosticsParams` carries `buildTarget` + `reset`, verified). Resolution:
  target-scoped `DiagnosticRouter` merged per uri.
- **Segment cleanup (AC-2)**: single "invoke deleteSuperseded" split into publish-time
  drain-aware cleanup + a startup manifest-aware janitor; `compactionPending == 0` is a
  steady-state (no-retained-reader) assertion.
- **Export detection (AC-4)**: added a characterization test first (record real scalac
  SemanticDB) before deriving the grouping rule — proper TDD rather than a guessed rule.
- **Fuzzy search (AC-11)**: rejected FTS5 trigram (needs contiguous 3-char evidence;
  camel-hump is non-contiguous) in favor of a normalized/initials sidecar + bounded pull +
  in-memory ranking; moved out of "benchmarks" into a feature task with a v1→v2 migration.
- **WAL checkpoint (AC-8)**: corrected the semantics — `PASSIVE` does not report `busy`
  for held readers; gate `TRUNCATE` on all frames checkpointed (`log == checkpointed`),
  tolerate BUSY as skip, never block the writer.
- **lsp4j version**: do NOT bump declared `0.24.0` to the resolved `1.0.0` (risky churn);
  keep the documented eviction + lock verification.
- **Forked-PC default (was DEC-2)**: both reviewers agreed on opt-in `--forked-pc` first,
  flipping the default after the real-BSP forked test stabilizes — resolved, not pending.
- **Opaque/export rename policy (was DEC-3)**: both agreed conservative reject per plan
  §6.2 — resolved, not pending.
- **AC-7/AC-12/AC-16 test concreteness**: replaced "pin whatever passes today" and "fails
  loudly on any regression" with exact expected references/spans/rejection reasons and
  concrete failure injections (kill the worker pid; rename to `class`; rename in the
  no-semanticdb module).
- **Path boundaries**: restructured into a completion-convergence lower bound (no AC
  dropped) plus a separate minimum-shippable milestone.

### Convergence Status
- Final Status: `converged` (two Codex rounds; the only round-2 blocker — the AC-8
  checkpoint semantics — was corrected as above; no REQUIRED_CHANGES remain).

---

## Pending User Decisions

All four decisions were resolved by the user during planning; none remain PENDING.

- DEC-1: RawSemanticDBPath behavior.
  - Claude Position: keep serve-raw + scheduled reindex, documented as a deviation.
  - Codex Position: either, owner call.
  - Tradeoff Summary: strict §10/§12.1 write-through gives immediate consistency at the
    cost of a per-request write path in the single-segment model; the deferred path is
    simpler but only eventually consistent.
  - Decision Status: **RESOLVED — implement synchronous per-doc write-through (AC-18).**

- DEC-2: Forked PC as production default.
  - Claude Position / Codex Position: opt-in `--forked-pc` first, flip default after CI
    stabilizes.
  - Tradeoff Summary: immediate §5.2 compliance vs. lower breakage risk.
  - Decision Status: **RESOLVED during convergence — sequenced opt-in-first then default
    flip (AC-7).**

- DEC-3: Opaque-type & export rename policy.
  - Claude Position / Codex Position: conservative reject (plan §6.2).
  - Tradeoff Summary: safety-first rejection vs. attempting full grouped edits.
  - Decision Status: **RESOLVED during convergence — conservative reject (AC-4, AC-12/B9).**

- DEC-4: Workspace-symbol dirty-buffer overlay (plan §11).
  - Claude Position: defer + document (mtags exposes no workspace-symbol provider).
  - Codex Position: owner call whether required for v1.
  - Tradeoff Summary: literal §11 coverage vs. a bespoke buffer scanner for modest value.
  - Decision Status: **RESOLVED — implement the PC-only overlay now (AC-19).**

- DEC-5: SQLite connection model (plan Phase 4 "connection pool").
  - Claude Position / Codex Position: documented single-writer deviation.
  - Tradeoff Summary: correct single-writer simplicity vs. literal Phase-4 pool wording.
  - Decision Status: **RESOLVED — implement a real reader-connection pool (AC-20).**

- DEC-6: Completion scope for fuzzy / full benchmark tiers / AOT training.
  - Claude Position: keep AC-11 + AC-14 in scope; 100k tier `--full`-only.
  - Codex Position: flagged for explicit owner scoping.
  - Tradeoff Summary: fully audit-clean completion vs. a faster shippable core.
  - Decision Status: **RESOLVED — keep all in scope; 100k benchmark tier runs under
    `--full` only (AC-11, AC-13, AC-14).**

---

## Implementation Notes

### Code Style Requirements
- Implementation code and comments MUST NOT contain plan-document terminology such as
  "AC-", "Milestone", "Phase", "Step", "DEC-", or task ids ("T-A1"); these belong to this
  plan only. Use descriptive, domain-appropriate names in source.
- Follow the existing module conventions: Scala 3 indentation syntax, munit tests forked
  on JDK 25, opaque id types from `ls-index-model`, `LsError` for typed failures, no
  approximate/global-truth shortcuts.
- Every task lands its RED test in the same change as its implementation; the test must
  fail if the implementation is reverted.

### TDD Working Rules (binding)
1. RED first: write the specified test, run it, confirm it fails for the stated reason.
2. GREEN: minimum implementation that passes without breaking existing tests.
3. Acceptance: run the task's gate commands and record output.
4. Docs: any newly discovered deviation is documented in `docs/*` in the same change
   (subset enforced by `scripts/check-docs.sh`).
5. Full gates before merge:
   `nix develop -c mill __.compile + __.test + bench.smoke`; `nix flake check`;
   `nix develop -c ./scripts/check-ivy-lock.sh`; `nix develop -c ./scripts/check-docs.sh`;
   and for end-to-end tasks `nix develop -c ./scripts/it-real-bsp.sh`.

### Per-module "enough tests" bar (binding coverage matrix)
- M1: every public API entry point is called by ≥1 test in its own module.
- M2: every `LsError` variant is produced through a real code path.
- M3: every `UnsafeReason` bit has a producing ingest/engine test (all bits after AC-4/AC-5).
- M4: every on-disk format/schema element has a write→read round-trip test and ≥1
  corruption/rejection test (including the v1→v2 migration for AC-11).
- M5: every advertised LSP capability has a fake-BSP e2e test AND a real-BSP e2e test.

### Definition of Done
The full gate set passes on a clean clone in `nix develop` with a cold coursier cache; a
re-run of the six-auditor section-by-section audit reports no `missing` mandate and no
undocumented deviation; `docs/traceability.md` maps every plan §13.1 rule, §18.1 case, and
§18.3 benchmark to a named test.

--- Original Design Draft Start ---

# plan2.md — scala3-bsp-semantic-ls 完成计划 (TDD Completion Plan)

> Status: normative completion plan, derived from the 2026-07-03 adversarial audit
> Audience: project owner, implementers
> Relationship to plan.md: plan.md remains the architectural truth. plan2.md is the
> verification-driven plan that closes every gap between plan.md and the tree.
> Method: **strict TDD** — every task below defines its failing test(s) FIRST.
> No task may land implementation code without the RED test in the same change.

---

## 0. Baseline: what the audit established

Audit method: six adversarial auditors swept every mandate of plan.md §1–§23
against the tree at commit `0fede35`; one integration agent built the real-BSP
end-to-end proof. Result: 56 findings that are not cleanly "implemented".

Already true (verified, keep green):

```text
1249/1249 mill tasks green (50 suites, ~230 tests) + bench.smoke
nix flake check green (toolchain, lock parse, input pin, full offline package)
Offline nix build .#default; wrapper runs on JDK 25
RealBspIntegrationTest: 6/6 against real `mill --bsp` (gated LS_REAL_BSP_IT=1)
Sections fully holding: §7 schema, §8 postings format, §9 snapshot/epoch,
§10 three paths, §12 references core, §13 rename core, §14 SPI, §17 items 1–15
```

Major gaps (must close — each has a task below):

```text
G1  BSP diagnostics never forwarded to the LSP client          -> A1
G2  Superseded-segment cleanup implemented but never invoked   -> A2
G3  PC runs in-process; plan 5.2 mandates separate JVM         -> A7
G4  SQLite WAL checkpoint scheduling absent                    -> A8
G5  Export forwarders: no grouping/unsafe flag, no tests       -> A4 + B1
G6  §18.1 cases untested: inline, macro-generated, private,
    local def, val member, using, opaque, top-level def        -> B2–B9
G7  §18.3 benchmarks missing: ingest 1k/10k/100k, cold/warm
    start, BSP import, rename small/large, PC completion
    percentiles + plugin overhead, fuzzy workspace symbol      -> C1–C7
G8  AOT training mode absent (docs overclaim it)               -> D1
G9  Real-BSP e2e exists but covers only the happy path         -> E1–E9
```

---

## 1. TDD working rules (binding for every task)

```text
1. RED first: write the test exactly as specified, run it, confirm it fails
   for the stated reason (not a compile error in the test itself).
2. GREEN: implement the minimum that makes the test pass without breaking
   any existing test.
3. Acceptance: run the task's acceptance commands verbatim; paste output in
   the PR/commit description.
4. One task = one commit (or one reviewable unit); the RED test and the
   implementation land together; the test must FAIL if the implementation
   is reverted.
5. No new public API without a test that calls it.
6. Deviations from plan.md discovered mid-task are recorded in
   docs/architecture.md (or index-format.md / nix-build.md) in the same
   commit — the docs checker (F3) enforces a subset mechanically.
7. Full gates before merge to main:
     nix develop -c mill __.compile + __.test + bench.smoke
     nix flake check
     nix develop -c ./scripts/check-ivy-lock.sh
   and for E-tasks: nix develop -c ./scripts/it-real-bsp.sh
```

Test placement conventions (unchanged): `modules/<dir>/test/src/ls/<pkg>/…`,
munit, forked on JDK 25. Real-BSP e2e tests live in
`modules/ls-core/test/src/ls/core/RealBspIntegrationTest.scala` (and siblings),
gated by `LS_REAL_BSP_IT=1`, sample project under `it/sample-workspace/`.

---

## 2. Workstream A — production wiring gaps

### A1. Forward BSP diagnostics to the LSP client  [MAJOR, plan §5.1/§4.1]

Audit: `Bootstrap` wires only `onLogMessage`/`onServerStderr`; every
`build/publishDiagnostics` is dropped; `ScalaLs` stores the `LanguageClient`
but never calls `publishDiagnostics`.

RED (write first):
- `LsEndToEndTest`: new case `"buildTarget/compile failure publishes diagnostics to the client"` —
  extend the fake BSP server so `buildTarget/compile` emits one
  `build/publishDiagnostics` (uri = fixture file, range 1:2–1:7, severity Error,
  `reset=true`) before returning statusCode ERROR. Drive
  `executeCommand scala3SemanticLs.compile`; assert the test client's collected
  `textDocument/publishDiagnostics` contains exactly that uri (converted to
  `file://`), that range, severity Error. Fails today: client collection empty.
- Second case `"diagnostics are replaced on the next compile"`:
  second compile emits zero diagnostics for the same uri with `reset=true`;
  assert the client received an empty-list publish clearing the uri.

GREEN:
- `Bootstrap.run`: wire `handlers.onDiagnostics` → convert
  `bsp4j PublishDiagnosticsParams` (uri, range, severity, code, message,
  reset flag) → lsp4j → `client.publishDiagnostics`, with per-uri replace
  semantics honoring `reset`; keep a per-uri last-published set so a target
  recompile clears stale entries even when the server omits empty publishes.
- Conversion lives in `LspConvert`; unit-test the mapping edge cases
  (missing severity, multi-line range) in `LspConvertSuite`.

Acceptance: `nix develop -c mill core.test` and E2E task E2 (real compiler error).

### A2. Actually invoke superseded-segment cleanup  [MAJOR, plan §9.3/Phase 10]

Audit: `SnapshotManager.deleteSuperseded()` is tested but has zero production
callers; every didSave re-ingest leaks a segment directory.

RED:
- `ReferencesAndQuerySuite` (or new `IngestJanitorSuite` in ls-rename): case
  `"publish prunes drained superseded segments"` — ingest, re-ingest twice with
  no retained readers, then assert `segmentsDir` contains exactly one
  `segment-*` dir (the active one). Fails today: three dirs remain.
- Case `"held snapshots delay pruning until release"` — retain the old snapshot
  across a re-ingest; assert its dir survives; release; trigger one more
  publish (or an explicit janitor tick); assert it is gone.
- `ls-core` e2e: after two `didSave` cycles in `LsEndToEndTest`, assert on-disk
  segment count == 1 (exposes the wiring, not just the pipeline).

GREEN: call `manager.deleteSuperseded()` at the end of every successful
`IngestPipeline.ingest` publish, plus once during startup recovery for
non-active leftover dirs (`tmp-*` debris included). Doctor's
`compactionPending` should read 0 in steady state — assert that in the
doctor store test.

### A3. Consume buildTarget/didChange  [recommended, plan §4.1]

RED: `LsEndToEndTest` case `"buildTarget/didChange triggers model reload and reingest"` —
fake server sends `onBuildTargetDidChange` after initialization; assert the
server re-fetched `workspaceBuildTargets` (fake counts calls) and scheduled a
re-ingest (observable via ingest generation bump in doctor/segment id).
GREEN: wire `handlers.onDidChangeBuildTarget` in `Bootstrap` to re-run
`ProjectModelLoader.load`, rebuild `WorkspaceTargets`, refresh PC target
configs, and enqueue the existing debounced re-ingest job.

### A4. Export forwarders: exact grouping + unsafe rename family  [MAJOR, plan §6.2]

Audit: no export handling exists; `UnsafeReason.UnsupportedSymbolFamily` has
no producer anywhere.

RED (ls-semanticdb first):
- `ScalacIntegrationSuite`: fixture

  ```scala
  object Impl { def work(x: Int): Int = x }
  object Api { export Impl.work }
  val use = Api.work(1)
  ```

  Case `"export forwarder call sites join the original's ref group"`: the
  occurrence at `Api.work(1)` (bound to the forwarder symbol) and the export
  clause occurrence resolve into the same REF group as `Impl.work`.
  Case `"export forwarder marks the rename group UnsupportedSymbolFamily"`:
  the rename group's semantic mask has the bit set.
- `ls-rename` `RenameSuite`: `"rename of an exported symbol is rejected with the exported-symbol reason"` —
  cursor on `Impl.work` definition; expect `LsError.RenameRejected` whose
  message contains "exported symbol" (from `UnsafeReason.explain`).
- `ReferencesAndQuerySuite`: `"references through an export forwarder are found"` —
  references on `Impl.work` include the `Api.work(1)` site.

GREEN: in `AliasGroupBuilder`, detect forwarders (SymbolInformation of the
export target shares display name + `overriddenSymbols`/owner shape per real
scalac output — derive the exact rule from the fixture's parsed SemanticDB,
not from guesswork) and (a) merge forwarder into the original's ref group,
(b) set `UnsupportedSymbolFamily` on the merged rename group.
`RenameProfileBuilder` carries the bit through; no engine change needed.

### A5. Make SyntheticOnly rejection reachable  [minor, plan §13.1]

Audit: synthetic occurrences never enter rename postings, so the concrete
`SyntheticOnly` rejection at `RenameEngine` is dead code; such groups fall
through to the generic "no editable occurrences".

RED: `RenameSuite` case `"synthetic-only symbol is rejected with the synthetic-only reason"` —
cursor on a case-class `copy` reference (synthetic definition, plan §18.1
macro-generated kin); expect `RenameRejected` message to contain "synthetic".
GREEN: at ingest, set `UnsafeReason.SyntheticOnly` on rename groups whose
occurrence set contains no editable non-synthetic definition; keep the engine
check as the enforcement point.

### A6. Overlay contributions keyed by alias group  [minor, plan §12.3]

Audit: `ReferencesEngine` asks the overlay for one symbol, not the group;
production `PcOverlay.occurrencesOf` is a permanent no-op (undocumented).

RED: `ReferencesAndQuerySuite` case `"overlay hits for any alias-group member are merged"` —
stub overlay returns an occurrence only for the companion-object member;
references on the class must include it. Fails today (only the cursor symbol
is queried).
GREEN: query the overlay once per rename/ref-group member (bounded by group
size), dedupe. Document in docs/architecture.md that the production PC overlay
contributes symbol-at-cursor only (mtags 1.6.7 has no occurrence scan) and the
occurrence hook is exercised via SPI.

### A7. PC worker separate JVM as production default  [MAJOR, plan §5.2]

Audit: `ForkedPcWorker`/`PcWorkerMain` exist and are tested, but `Main` is
"--in-process-pc (default and only mode)". Plan §5.2 mandates process isolation.

RED:
- `ls-core` unit: `"--forked-pc constructs a forked worker backend"` — flag
  parsing + `WorkspaceState` uses a `PcBackend` abstraction whose forked
  variant reports `workerAlive = Some(true)` in doctor input.
- E2E (fake BSP, in `LsEndToEndTest` or new `ForkedPcE2eTest`, may be gated
  `LS_FORKED_PC_IT=1` if runtime > 30s): boot with forked mode; assert
  completion/hover work through the worker JVM; kill the worker process
  (obtain pid via test hook); assert the next completion succeeds after
  respawn+replay and the LS process never died.
- Doctor: `PcSection` renders "forked worker alive".

GREEN: introduce `PcBackend` in ls-core routing all PC calls through
`PcWorkerApi` (both `InProcessPcWorker` and `ForkedPcWorker` implement it).
Extend `PcWorkerApi`/`PcWorkerDefinitionResult` if the overlay's origin marking
(pcOnly detection) is not yet representable over the wire — with a
`WorkerProtocolSuite` RED test for origins first. Default mode: **forked**;
`--in-process-pc` stays for tests/debug. Update `Main` help text,
docs/architecture.md §3.4, docs/plugin-spi.md.

### A8. SQLite WAL checkpoint scheduling  [MAJOR, plan Phase 10]

RED: `ls-sqlite-ffm` `DbSuite` case `"explicit checkpoint truncates the WAL"` —
write a few thousand rows in transactions, capture `meta.sqlite-wal` size,
call the new `Db.checkpoint(Truncate)`, assert `PRAGMA wal_checkpoint` result
row is ok (busy=0) and the wal file shrank to 0/near-0. Case
`"checkpoint under a concurrent reader does not fail"` — hold a read statement
open in another Db handle, expect PASSIVE checkpoint to return busy without
throwing.
GREEN: `Db.checkpoint(mode)` wrapping `PRAGMA wal_checkpoint(PASSIVE|TRUNCATE)`;
`IngestPipeline` runs PASSIVE after every publish and TRUNCATE when
`meta.sqlite-wal` exceeds 16 MiB (constant, documented). Wire on the ls-core
index executor (single-writer contract). Doctor SQLite section gains a
`wal size` line (RED in `RenderTest` first).

### A9. Fifth materialization: doc → editable occurrences  [minor, plan §8.6]

Decision: implement the API view rather than a fifth file — the doc-postings
records already carry the Editable flag.

RED: `ls-postings` `HandBuiltCorpusTest` case `"scanDocEditable yields exactly the editable subset"` —
compare against brute-force filter of `scanDocOccurrences`. `ls-index-model`
gains `IndexSnapshot.scanDocEditable(doc, sink)` (contract addition).
GREEN: implement in `PostingsSnapshot` as a flag-filtered doc scan; spec the
choice (view, not file) in docs/index-format.md §materializations.

### A10. Doctor completeness: generated-source status + per-target staleness  [minor, plan §19/§21]

RED: `ls-doctor` `RenderTest` — extend the golden key-line list with
`generated source status:` (count from `documents.generated`) under SemanticDB
and `stale targets:` (targets owning ≥1 stale doc) under BSP/SemanticDB;
`StoreSectionsTest` asserts the counts against a store containing one
generated doc and one stale-md5 doc.
GREEN: extend `SemanticdbSection`/`DocFreshnessStats` gathering; ls-core
supplies generated counts from `MetaStore` on the index executor.

---

## 3. Workstream B — §18.1 correctness-case completion (tests only unless a case fails)

Every case: add the fixture to `ls-rename` `FixtureWorkspace` (compiled by real
scalac with `-Xsemanticdb`) and/or `ls-semanticdb` `ScalacIntegrationSuite`,
then assert references AND rename behavior. If the RED test passes immediately,
the commit still lands the test (regression pin); if it fails, fix the engine
under the same task id.

| id | case (plan §18.1) | RED test — exact expectation |
|----|-------------------|------------------------------|
| B1 | export | covered by A4's tests (tracked here for §18.1 completeness) |
| B2 | inline | `inline def twice(x: Int)` in target A, call sites in A and B: one ref group; references find all call sites; rename edits all tokens (inline defs are ordinary symbols) |
| B3 | macro-generated API | case-class synthetic `apply`/`copy`: references on `.copy(` resolve to the copy symbol with call-site occurrences; rename on `copy` rejected (A5 synthetic-only); `derives CanEqual` given resolution occurrence appears in SemanticDB and references on the derived given find the derives clause |
| B4 | private member | `private def helper` + `private val state` used within the class: references stay in-file; rename succeeds with exact spans |
| B5 | local def | nested `def loop(n: Int)` inside a method: references stay document-local (SymbolKey.local), rename edits only that document |
| B6 | val member getter | `val label` on a class referenced cross-file: single-symbol group assumption pinned; rename edits definition + all uses |
| B7 | given/using | method `def render(using Core)`: the implicit-argument call site yields an occurrence of the given (`defaultCore`) and references on the given include it |
| B8 | top-level def/val | top-level `def topHelper` / `val topConst` in a `.scala` file (no object): cross-file references + rename |
| B9 | opaque type | `opaque type UserId = Long` with companion ops: type and companion merge per v1 policy; references find both type and term uses; rename edits both or is rejected — pin whichever the engine does and document it in architecture.md §7 |
| B10 | extension method rename | rename the fixture's existing extension method: succeeds, editing definition + call sites (`x.doubled` form) |
| B11 | external symbol rename | cursor on a `scala.collection.immutable.List` reference: `RenameRejected` containing "outside the workspace" (External bit) |
| B12 | fresh-snapshot StaleIndex branch | mutate the cursor document itself between compile stub and ingest so the post-ingest resolve is non-Snapshot: expect `LsError.StaleIndex` (audit: branch currently untested) |
| B13 | manifest → missing segment recovery | ls-core `WorkspaceState` test: delete the active segment dir behind a persisted store, boot; assert NotReady degrades gracefully with a doctor note and the next ingest heals (audit §18.2 gap) |

Acceptance for the workstream: `nix develop -c mill semanticdb.test + rename.test + core.test`
green; every §18.1 line in plan.md now names ≥1 test (record the mapping table
in docs/architecture.md appendix).

---

## 4. Workstream C — §18.3 benchmark completion (ls-bench)

Bench rules: every new measurement carries a ground-truth consistency check
(non-zero exit on mismatch) like the existing rows; `--smoke` stays < 60 s;
heavy tiers go behind `--full`. Each task's RED is a `BenchSuite` case invoking
the new mode on the tiny tier and asserting the report contains the new row
with sane values (>0, monotonic percentiles).

| id | benchmark (plan §18.3) | definition |
|----|------------------------|------------|
| C1 | SemanticDB ingest 1k/10k/100k | generate synthetic `.semanticdb` files (move the test protobuf encoder into a shared `ls-semanticdb` test-support artifact or a small bench-local encoder), then time `IngestPipeline.ingest` end-to-end (parse→SQLite→segment→publish). Tiers: smoke=1k, full=10k and 100k. Report docs/s and total ms |
| C2 | cold start / warm start | cold = first ingest of a fresh store (from C1 corpus); warm = process-restart simulation: reopen `MetaStore` + `SegmentReader.open(active)` + publish, time to first successful references query. Rows `cold-start` / `warm-start` |
| C3 | BSP import | time `ProjectModelLoader.load` against the fake BSP server with 5/50/200 synthetic targets (ls-bench gains a test-scope dep on the fake server or a bench-local canned server) |
| C4 | rename small / large | time `RenameEngine.rename` edit-plan generation for a rare symbol (≤5 occs) and the hottest group (≥1k occs) on the C1 corpus with a no-op CompileService; report p50/p95/p99 |
| C5 | PC completion percentiles + plugin overhead | drive `PcFacade.completion` on a fixture buffer ×200: report p50/p95/p99 with (a) no plugins, (b) a registered pass-through plugin — the delta is the plugin overhead row |
| C6 | workspace symbol fuzzy | REQUIRES the fuzzy feature: implement `MetaStore.workspaceSymbolSearchFuzzy` = candidate pull via FTS5 trigram table (`tokenize='trigram'`, new virtual table beside the prefix one, same external-content pattern) + in-memory subsequence/camel-hump ranking, cap 5000 candidates. TDD: `MetaStoreSuite` RED cases first (`"fuzzy finds camel-hump wSy → workspaceSymbol"`, `"fuzzy respects limit and ranking"`), then engine wiring (`workspace/symbol` uses prefix, falls back to fuzzy when prefix yields < limit), then the bench row (prefix vs fuzzy percentiles) |
| C7 | references medium tier + FFM microbench | add mid-rank group selector to `Corpus` + one bench row; add a tight prepared-statement loop row (`sqlite-ffm-call-overhead`, ns/call) |
| C8 | occurrence-set preservation gate | promote the audit's suggestion to a bench-side invariant: after every bench ingest/publish cycle, full-scan occurrence multiset equality vs the generator's ground truth (extends the existing consistency checker to the C1 ingest path) |

Acceptance: `nix develop -c mill bench.smoke` (< 60 s, includes C1 smoke tier,
C2, C4, C6 rows) and `nix develop -c mill bench.run --full` executed once with
output attached; `mill bench.test` green.

---

## 5. Workstream D — runtime/ops/toolchain mandates

### D1. AOT training mode  [MAJOR, plan §16.3/Phase 10]

RED:
- `scripts/aot-train.sh --workspace it/sample-workspace --out .scala3-bsp-semantic-ls/aot-cache.bin`
  must exist and, run inside `nix develop`, produce a non-empty cache file.
  RED harness: new gated test `AotTrainIntegrationTest` (env `LS_AOT_IT=1`,
  script-driven like the real-BSP suite) asserting the file is produced and
  that a follow-up `Main --version` run with `-XX:AOTCache` succeeds and the
  doctor reports `AOT cache: loaded`.
GREEN:
- `Main --aot-train <workspace>`: headless run covering the plan-§16.3 list —
  LSP initialize (in-process pipes), BSP initialize (real mill-bsp when the
  workspace has .bsp, else fake), SQLite open + hot statements, snapshot
  mmap load, SemanticDB parse batch, workspace/symbol, references, PC
  initialize + one completion — then clean exit.
- Script does the JDK 25 two-step: run 1 with `-XX:AOTMode=record
  -XX:AOTConfiguration=$tmp/aot.conf`, run 2 with `-XX:AOTMode=create
  -XX:AOTCache=<out>`; wrapper already consumes `LS_AOT_CACHE`.
- Fix the overclaim in docs/nix-build.md (claims training coverage exists).

### D2. JFR presets  [minor, Phase 10]

RED: `BenchSuite` case `"--jfr uses the named preset"` — run tiny bench with
`--jfr out.jfr --jfr-preset profile`, parse the recording (jdk.jfr consumer
API) and assert it contains JVM profiling events; default preset = `default`.
GREEN: `jdk.jfr.Configuration.getConfiguration(name)` wiring + optional
project preset `modules/ls-bench/resources/ls-bench.jfc` selectable by name.

### D3. snapshots/current.json  [minor, plan §5.3]

Decision: implement it (cheap, makes the on-disk layout self-describing).
RED: `SnapshotLifecycleTest` case `"publish writes snapshots/current.json"` —
after publish, `<root>/snapshots/current.json` exists, contains
`{segmentId, path, publishedAtMs, generation}` and matches the active
snapshot; atomic replace (write temp + move). `WorkspaceState` recovery
cross-checks it against the SQLite manifest and reports divergence in doctor.
GREEN: SnapshotManager writes it on publish; doctor Postings section renders
`snapshot file: consistent|divergent`.

### D4. Native-access tests guarded on Java 25  [minor, plan §15.4]

RED: add `RuntimeGuardSuite` to `ls-sqlite-ffm` and `ls-postings` test roots
asserting `Runtime.version().feature() == 25` (mirrors ls-index-model), so a
wrong-JDK environment fails loudly in the modules that use native access.

### D5. CI cannot resolve unlocked dependencies  [minor, plan §15.3 rule 4]

RED: `scripts/check-offline-compile.sh` — seeds a temp `COURSIER_CACHE` from
the flake's `ivy-gather` output (`nix build .#default.passthru.ivyCache`),
then runs `mill --no-daemon __.compile` with `COURSIER_MODE=offline` in a
clean checkout copy; the script must FAIL (proving the guard) when a
not-in-lock dependency is temporarily appended to build.mill (self-test mode
`--self-test` does exactly that in a scratch copy and expects failure).
GREEN: add the script + a CI step replacing the plain online compile
(`nix develop -c ./scripts/check-offline-compile.sh`), keeping `__.test`
online (test-runner fetches nothing new once compile is offline-proven).

### D6. lsp4j/gson declared-vs-resolved honesty  [minor, audit protocols]

RED: a build.mill-level check is impractical; instead extend F3's docs checker
to assert docs/architecture.md documents the lsp4j 1.0.0 eviction (already
does) AND bump `Deps.lsp4j`/`lsp4jJsonrpc` to `1.0.0` so declared == resolved.
Full suite + lock regen (`scripts/regen-ivy-lock.sh`) must stay green; if any
module fails to compile against declared 1.0.0, revert and document instead.

---

## 6. Workstream E — mill-based BSP end-to-end suite (the LS-wide acceptance suite)

Foundation (already green, committed): `it/sample-workspace` +
`RealBspIntegrationTest` (6 tests: doctor/targets, BSP-driven compile+reindex,
workspace/symbol, cross-module references, cross-module rename WorkspaceEdit,
dirty-buffer completion) + `scripts/it-real-bsp.sh`.

Expansion — every test drives the REAL `mill --bsp` server end-to-end through
the LSP protocol. Extend the sample workspace as needed (keep it < 10 files).
Each task is RED-first: write the e2e assertion, watch it fail (or fail to
compile against a missing feature), then fix in the owning module.

| id | e2e test | definition & exact assertions |
|----|----------|-------------------------------|
| E1 | IndexUnavailable target | add module `c` to the sample WITHOUT `-Xsemanticdb`: doctor lists exactly {mill-build, c} as IndexUnavailable; references/rename on a `c` file answer with `LsError.IndexUnavailable`-derived responses; PC completion in `c` still works (plan §4.2) |
| E2 | diagnostics forwarding (needs A1) | test edits `Consumer.scala` in the temp workspace to introduce a type error, saves via didSave; assert the client receives `textDocument/publishDiagnostics` for that file with severity Error from the real mill compile; fixing the file and saving clears the diagnostics |
| E3 | didSave → compile → reingest cycle | after E2's fix-and-save, assert (poll ≤ 60 s) references reflect the edited file's new token positions without any explicit reindex command — proving the debounced pipeline end-to-end |
| E4 | rename rejection paths over real BSP | (a) stale source: edit a file on disk without saving through LSP, rename → `RenameRejected/StaleIndex`; (b) invalid identifier `class` → rejection message; (c) rename in `c` (no semanticdb) → IndexUnavailable |
| E5 | hover / signatureHelp / definition / documentHighlight | on `Consumer.scala` against real classpath: hover on `Greeting` shows a type; signatureHelp inside `greeting.message(` call (add an arg-taking method to the fixture); definition jumps to `Greeting.scala` with the exact span; documentHighlight distinguishes read/write on a var (add one to the fixture) |
| E6 | shared source across targets | add a shared source dir compiled into both `a` and a new target `shared-b` (mill `sources` override): references from the shared file unify; rename passes the shared-source consistency check (plan §13.1) |
| E7 | forked PC over real BSP (needs A7) | run the suite once with forked-PC default: completion + plugin status work; kill the PC worker pid mid-session; next completion succeeds (respawn), LS alive |
| E8 | segment hygiene + startup recovery (needs A2/D3) | after ≥3 didSave cycles: exactly one segment dir on disk and snapshots/current.json consistent; then shut the LS down, boot a NEW ScalaLs on the same temp workspace WITHOUT recompiling: warm recovery serves references before any BSP compile completes |
| E9 | AOT-trained boot (needs D1, gated LS_AOT_IT=1) | train on the sample workspace, boot with the cache, doctor reports `AOT cache: loaded`, all E-suite basics pass |
| E10 | CI wiring | `.github/workflows/ci.yml` gains a `real-bsp-e2e` job running `nix develop -c ./scripts/it-real-bsp.sh` (and the gated E7 variant); the job must be required for merge. Local acceptance: two consecutive green runs to establish flake-freedom; any flaky test gets a deflake fix before merge, never a retry loop |

Suite organization: split `RealBspIntegrationTest` into
`RealBspCoreTest` (E-foundation, E1, E4, E5), `RealBspLifecycleTest`
(E2, E3, E6, E8), `RealBspIsolationTest` (E7, E9) — all sharing one
lazily-initialized workspace fixture per suite to keep wall-clock ≈ setup-once.
Budget: full suite ≤ 5 min warm.

---

## 7. Per-module "enough tests" bar (binding coverage matrix)

Definition of "enough" (mechanically checkable at review):

```text
M1. Every public API entry point (object/class method exported for another
    module) is called by at least one test in its own module.
M2. Every LsError variant is produced by at least one test through a real
    code path (no direct construction-only tests).
M3. Every UnsafeReason bit has a producing ingest/engine test (after A4/A5:
    all 9 bits; SharedSourceDisagreement + PcOnly already covered).
M4. Every on-disk format/schema element (table, file, header field) has a
    write→read round-trip test and at least one corruption/rejection test.
M5. Every LSP capability advertised in initialize has an e2e test (fake BSP)
    AND a real-BSP e2e test (workstream E).
```

| module | existing suites (keep green) | required additions |
|--------|------------------------------|--------------------|
| indexModel | RuntimeContract, TargetGraph | `scanDocEditable` contract test (A9) |
| semanticdb | WireDecoder, SymbolStrings, Locator, Md5, Groups, ScalacIntegration | A4 export, A5 synthetic flag, B2/B3/B7/B9 fixture cases |
| sqliteFfm | Sqlite3, Db, Schema, MetaStore | A8 checkpoint, C6 fuzzy search, D4 guard |
| postings | HandBuilt, Random, Corruption, IntervalIndex, SnapshotLifecycle, EmptySegment | A9 editable doc scan, D3 current.json, D4 guard |
| bsp | Discovery, Session, Launch, SemanticdbFlags, ProjectModel | A3 didChange dispatch unit test (handler plumbed) |
| pc | Utf16, CompilerPluginConfig, PluginManager, PcQuery, WorkerProtocol, ForkedWorker, PcWorkerManager, ServiceLoader | A7 origins-over-the-wire protocol test |
| rename | SymbolEncoding, Identifier, WorkspaceTargets, ReferencesAndQuery, Rename, RenameMutation, RawPath | A2 janitor, A6 group overlay, B4–B13 |
| doctor | RuntimeNix, Render, StoreSections, BspLauncherCompat | A10 lines, D3 divergence line, A2 pending==0 |
| core | Uris, LspConvert, Capabilities, ExecuteCommand, PcOverlay, LsEndToEnd, RealBspIntegration | A1 diagnostics (fake+real), A3, A7 backend, B13 recovery, E-suite |
| bench | BenchSuite | C1–C8 cases, D2 preset |

---

## 8. Workstream F — documentation reconciliation (with a mechanical check)

### F1. Purge stale/false claims  [minor, multiple audit findings]

Fix in one commit: docs/nix-build.md "Current gaps" (lock + schema exist now),
`.#mill` export contradiction, three-vs-four checks, `mill -i` vs
`--no-daemon`, AOT training overclaim (until D1 lands, then reinstate);
docs/architecture.md: storage layout (`snapshots/current.json` per D3,
`pc-plugins.json` instead of `pc/plugins/` staging), "SemanticDB watcher" →
"full rescan (v1)", RawSemanticDBPath write-through wording, request-time-only
UnsafeReason bits table, `contentless_delete=1` note, PC in-process/forked
default (per A7), single-connection SQLite design (no pool) rationale.

### F2. plan-to-test traceability appendix

Add `docs/traceability.md`: three tables mapping every §13.1 safety rule,
§18.1 correctness case, and §18.3 benchmark to its test file + case name.
Updated by every B/C task; reviewed at final acceptance.

### F3. scripts/check-docs.sh  [the docs' RED test]

Grep-based consistency checker run in CI: asserts (a) no "Current gaps" section
older than the tree (checks the two known file paths exist), (b) every test
file named in docs/traceability.md exists and contains the named case string,
(c) forbidden stale phrases (`mill -i core.assembly`, `does not export .#mill`)
absent. RED: write the checker, watch it fail on today's docs; GREEN: F1.

---

## 9. Sequencing, parallelism, and gates

```text
Wave 1 (independent, start immediately):
  A1, A2, A3, A8, A9, A10, D2, D3, D4, F1+F3
Wave 2 (semantic layer):
  A4, A5, A6, then B1–B13 (fixture batches: B2–B6 / B7–B10 / B11–B13)
Wave 3 (backends + bench):
  A7 (after A6), C1–C8 (C6 first — it adds a feature), D1, D5, D6
Wave 4 (end-to-end):
  E1–E9 in two batches (E1/E4/E5, then E2/E3/E6/E8 after A1/A2/D3,
  then E7/E9 after A7/D1), E10 last
Final gate (all must pass, outputs recorded):
  nix develop -c mill __.compile + __.test + bench.smoke
  nix flake check
  nix develop -c ./scripts/check-ivy-lock.sh
  nix develop -c ./scripts/check-docs.sh
  nix develop -c ./scripts/it-real-bsp.sh          (full E-suite)
  nix build .#default && ./result/bin/scala3-bsp-semantic-ls --version
  Zero remaining MAJOR audit findings; every §18 line mapped in
  docs/traceability.md.
```

Definition of done for the project: the final gate passes on a clean clone in
`nix develop` with a cold coursier cache, and a re-run of the section-by-section
audit (same six-auditor method) reports no `missing` and no undocumented
deviations.

--- Original Design Draft End ---
