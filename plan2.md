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
