# Traceability

Every accepted plan mandate and acceptance row maps to a concrete test class (and,
where useful, a case-name substring). `scripts/check-docs.sh` mechanically verifies
that every test class and file named here exists. Test classes live under
`modules/<module>/test/src/`.

This file also records the **accepted plan evolutions** (deviations agreed during
implementation) so the docs and the plan stay reconciled without editing `rlcr.md`.

## Acceptance criteria (AC-1 … AC-20)

| AC | What it verifies | Primary test class(es) |
|----|------------------|------------------------|
| AC-1 | BSP build diagnostics forwarded to the LSP client, target-scoped | `DiagnosticRouterSuite`, `LspConvertDiagnosticSuite`, `LsEndToEndTest` (fake BSP), `RealBspLifecycleTest` (E2, real BSP) |
| AC-2 | Superseded postings segments reclaimed (publish-time + startup janitor) | `SnapshotJanitorTest`, `IngestJanitorSuite`, `BootstrapJanitorSuite`, `LsEndToEndTest` |
| AC-3 | `buildTarget/didChange` reloads the model and re-ingests | `BuildTargetsChangeBufferingSuite`, `LsEndToEndTest` |
| AC-4 | Export forwarders grouped and rename-rejected | `ScalacIntegrationSuite`, `RenameSuite`, `ReferencesAndQuerySuite` |
| AC-5 | Synthetic-only symbol rejected with the synthetic-only reason | `ScalacIntegrationSuite`, `RenameSuite` |
| AC-6 | References dirty-buffer overlay keyed by the whole alias group | `RenameSuite`, `PcOverlaySuite` |
| AC-7 | PC process isolation is the production default | `PcBackendSuite`, `ForkedWorkerSuite`, `PcWorkerManagerSuite`, `RealBspIsolationTest` (E7) |
| AC-8 | SQLite WAL checkpoint scheduling that never blocks the writer | `IngestCheckpointSuite`, `StoreSectionsTest`, `DbSuite` |
| AC-9 | A doc→editable-occurrence scan view (`scanDocEditable`) | `HandBuiltCorpusTest` |
| AC-10 | Doctor generated-source status and per-target staleness | `StoreSectionsTest`, `RenderTest`, `DoctorCommandSuite` |
| AC-11 | Fuzzy workspace-symbol matching + schema v1→v2 migration | `FuzzyRankSuite`, `MetaStoreSuite`, `SchemaSuite` |
| AC-12 | Every plan §18.1 correctness case named by a test with exact output | `RenameSuite`, `ReferencesAndQuerySuite`, `ScalacIntegrationSuite`, `RenameMutationSuite` |
| AC-13 | Every plan §18.3 benchmark exists with a ground-truth consistency check | `BenchSuite` |
| AC-14 | AOT cache training mode covering the plan §16.3 workload | `AotTrainIntegrationTest`, `RealBspIsolationTest` (E9) |
| AC-15 | Runtime/ops mandates (JFR preset, snapshots/current.json, Java-25 guards, offline-CI compile guard) | `Jdk25GuardSuite`, `CurrentSnapshotFileSuite`, `BenchSuite`, `OfflineCompileGuardSuite`, `StoreSectionsTest`, `RenderTest` |
| AC-16 | The mill-based real-BSP end-to-end suite | `RealBspCoreTest`, `RealBspLifecycleTest`, `RealBspIsolationTest`, `RealBspIntegrationTest` (see E-rows) |
| AC-17 | Documentation reconciled with a mechanical checker | `docs/traceability.md` (this file) + `scripts/check-docs.sh` |
| AC-18 | Synchronous per-doc write-through on the RawSemanticDBPath | `RawPathSuite` |
| AC-19 | PC-only workspace-symbol dirty-buffer overlay | `PcOverlaySuite`, `LsEndToEndTest`, `MetaStoreSuite` |
| AC-20 | SQLite reader-connection pool | `ReaderPoolSuite`, `MetaStoreSuite` |

## AC-16 real-BSP end-to-end rows (E1 … E10)

All E-rows drive the real `mill --bsp` server over LSP; gated by `LS_REAL_BSP_IT=1`
(E9 by `LS_AOT_IT=1`). Suites share the `RealBspServer` fixture.

