# Rust port coverage audit

This audit maps every retained Scala test suite (`modules/*/test/`), every §18.1
references/rename correctness case, and every E0–E8 real-BSP row onto the Rust
test(s) that prove it, and classifies the gaps. It is the fixture-coverage
acceptance gate for the big-bang rewrite (the plan's CORE_RISKS: "big-bang makes
fixture coverage existential").

**Method.** The mapping was produced by a Codex analysis of both trees and
independently cross-checked by two focused scans (a §18.1 matrix parity scan and
a per-module suite scan); every disputed claim was resolved by reading the actual
assertions on both sides. The Scala tree is retained read-only until cutover
(task22), so each Rust test is diffed against its source suite.

**Classification.** Each Scala suite is exactly one of: **PORTED** (a Rust test
proves the same behavior), **INTENTIONAL NON-PORT** (out of scope by a ratified
decision — SQLite removal DEC-7, AOT/`AotTrain`, `ls-tasty`/dependency-index
v1.1, or a DEC-1/DEC-2 trim), **REAL GAP** (a mandatory behavior with no proving
Rust test), or **TASK22 OBLIGATION** (strictly docs/deletion/bench work).

## Verdict

- **§18.1 correctness matrix: fully ported (22/22 families).** Every safe family
  that the Scala `ReferencesAndQuerySuite`/`RenameSuite`/`RenameMutationSuite`
  pins with an exact span set / exact rejection reason is pinned identically in
  `crates/ls-engine/tests/{references_matrix,rename_matrix}.rs`; the SemanticDB
  characterizations (`ScalacIntegrationSuite`) are in
  `crates/ls-semanticdb/tests/scalac_integration.rs`.
- **E0–E8 real-BSP rows: every non-trimmed row ported** over live mill in
  `crates/ls-server/tests/real_bsp_{e2e,pc,pc_recovery}.rs`.
- **One real gap found and closed this round:** the seeded-random differential
  corpus (`RandomCorpusTest`) had no Rust equivalent → ported as
  `crates/ls-store/tests/random_corpus.rs` (11 differential tests, green).
- **SQLite / AOT / `ls-tasty`** suites are intentional non-ports by ratified
  decision; the **DEC-1/DEC-2 trims** (`pcPluginStatus`, no-BSP warm restart) are
  deferred and owe a task22 deferral note.

## Per-suite coverage

| Scala suite | module | class | proving Rust test / rationale |
|---|---|---|---|
| BspDiscoveryTest | ls-bsp | PORTED | `ls-bsp/tests/discovery.rs` |
| BspLaunchTest | ls-bsp | PORTED | `ls-bsp/tests/launch.rs` |
| BspProjectModelTest | ls-bsp | PORTED | `ls-bsp/tests/project_model.rs` |
| BspSessionTest | ls-bsp | PORTED | `ls-bsp/tests/session.rs`, `mill_smoke.rs` |
| SemanticdbFlagsTest | ls-bsp | PORTED | `ls-bsp/tests/semanticdb_flags.rs` |
| WireDecoderSuite | ls-semanticdb | PORTED | `ls-semanticdb/tests/wire.rs` |
| SymbolStringsSuite | ls-semanticdb | PORTED | `ls-semanticdb/tests/symbols.rs` |
| ScalacIntegrationSuite | ls-semanticdb | PORTED | `ls-semanticdb/tests/scalac_integration.rs` (incl. `derives_clause_case_class_defined_and_derived_given_synthetic_only`) |
| Md5Suite | ls-semanticdb | PORTED | `ls-semanticdb/tests/md5.rs`, `pipeline.rs` |
| LocatorSuite | ls-semanticdb | PORTED | `ls-semanticdb/tests/locator.rs` |
| GroupsSuite | ls-semanticdb | PORTED | `ls-semanticdb/tests/groups.rs`, `pipeline.rs` |
| TargetGraphSuite | ls-index-model | PORTED | `ls-index-model/tests/properties.rs`, `src/bitset.rs` tests |
| RuntimeContractSuite | ls-index-model | PORTED | `ls-index-model/tests/properties.rs`, `src/text.rs` tests |
| RenameSuite | ls-rename | PORTED | `ls-engine/tests/rename_matrix.rs` (exact edit spans + rejection reasons) |
| RenameMutationSuite | ls-rename | PORTED | `ls-engine/tests/rename_matrix.rs` (`stale_md5_*`, `fresh_snapshot_*`, `shared_source_disagreement_rejected`) |
| ReferencesAndQuerySuite | ls-rename | PORTED | `ls-engine/tests/references_matrix.rs` — assertion-for-assertion parity (see §18.1 note) |
| RawPathSuite | ls-rename | PORTED | `ls-engine/tests/engine.rs::raw_path_write_through_runs_inline_and_heals`, `references_raw_fallback_serves_same_doc` (production write-through) |
| UnitSuites (Identifier / SymbolEncoding / WorkspaceTargets) | ls-rename | PORTED | identifier backtick-quoting `ls-engine/tests/engine.rs` (`identifiers::encode`); symbol string round-trip `engine.rs` + `ls-semanticdb/tests/symbols.rs`; reverse-dependency closure + duplicate-bspId refusal `ls-bsp/tests/project_model.rs`, `ls-engine/src/targets.rs` |
| IngestJanitorSuite | ls-rename | PORTED | publish-time auto-reclaim of drained superseded generations `ls-store/tests/store.rs::publishes_auto_reclaim_drained_superseded_generations` (added this round); held-snapshot-defers-deletion `store.rs::janitor_defers_deletion_until_snapshot_drops`; live end-to-end `real_bsp_e2e.rs::real_bsp_repeated_saves_keep_a_single_committed_segment_dir` |
| HandBuiltCorpusTest | ls-postings | PORTED | `ls-store/tests/segment.rs`, `symbol_at.rs` |
| IntervalIndexTest | ls-postings | PORTED | `ls-store/tests/symbol_at.rs::interval_block_pruning`, `segment.rs::multi_block_group_chunks_and_skips` |
| CorruptionTest | ls-postings | PORTED | `ls-store/tests/segment.rs`, `validation.rs`, `store.rs` corruption cases |
| CurrentSnapshotFileSuite | ls-postings | PORTED | `ls-store/tests/store.rs::publish_then_reopen_preserves_pair`, `second_publish_increments_generation` (current.json → manifest+generation) |
| SnapshotJanitorTest | ls-postings | PORTED | `ls-store/tests/store.rs::janitor_defers_deletion_until_snapshot_drops`; real-BSP segment hygiene |
| SnapshotLifecycleTest | ls-postings | PORTED | `ls-store/tests/store.rs::retained_snapshot_survives_publish`, `second_publish_increments_generation` |
| **RandomCorpusTest** | ls-postings | **REAL GAP → CLOSED** | `ls-store/tests/random_corpus.rs` (added this round) |
| Jdk25GuardSuite | ls-postings | INTENTIONAL NON-PORT | Rust store is not Java-25-bound; guard obsolete |
| RenderTest | ls-doctor | PORTED | `ls-server/src/doctor.rs::text_renders_the_seven_sections_in_fixed_order`, `unavailable_sections_render_the_reason_not_omitted` |
| RuntimeNixSectionsTest | ls-doctor | PORTED | `ls-server/src/doctor.rs::text_renders_runtime_nix_and_pc_facts` |
| StoreSectionsTest | ls-doctor | PORTED | `ls-server/src/doctor.rs::json_has_the_store_key_and_no_sqlite_or_postings`, real-BSP doctor checks |
| BspLauncherCompatTest | ls-doctor | PORTED | `ls-bsp/tests/session.rs`, `mill_smoke.rs` (BSP launcher/session compat) |
| CapabilitiesSuite | ls-core | PORTED | `ls-server/src/capabilities.rs` tests; `server_surface.rs` (incl. absence of `pcPluginStatus`) |
| BuildTargetsChangeBufferingSuite | ls-core | PORTED | `ls-server/tests/bootstrap.rs::a_build_target_change_*` |
| BootstrapRecoverySuite | ls-core | PORTED | `ls-server/tests/bootstrap.rs::no_bsp_recovered_index_*`; `ls-store/tests/store.rs::{torn_state_tmp_recovers_old,crash_after_state_before_manifest_recovers_old,state_generation_mismatch_rejected}` |
| BootstrapJanitorSuite | ls-core | PORTED | `ls-store/tests/store.rs::open_readonly_recovers_without_creating_or_cleaning` (+ `assert_no_tmp_debris`) |
| DiagnosticRouterSuite | ls-core | PORTED | `ls-server/src/diagnostics.rs` tests; `fake_bsp_e2e.rs::compile_error_diagnostics_are_published_to_the_client` |
| DoctorCommandSuite | ls-core | PORTED | `ls-server/tests/server_surface.rs::execute_command_doctor_renders_text_and_json` |
| ExecuteCommandSuite | ls-core | PORTED | `ls-server/tests/server_surface.rs::execute_command_reindex_compile_and_unknown` (`pcPluginStatus` trimmed — DEC-1) |
| IndexPcDefinitionResolverSuite | ls-core | PORTED | `ls-engine/tests/engine.rs::symbol_definition_*`; `ls-jvm/tests/live_definition.rs` |
| LspConvertSuite | ls-core | PORTED | `ls-server/src/convert.rs` tests |
| PcOverlaySuite | ls-core | PORTED | `ls-server/src/pc_overlay.rs` tests; `references_matrix.rs` overlay tests; `fake_bsp_e2e.rs` PC-only-over-the-wire |
| PcBackendSuite | ls-core | PORTED | `ls-jvm/tests/{live_boundary,live_recovery}.rs`; `real_bsp_pc_recovery.rs` (embedded island/watchdog model) |
| UrisSuite | ls-core | PORTED | `ls-index-model/src/uri.rs` tests |
| LsEndToEndTest | ls-core | PORTED | `ls-server/tests/fake_bsp_e2e.rs`, `bootstrap.rs`, `server_surface.rs` |
| RealBspCoreTest | ls-core | PORTED | `real_bsp_e2e.rs`, `real_bsp_pc.rs` (E0/E1/E4/E5) |
| RealBspLifecycleTest | ls-core | PORTED | `real_bsp_e2e.rs` (E2/E3/E6/E8 segment hygiene) |
| RealBspIntegrationTest | ls-core | PORTED | `real_bsp_e2e.rs`, `real_bsp_pc.rs` |
| RealBspIsolationTest | ls-core | PORTED | `real_bsp_pc_recovery.rs`, `ls-jvm/tests/live_recovery.rs` (E7 dispatch-generation recovery; E9 AOT non-port) |
| AotTrainIntegrationTest | ls-core | INTENTIONAL NON-PORT | AOT island cache / `AotTrain` out of scope (also E9) |
| MarshalSuite / CodecSuite / LayoutSuite / BoundarySuite / PcHostConfigSuite / PcHostOpsSuite | ls-pc-host | PORTED | `ls-pc-abi/tests/{roundtrip,fuzz,boundary,canary}.rs`; `ls-jvm/tests/live_boundary.rs` |
| PcQuerySuite / PcSymbolSearchSuite / Utf16TextSuite / WorkerProtocolSuite / PcWorkerManagerSuite / ForkedWorkerSuite / PluginManagerSuite / CompilerPluginConfigSuite | ls-pc | PORTED | `ls-jvm/tests/{live_boundary,live_definition,live_recovery,live_zaozi}.rs`; `real_bsp_pc*.rs`. The old forked-worker/LRU-manager shape is replaced by the embedded island + watchdog; plugin-internals stay island-side (retained Scala). |
| FuzzyRankSuite | (sqlite metastore) | PORTED | `ls-store/tests/search.rs` (fuzzy ranking moved to the segment search section) |
| Sqlite3Suite / SchemaSuite / ReaderPoolSuite / MetaStoreSuite / DbSuite / TempDbFixture / IngestCheckpointSuite | sqlite | INTENTIONAL NON-PORT | DEC-7 removed SQLite (DB/WAL/schema/reader-pool/metastore/checkpoint); persistence + search replaced by the segment store, covered by `ls-store/tests/*` |
| BenchSuite / OfflineCompileGuardSuite | ls-bench | TASK22 OBLIGATION | bench-smoke port; no server runtime behavior to assert now |
| FakeBuildServer, E2eSupport, RealBspFixture, DoctorTestSupport, PcTestHarness, TestSupport, FixtureWorkspace, ProtoTestWriter | (various) | SUPPORT | fixtures/harnesses, not standalone suites |

## §18.1 case coverage

All 22 families are ported with the **same exactness the Scala oracle asserts**:

- **Exact span set / exact rejection reason (12 safe + all unsafe families):**
  export forwarder, case-class copy, var getter/setter, val member (cross-file),
  local val, nested local def, private member, top-level def/val, extension
  method, given/using, inline def, opaque type; and every unsafe family
  (override, generated, readonly, dependency, external, synthetic-only,
  PC-only, unsaved dirty buffer, shared-source disagreement, stale-md5 /
  fresh-snapshot, compile failure). Rust pins these with
  `assert_eq!(spans_in(...), token_set(...))` / typed-error matches — identical
  to the Scala `assertEquals(...toSet, tokenSpans.toSet)`.

- **Inclusion / exclusion families (class-unify, apply-sugar, trait, object,
  enum, method-overload):** these six are asserted with
  `assert(containsToken(...))` + `nonEmpty` + critical exclusions **in the Scala
  oracle itself** (`ReferencesAndQuerySuite` lines 56, 75, 84, 89, 93, 100 — no
  `assertEquals` on the span set), because they carry synthetic
  companion/constructor/`apply` occurrences whose exact set is
  implementation-defined. The Rust matrix mirrors this shape exactly
  (`references_matrix.rs` `class_references_unify_*`, `apply_sugar_unification_*`,
  `trait_references_*`, `object_references_*`, `enum_references_*`,
  `method_overloads_stay_separate`). This is **faithful parity, not a gap**:
  strengthening the Rust side to an exact set would over-specify beyond the
  ported spec and risk failing on legitimate synthetic occurrences the oracle
  deliberately tolerates.

## E0–E8 row coverage

(The docs number these E1..E10; `RealBspCoreTest` also carries an E0. All
non-trimmed rows are ported over live mill.)

| E-row | Rust equivalent |
|---|---|
| E0 real mill boots + index fills | `real_bsp_e2e.rs::real_bsp_doctor_symbol_references_and_rename_over_live_mill` |
| E1 no-SemanticDB hard error + doctor ERROR | `real_bsp_semanticdb_is_mandatory_on_the_uncovered_module` |
| E2 diagnostics forwarded then cleared | `real_bsp_compile_error_is_forwarded_then_cleared_by_the_fix` |
| E3 didSave compile/reingest updates positions | `real_bsp_save_driven_reingest_reflects_new_token_positions` |
| E4 rename rejection paths | `real_bsp_rename_rejections_carry_the_typed_reason` |
| E5 hover/signatureHelp/definition/documentHighlight | `real_bsp_pc.rs::real_bsp_presentation_compiler_position_features_and_completion`, `real_bsp_documenthighlight_returns_in_file_occurrences` |
| E6 shared-source references + rename consistency | `real_bsp_shared_source_unifies_references_and_passes_rename_consistency` |
| E7 PC dispatch-generation recovery | `real_bsp_pc_recovery.rs::real_bsp_forked_pc_recovers_from_a_dispatch_wedge`; `ls-jvm/tests/live_recovery.rs` |
| E8 segment hygiene (repeated save) | `real_bsp_repeated_saves_keep_a_single_committed_segment_dir` |

E8's no-BSP warm-restart half is DEC-1/DEC-2 trimmed (task22 deferral note);
E9 AOT and E10 CI wiring are out of scope.

## Real gaps

1. **`RandomCorpusTest` (ls-postings) → CLOSED this round.** The retained Scala
   suite drives a 200-doc / 2000-symbol / ~20k-occurrence seeded-random corpus
   through the writer and checks every reader obligation against a brute-force
   reference. The Rust store had the deterministic `HandBuiltCorpusTest`/
   `IntervalIndexTest` equivalents and a small `bruteforce_matches_naive_scan`,
   but no scaled differential. Ported as `crates/ls-store/tests/random_corpus.rs`
   (dependency-free `splitmix64` generator; 11 differential tests covering group
   scans with/without target pruning, definition/rename scans, epoch-stale
   dropping, per-doc scans, rename-profile round-trip, the symbol and doc
   dictionaries, and 2000 `symbol_at` probes) — all green.

2. **`IngestJanitorSuite` publish-time auto-reclaim (ls-postings/ls-rename) →
   CLOSED this round.** The held-snapshot-defers-deletion half was already
   covered (`store.rs::janitor_defers_deletion_until_snapshot_drops`), but the
   publish-TIME half — a publish auto-reclaiming a *drained, unretained*
   superseded generation with no explicit janitor — was proven only by the
   `LS_REAL_BSP_IT`-gated `real_bsp_e2e.rs::real_bsp_repeated_saves_keep_a_single_committed_segment_dir`.
   Added a fast unit test
   `store.rs::publishes_auto_reclaim_drained_superseded_generations` that pins
   the real contract (a publish pins the immediately-prior generation across its
   own janitor, so a drained generation is reclaimed by the next publish; drained
   generations do not accumulate).

The six §18.1 inclusion-family items Codex flagged are faithful parity (above),
not gaps. No other mandatory behavior lacks a proving Rust test.

## Task22 obligations (recorded, not this round)

- Reconcile `docs/traceability.md` — it is the stale Scala-era doc (old
  AC-1..AC-20 numbering, SQLite ACs) and must be rewritten/retired at cutover.
- Port the bench smoke (`BenchSuite`/`OfflineCompileGuardSuite`).
- Record the DEC-1/DEC-2 deferral notes (`pcPluginStatus`, no-BSP warm restart)
  in the cutover docs; both must stay absent from the advertised surface.
- The deletion sweep of the retained Scala modules.
