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

## Plan §18.1 correctness cases → tests

The full §18.1 case→test mapping is maintained in `docs/architecture.md` (the
"§18.1 → test" section); the cases are implemented in `RenameSuite`,
`ReferencesAndQuerySuite`, `ScalacIntegrationSuite`, and `RenameMutationSuite`.

## Plan §18.3 benchmark rows → `BenchSuite`

Every §18.3 benchmark row (SemanticDB ingest 1k/10k/100k docs, cold/warm start,
BSP import 5/50/200, rename small/large, references, workspace-symbol prefix +
fuzzy, PC completion + plugin overhead, FFM call overhead, occurrence-set
preservation) is asserted by `BenchSuite` with a generator-vs-index ground-truth
consistency check (mismatch → non-zero exit).

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