| E | Definition | Test class / location |
|---|------------|-----------------------|
| E1 | A target without `-Xsemanticdb` (module `c`) is a hard SemanticDB error (doctor `SemanticDB coverage: ERROR`; every request on a `c` source errors) | `RealBspCoreTest` |
| E2 | Diagnostics forwarding on a real compile error; the fix clears them | `RealBspLifecycleTest` |
| E3 | `didSave`→compile→reingest reflects new token positions with no explicit reindex | `RealBspLifecycleTest` |
| E4 | Rename rejection paths (no-SemanticDB source, external symbol, no occurrence) | `RealBspCoreTest` |
| E5 | hover / signatureHelp / definition / documentHighlight | `RealBspCoreTest` |
| E6 | A source shared across two targets unifies references + passes rename consistency | `RealBspLifecycleTest` |
| E7 | Forked PC over real BSP survives a worker kill (respawn + buffer replay) | `RealBspIsolationTest` |
| E8 | Segment hygiene (one segment dir) + warm restart serves references from recovery | `RealBspLifecycleTest` |
| E9 | AOT-trained boot loads the cache and stays queryable | `RealBspIsolationTest` |
| E10 | CI wiring: the `real-bsp-e2e` job runs the gated suite | `.github/workflows/ci.yml` + `scripts/it-real-bsp.sh` |

## Plan §13.1 rename-safety rules → `UnsafeReason` + test

The rename group's `unsafeReasonMask` (see `ls.index.UnsafeReason`) rejects a rename
when any bit is set. Each rule and its test:

| Rule (`UnsafeReason`) | Meaning | Test class |
|-----------------------|---------|------------|
| `External` | symbol defined outside the workspace | `RenameSuite`, `RealBspCoreTest` (E4b) |
| `GeneratedOccurrence` | occurrence in generated source | `RenameSuite` |
| `ReadonlyOccurrence` | occurrence in a read-only source | `RenameSuite` |
| `OverrideFamily` | member of an override family | `RenameSuite`, `ScalacIntegrationSuite` |
| `SyntheticOnly` | only synthetic occurrences (e.g. case-class `copy`) | `RenameSuite`, `ScalacIntegrationSuite` |
| `PcOnly` | PC-only symbol not in fresh SemanticDB | `PcOverlaySuite`, `LsEndToEndTest` |
| `SharedSourceDisagreement` | targets disagree on a shared source | `RenameSuite` (and `RealBspLifecycleTest` E6, the passing case) |
| `UnsupportedSymbolFamily` | exported-symbol / unsupported family | `RenameSuite`, `ScalacIntegrationSuite` (AC-4) |
| `DependencySource` | occurrence in a dependency source | `RenameSuite` |
| `OpaqueType` | opaque type (conservative reject, DEC-3) | `RenameSuite`, `ScalacIntegrationSuite` |

## Plan §18.1 cases and §18.3 benchmarks → tests

Both are enumerated **row-by-row** in the machine-checkable Case map below: every
plan §18.1 correctness case and every §18.3 benchmark row maps to a test class + an
exact case/report-row substring that `scripts/check-docs.sh` verifies is present.
The §18.1 cases live in `ReferencesAndQuerySuite` / `RenameSuite` /
`ScalacIntegrationSuite` / `RenameMutationSuite`; the §18.3 rows in `BenchSuite`.

## Case map (machine-checkable)

`scripts/check-docs.sh` parses every `` `<TestClass>` :: "<case substring>" `` line
below and FAILS if the substring is not present in that class's `.scala` test file
(so a typo, or a renamed/removed test, breaks the gate). Each substring is an exact
fragment of a real `test("…")` name.

### Plan §13.1 rename-safety rules
- `RenameSuite` :: "external library symbol is rejected as outside the workspace"
- `RenameSuite` :: "occurrences in generated sources are rejected"
- `RenameSuite` :: "occurrences in readonly sources are rejected"
- `RenameSuite` :: "override family is rejected"
- `RenameSuite` :: "synthetic-only symbol is rejected with the synthetic-only reason"
- `RenameSuite` :: "PC-only symbols are rejected"
- `RenameMutationSuite` :: "shared-source disagreement between targets is rejected"
- `RenameSuite` :: "exported symbol is rejected with the exported-symbol reason"
- `RenameSuite` :: "dependency sources are rejected"
- `RenameSuite` :: "rename of an opaque type is rejected (conservative policy)"
- `RenameSuite` :: "compile failure rejects the rename"
- `RenameMutationSuite` :: "stale md5: source edited after compile is rejected before emitting edits"

### Plan §18.1 correctness cases (one entry per plan.md §18.1 row)
- class references — `ReferencesAndQuerySuite` :: "class references unify companion object and constructor across files and targets"
- object references — `ReferencesAndQuerySuite` :: "object references (SharedThing) from the shared source"
- trait references — `ReferencesAndQuerySuite` :: "trait references (Greeter) reach the extends clause and cross-target signatures"
- enum references — `ReferencesAndQuerySuite` :: "enum references (Color) include the cross-target type and case use"
- constructor references — `ReferencesAndQuerySuite` :: "apply-sugar unification: case class references include Item(1), Item.apply(2), new Item(3)"
- companion class/object — `ReferencesAndQuerySuite` :: "class references unify companion object and constructor across files and targets"
- method overload — `ReferencesAndQuerySuite` :: "method overloads stay separate"
- val getter — `ReferencesAndQuerySuite` :: "cross-file val member references are exactly the definition and cross-file use"
- var getter/setter — `ReferencesAndQuerySuite` :: "var getter, setter, and definition references are exactly all value tokens"
- local val — `ReferencesAndQuerySuite` :: "local val references stay inside the document"
- local def — `ReferencesAndQuerySuite` :: "nested local def references stay inside the document"
- private member — `ReferencesAndQuerySuite` :: "private member references are exactly the in-file definition and uses"
- top-level definitions — `ReferencesAndQuerySuite` :: "top-level def and val references are exactly their definitions and cross-file uses"
- extension methods — `ReferencesAndQuerySuite` :: "extension method references are exactly the definition and both call sites"
- given / using — `ReferencesAndQuerySuite` :: "given references are exactly the by-name uses including the using-clause site"
- export — `ReferencesAndQuerySuite` :: "export forwarder references are exactly the definition and the forwarder call"
- inline — `ReferencesAndQuerySuite` :: "inline def references are exactly the definition and both call sites"
- macro-generated API — `ReferencesAndQuerySuite` :: "case-class copy references resolve to the copy symbol call site only"
- shared sources across targets — `RealBspLifecycleTest` :: "E6 a source shared across two targets unifies references and passes rename consistency"
- generated sources — `RenameSuite` :: "occurrences in generated sources are rejected"
- readonly source rejection — `RenameSuite` :: "occurrences in readonly sources are rejected"
- dependency source rejection — `RenameSuite` :: "dependency sources are rejected"
- stale md5 rejection — `RenameMutationSuite` :: "stale md5: source edited after compile is rejected before emitting edits"
- compile-failure rename rejection — `RenameSuite` :: "compile failure rejects the rename"
- PC-only symbol rename rejection — `RenameSuite` :: "PC-only symbols are rejected"
- (also) opaque type references — `ReferencesAndQuerySuite` :: "opaque type references are exactly the type, companion, and all in-file uses"
- (also) derives synthetic-only — `ScalacIntegrationSuite` :: "derives clause: the case class is defined and the derived given is synthetic-only"
- (also) fresh-snapshot stale index — `RenameMutationSuite` :: "fresh-snapshot stale index: the cursor document itself is edited after compile"

### Plan §18.3 benchmark rows (one entry per plan.md §18.3 row; the substring is the report-row string `BenchSuite` emits and asserts)
- cold start — `BenchSuite` :: "cold-start"
- warm start — `BenchSuite` :: "warm-start"
- BSP import — `BenchSuite` :: "bsp-import-"
- SemanticDB ingest 1k — `BenchSuite` :: "semanticdb-ingest-1k"
- SemanticDB ingest 10k — `BenchSuite` :: "semanticdb-ingest-10k"
- SemanticDB ingest 100k — `BenchSuite` :: "semanticdb-ingest-100k"
- workspace symbol prefix — `BenchSuite` :: "workspace/symbol fts (prefix)"
- workspace symbol fuzzy — `BenchSuite` :: "workspace/symbol fuzzy"
- references rare — `BenchSuite` :: "references rare (all targets)"
- references medium — `BenchSuite` :: "references medium (all targets)"
- references hot — `BenchSuite` :: "references hot (all targets)"
- rename small — `BenchSuite` :: "rename small (rare)"
- rename large — `BenchSuite` :: "rename large (hot)"
- PC completion P50/P95/P99 — `BenchSuite` :: "pc completion"
- PC plugin overhead — `BenchSuite` :: "pc plugin overhead"
- SQLite FFM overhead — `BenchSuite` :: "sqlite-ffm-call-overhead"
- mmap scan records/sec — `BenchSuite` :: "doc scan (full)"
- (consistency gate) — `BenchSuite` :: "tiny run renders the report and passes all consistency checks"
- (ingest tiers by docs) — `BenchSuite` :: "ingest tiers are sized by document count (1000 smoke, 10000/100000 full)"
- (occurrence-set gate) — `BenchSuite` :: "a real occurrence-set mismatch trips the ingest gate (not a bare check)"
- (JFR preset) — `BenchSuite` :: "--jfr uses a named preset and records JVM events; default preset is 'default'"

### AC-16 real-BSP E-rows
- `RealBspCoreTest` :: "E1 doctor: module c (no -Xsemanticdb) is flagged as a SemanticDB error"
- `RealBspCoreTest` :: "E1 SemanticDB is mandatory: completion on module c is a hard error (no PC fallback)"
- `RealBspCoreTest` :: "E1 SemanticDB is mandatory: documentHighlight on module c is a hard error (not empty)"
- `RealBspCoreTest` :: "E4a rename on a source without SemanticDB (module c) is a hard error"
- `RealBspCoreTest` :: "E4b rename of an external/library symbol is rejected (outside the workspace)"
- `RealBspCoreTest` :: "E4c rename at a position with no symbol occurrence is rejected"
- `RealBspCoreTest` :: "E5 hover (PC) answers on an indexed module"
- `RealBspCoreTest` :: "E5 signatureHelp (PC) answers at a constructor call site"
- `RealBspCoreTest` :: "E5 definition (PC) resolves a same-file reference to its declaration"
- `RealBspCoreTest` :: "E5 documentHighlight (index) returns the in-file occurrences of a symbol"
- `RealBspLifecycleTest` :: "E2 a real compile error is forwarded as an Error diagnostic; the fix clears it"
- `RealBspLifecycleTest` :: "E3 didSave -> compile -> reingest reflects new token positions with no explicit reindex"
- `RealBspLifecycleTest` :: "E6 a source shared across two targets unifies references and passes rename consistency"
- `RealBspLifecycleTest` :: "E8 repeated saves keep one segment dir; a warm restart serves references from recovery"
- `RealBspIsolationTest` :: "E7 forked PC over real BSP survives a worker kill"
- `RealBspIsolationTest` :: "E9 an AOT-trained boot loads the cache and stays queryable"

## Accepted plan evolutions (recorded here; `rlcr.md` unmodified)

- **SemanticDB is mandatory (supersedes the graceful `IndexUnavailable` state).**
  A build target that emits no SemanticDB — including Mill's own `mill-build` — is
  a hard error: every request on such a source fails with `LsError.NoSemanticdb`
  ("<uri> has no SemanticDB output; every source must be compiled with
  -Xsemanticdb"), and the doctor renders `SemanticDB coverage: ERROR`. Tests:
  `LsEndToEndTest` (fake BSP), `RealBspCoreTest` (E1), `RealBspIntegrationTest`,
  `RenderTest`. `requireSemanticdb` also honors the recovered/persisted index so a
  no-BSP warm restart (`RealBspLifecycleTest` E8) still serves indexed sources.
- **Full original zaozi validated via its own Nix toolchain.** The real-repo
  validation builds the unmodified `github:xinpian-tech/zaozi` (a pinned flake
  input, SemanticDB enabled by `nix/patches/zaozi-semanticdb.patch`) with its own
  flake and indexes it. Driver: `scripts/it-zaozi.sh` (manual, heavy).
- **`derives` clause is synthetic-only.** scalac emits the derived given only in
  the SemanticDB synthetics payload, which the parser skips (plan 4.3), so the
  `derives` case is characterized as synthetic-only, not an indexed occurrence.
  Test: `ScalacIntegrationSuite`.
- **Forked PC is the production default.** `Main.pcBackendMode` defaults to forked
  process isolation; `--in-process-pc` opts back into the same JVM (used by AOT
  training). Tests: `PcBackendSuite`, `RealBspIsolationTest` (E7).
