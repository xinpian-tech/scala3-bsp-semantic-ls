# Rust port coverage audit

> **Cutover note.** The big-bang deletion has happened: the Scala modules this
> audit inventories (`ls-core`, `ls-rename`, `ls-sqlite-ffm`, `ls-postings`,
> `ls-bsp`, `ls-semanticdb`, `ls-index-model`, `ls-doctor`, `ls-bench`, the
> worker-protocol files of `ls-pc`, and the core-dependent zaozi suites) were
> deleted after this mapping was verified. Rows for deleted paths are preserved
> as the port's evidence record; `scripts/check-audit-inventory.sh` now checks
> the retained island suites only.

This audit maps **every** retained Scala test file under `modules/*/test/` (a
path-qualified inventory, so duplicate basenames like the two
`Jdk25GuardSuite.scala` never collapse), plus the §18.1 references/rename
correctness matrix and the E0–E8 real-BSP rows, onto the Rust test(s) that prove
it, and classifies the gaps. It is the fixture-coverage acceptance gate for the
big-bang rewrite (the plan's CORE_RISKS: "big-bang makes fixture coverage
existential"). `scripts/check-audit-inventory.sh` mechanically checks that every
retained test-file path appears here.

**Classification codes.**
- **PORTED** — a Rust test proves the same behavior (named in the row).
- **NON-PORT** — out of scope by a ratified decision (SQLite removed by the
  decision-table row 7 of the rewrite decision record, `rlcr-rust.md:284`; AOT /
  `AotTrain`; `ls-tasty` v1.1; the forked PC worker, replaced by the embedded
  island; or a DEC-1/DEC-2 trim). The row states the authority.
- **ISLAND** — the JVM presentation-compiler island is **not rewritten** (`ls-pc`
  facade/SPI, `ls-zaozi-pcplugin`; decision table row 1, `rlcr-rust.md:278`).
  These suites stay green on the retained Scala code (**AC-6.5 island parity**);
  the Rust side proves the vtable boundary (named in the row).
- **TASK22** — strictly docs/deletion/bench work with no runtime behavior to
  assert now.
- **SUPPORT** — a fixture/harness, not a standalone suite.
- **CORPUS** — new island-only munit suites ported from the Scala 3 (dotty)
  3.8.4 presentation-compiler test corpus onto the `PcFacade` surface
  (attribution in `NOTICE.md`). These are additions guarding the retained
  island against PC-version bumps, not ports of deleted Scala suites; they
  run in `mill pc.test` and have no Rust counterpart by design.

> Note: SQLite removal's authority is the **decision-table row 7** of the ratified
> rewrite decision record, NOT "DEC-7" (DEC-7 is the `PcFacade.diagnostics`-ABI
> decision, `rlcr-rust.md:229`). This audit cites the decision table.

## Verdict

- **§18.1: fully ported (22/22 families)** — exact span sets / exact rejection
  reasons where the Scala oracle asserts them; inclusion-only where the Scala
  oracle itself is inclusion-only (faithful parity, see §18.1 section).
- **E0–E8: every non-trimmed row ported** over live mill.
- **Zaozi cross-file navigation re-pointed at the production vtable** in
  `crates/ls-jvm/tests/live_zaozi.rs` (verified live); the single-buffer
  plugin-internal steering shapes stay ISLAND (AC-6.5).
- Gaps found and closed across R15/R16: `RandomCorpusTest` (differential),
  `IngestJanitorSuite` publish-time auto-reclaim, and the Zaozi normal-member /
  exact-range / hover / missing-field boundary cases.

## Path-qualified inventory (all retained Scala test files)

| Path | Class | Proving Rust test / rationale |
|---|---|---|
| modules/ls-bench/test/src/ls/bench/BenchSuite.scala | TASK22 | bench-smoke port; no server runtime behavior to assert now |
| modules/ls-bench/test/src/ls/bench/OfflineCompileGuardSuite.scala | TASK22 | offline-CI compile guard; bench/packaging obligation |
| modules/ls-bsp/test/src/ls/bsp/BspDiscoveryTest.scala | PORTED | `ls-bsp/tests/discovery.rs` |
| modules/ls-bsp/test/src/ls/bsp/BspLaunchTest.scala | PORTED | `ls-bsp/tests/launch.rs` |
| modules/ls-bsp/test/src/ls/bsp/BspProjectModelTest.scala | PORTED | `ls-bsp/tests/project_model.rs` |
| modules/ls-bsp/test/src/ls/bsp/BspSessionTest.scala | PORTED | `ls-bsp/tests/session.rs`, `mill_smoke.rs` |
| modules/ls-bsp/test/src/ls/bsp/FakeBuildServer.scala | SUPPORT | in-file fake BSP harness (→ `ls-server/tests/fake_bsp_e2e.rs`) |
| modules/ls-bsp/test/src/ls/bsp/SemanticdbFlagsTest.scala | PORTED | `ls-bsp/tests/semanticdb_flags.rs` |
| modules/ls-core/test/src/ls/core/AotTrainIntegrationTest.scala | NON-PORT | AOT island cache / `AotTrain` out of scope (also E9) |
| modules/ls-core/test/src/ls/core/BootstrapJanitorSuite.scala | PORTED | `ls-store/tests/store.rs::open_readonly_recovers_without_creating_or_cleaning` (+ `assert_no_tmp_debris`) |
| modules/ls-core/test/src/ls/core/BootstrapRecoverySuite.scala | PORTED | `ls-server/tests/bootstrap.rs::no_bsp_recovered_index_*`; `ls-store/tests/store.rs::{torn_state_tmp_recovers_old,crash_after_state_before_manifest_recovers_old,state_generation_mismatch_rejected}` |
| modules/ls-core/test/src/ls/core/BuildTargetsChangeBufferingSuite.scala | PORTED | `ls-server/tests/bootstrap.rs::a_build_target_change_*` |
| modules/ls-core/test/src/ls/core/CapabilitiesSuite.scala | PORTED | `ls-server/src/capabilities.rs` tests; `server_surface.rs` (incl. the advertised `pcPluginStatus`) |
| modules/ls-core/test/src/ls/core/DiagnosticRouterSuite.scala | PORTED | `ls-server/src/diagnostics.rs` tests; `fake_bsp_e2e.rs::compile_error_diagnostics_are_published_to_the_client` |
| modules/ls-core/test/src/ls/core/DoctorCommandSuite.scala | PORTED | `ls-server/tests/server_surface.rs::execute_command_doctor_renders_text_and_json` |
| modules/ls-core/test/src/ls/core/E2eSupport.scala | SUPPORT | fake-BSP e2e helpers |
| modules/ls-core/test/src/ls/core/ExecuteCommandSuite.scala | PORTED | `ls-server/tests/server_surface.rs::execute_command_reindex_compile_and_unknown` + `advertised_execute_commands_are_exactly_the_routed_ones` (`pcPluginStatus` implemented; wire round-trip in `ls-server/tests/pc_wire.rs`) |
| modules/ls-core/test/src/ls/core/IndexPcDefinitionResolverSuite.scala | PORTED | `ls-engine/tests/engine.rs::symbol_definition_*`; `ls-jvm/tests/live_definition.rs` |
| modules/ls-core/test/src/ls/core/LsEndToEndTest.scala | PORTED | `ls-server/tests/fake_bsp_e2e.rs`, `bootstrap.rs`, `server_surface.rs` |
| modules/ls-core/test/src/ls/core/LspConvertSuite.scala | PORTED | `ls-server/src/convert.rs` tests |
| modules/ls-core/test/src/ls/core/PcBackendSuite.scala | PORTED | `ls-jvm/tests/{live_boundary,live_recovery}.rs`; `real_bsp_pc_recovery.rs` (embedded island/watchdog model) |
| modules/ls-core/test/src/ls/core/PcOverlaySuite.scala | PORTED | `ls-server/src/pc_overlay.rs` tests; `references_matrix.rs` overlay tests; `fake_bsp_e2e.rs` PC-only-over-the-wire |
| modules/ls-core/test/src/ls/core/RealBspCoreTest.scala | PORTED | `real_bsp_e2e.rs`, `real_bsp_pc.rs` (E0/E1/E4/E5) |
| modules/ls-core/test/src/ls/core/RealBspFixture.scala | SUPPORT | → `ls-server/tests/real_bsp_common/mod.rs` |
| modules/ls-core/test/src/ls/core/RealBspIntegrationTest.scala | PORTED | `real_bsp_e2e.rs`, `real_bsp_pc.rs` |
| modules/ls-core/test/src/ls/core/RealBspIsolationTest.scala | PORTED | `real_bsp_pc_recovery.rs`, `ls-jvm/tests/live_recovery.rs` (E7; E9 AOT NON-PORT) |
| modules/ls-core/test/src/ls/core/RealBspLifecycleTest.scala | PORTED | `real_bsp_e2e.rs` (E2/E3/E6/E8) |
| modules/ls-core/test/src/ls/core/UrisSuite.scala | PORTED | `ls-index-model/src/uri.rs` tests |
| modules/ls-doctor/test/src/ls/doctor/BspLauncherCompatTest.scala | PORTED | `ls-bsp/tests/session.rs`, `mill_smoke.rs` |
| modules/ls-doctor/test/src/ls/doctor/DoctorTestSupport.scala | SUPPORT | doctor test fixtures |
| modules/ls-doctor/test/src/ls/doctor/RenderTest.scala | PORTED | `ls-server/src/doctor.rs::text_renders_the_seven_sections_in_fixed_order`, `unavailable_sections_render_the_reason_not_omitted` |
| modules/ls-doctor/test/src/ls/doctor/RuntimeNixSectionsTest.scala | PORTED | `ls-server/src/doctor.rs::text_renders_runtime_nix_and_pc_facts` |
| modules/ls-doctor/test/src/ls/doctor/StoreSectionsTest.scala | PORTED | `ls-server/src/doctor.rs::json_has_the_store_key_and_no_sqlite_or_postings`; real-BSP doctor checks |
| modules/ls-index-model/test/src/ls/index/RuntimeContractSuite.scala | PORTED | `ls-index-model/tests/properties.rs`, `src/text.rs` tests |
| modules/ls-index-model/test/src/ls/index/TargetGraphSuite.scala | PORTED | `ls-index-model/tests/properties.rs`, `src/bitset.rs` tests |
| modules/ls-pc-host/test/src/ls/pc/host/BoundarySuite.scala | PORTED | `ls-pc-abi/tests/boundary.rs`; `ls-jvm/tests/live_boundary.rs` |
| modules/ls-pc-host/test/src/ls/pc/host/codec/CodecSuite.scala | PORTED | `ls-pc-abi/tests/roundtrip.rs`, `fuzz.rs`, `boundary.rs` |
| modules/ls-pc-host/test/src/ls/pc/host/LayoutSuite.scala | PORTED | `ls-pc-abi/tests/canary.rs`, `boundary.rs` |
| modules/ls-pc-host/test/src/ls/pc/host/MarshalSuite.scala | PORTED | `ls-pc-abi/tests/roundtrip.rs` |
| modules/ls-pc-host/test/src/ls/pc/host/PcHostConfigSuite.scala | PORTED | `ls-jvm/tests/live_boundary.rs`; `ls-server/src/doctor.rs` PC section |
| modules/ls-pc-host/test/src/ls/pc/host/PcHostOpsSuite.scala | PORTED | `ls-pc-abi/tests/boundary.rs`; `ls-jvm/tests/live_boundary.rs` |
| modules/ls-pc/test/src/ls/pc/CompilerPluginConfigSuite.scala | ISLAND | pc-plugins.json compiler-plugin loading proven at the boundary by `ls-jvm/tests/live_zaozi.rs` (boots the island via a real `pc-plugins.json`); island loader unchanged (AC-6.5) |
| modules/ls-pc/test/src/ls/pc/FoldingRangeProviderSuite.scala | ISLAND | island-only custom provider (no dotty provider exists): the parser-only folding walker pinned by exact ranges+kinds — indentation/brace syntax, CRLF, unterminated-code recovery, comment blocks + `//`-runs, import runs, nested `// region` markers; served over the vtable `folding_range` slot |
| modules/ls-pc/test/src/ls/pc/PcV2OpsSuite.scala | ISLAND | island-only: the ABI v2 payload-query providers at the facade against the real PC — inlay hints (flag-bit gating, exact position/parts/data), semantic tokens, selection-range chains, every code-action id incl. the `DisplayableException` refusal-as-data case, auto-imports, pc diagnostics, folding wiring; the transport legs are `ls-jvm/tests/live_definition.rs` + `ls-pc-abi/tests/roundtrip.rs` |
| modules/ls-pc/test/src/ls/pc/ForkedWorkerSuite.scala | NON-PORT | forked PC worker deleted (`worker.scala`/`ForkedPcWorker`); replaced by the embedded island watchdog — `ls-jvm/tests/live_recovery.rs`, `real_bsp_pc_recovery.rs` |
| modules/ls-pc/test/src/ls/pc/PcQuerySuite.scala | ISLAND | PC queries proven over the vtable by `ls-jvm/tests/live_boundary.rs`, `real_bsp_pc.rs`; island query code unchanged (AC-6.5) |
| modules/ls-pc/test/src/ls/pc/PcSymbolSearchSuite.scala | PORTED | `ls-jvm/tests/live_definition.rs`; `ls-engine/tests/engine.rs::symbol_definition_*` |
| modules/ls-pc/test/src/ls/pc/PcTestHarness.scala | SUPPORT | PC test fixtures |
| modules/ls-pc/test/src/ls/pc/PcWorkerManagerSuite.scala | NON-PORT | forked LRU worker manager deleted; the embedded-island supervisor/generation lifecycle is `ls-jvm/tests/{live_boundary,live_recovery}.rs`. The island's own LRU instance cap is unchanged (AC-6.5) |
| modules/ls-pc/test/src/ls/pc/PluginManagerSuite.scala | ISLAND | plugin SPI/loading unchanged in the island (AC-6.5); the loaded-plugin path is exercised at the boundary by `ls-jvm/tests/live_zaozi.rs` |
| modules/ls-pc/test/src/ls/pc/Utf16TextSuite.scala | ISLAND | UTF-16 conversion unchanged in the island (AC-6.5); exercised via live PC positions in `ls-jvm/tests/live_boundary.rs` + `ls-pc-abi` position payloads |
| modules/ls-pc/test/src/ls/pc/WorkerProtocolSuite.scala | NON-PORT | forked-worker wire protocol deleted; the vtable protocol is `ls-pc-abi/tests/roundtrip.rs`, `fuzz.rs`, `boundary.rs` |
| modules/ls-pc/test/src/ls/pc/corpus/CorpusPc.scala | SUPPORT | ported-corpus fixtures: plugin-free shared facade + dotty `MockEntries` definition map and a `TestingWorkspaceSearch`-style workspace-method registry on the `PcDefinitionResolver` seam (see NOTICE.md) |
| modules/ls-pc/test/src/ls/pc/corpus/CorpusHarness.scala | SUPPORT | ported-corpus harness: dotty `BasePCSuite`/`Base*Suite` check DSL re-homed onto `PcFacade` + munit (see NOTICE.md) |
| modules/ls-pc/test/src/ls/pc/corpus/CompletionCorpusSuite.scala | CORPUS | island-only: completion cases ported from scala3 3.8.4 `tests/completion/CompletionSuite.scala`; exercises the live facade in `mill pc.test` |
| modules/ls-pc/test/src/ls/pc/corpus/CompletionExtensionCorpusSuite.scala | CORPUS | island-only: extension-method completion cases from scala3 3.8.4 `tests/completion/CompletionExtensionSuite.scala` |
| modules/ls-pc/test/src/ls/pc/corpus/CompletionKeywordCorpusSuite.scala | CORPUS | island-only: given/using/derives keyword completion cases from scala3 3.8.4 `tests/completion/CompletionKeywordSuite.scala` |
| modules/ls-pc/test/src/ls/pc/corpus/SingletonCompletionsCorpusSuite.scala | CORPUS | island-only: singleton/literal/union-type completion cases from scala3 3.8.4 `tests/completion/SingletonCompletionsSuite.scala` |
| modules/ls-pc/test/src/ls/pc/corpus/CompletionCaseCorpusSuite.scala | CORPUS | island-only: enum match/case exhaustiveness cases from scala3 3.8.4 `tests/completion/{CompletionCaseSuite,CompletionMatchSuite}.scala` |
| modules/ls-pc/test/src/ls/pc/corpus/HoverTypeCorpusSuite.scala | CORPUS | island-only: hover cases from scala3 3.8.4 `tests/hover/HoverTypeSuite.scala` (union/intersection/enums/extension/using) |
| modules/ls-pc/test/src/ls/pc/corpus/HoverTermCorpusSuite.scala | CORPUS | island-only: hover cases from scala3 3.8.4 `tests/hover/HoverTermSuite.scala` (top-level defs, named tuples, opaque types) |
| modules/ls-pc/test/src/ls/pc/corpus/HoverDefnCorpusSuite.scala | CORPUS | island-only: hover-on-definition cases from scala3 3.8.4 `tests/hover/HoverDefnSuite.scala` |
| modules/ls-pc/test/src/ls/pc/corpus/SignatureHelpCorpusSuite.scala | CORPUS | island-only: signature-help cases from scala3 3.8.4 `tests/signaturehelp/SignatureHelpSuite.scala` (using/context/opaque/named params) |
| modules/ls-pc/test/src/ls/pc/corpus/SignatureHelpInterleavingCorpusSuite.scala | CORPUS | island-only: interleaved-clause signature-help cases from scala3 3.8.4 `tests/signaturehelp/SignatureHelpInterleavingSuite.scala` |
| modules/ls-pc/test/src/ls/pc/corpus/PcDefinitionCorpusSuite.scala | CORPUS | island-only: definition cases from scala3 3.8.4 `tests/definition/PcDefinitionSuite.scala` (export/enum/derives/extension); cross-file via the mock `PcDefinitionResolver` |
| modules/ls-pc/test/src/ls/pc/corpus/TypeDefinitionCorpusSuite.scala | CORPUS | island-only: typeDefinition cases from scala3 3.8.4 `tests/definition/TypeDefinitionSuite.scala`; cross-file via the mock `PcDefinitionResolver` |
| modules/ls-pc/test/src/ls/pc/corpus/InlayHintsCorpusSuite.scala | CORPUS | island-only: inlay-hint cases from scala3 3.8.4 `tests/inlayHints/InlayHintsSuite.scala` — a curated 29-case subset favoring Scala 3 syntax (givens/using, quotes, named tuples, xray chains, closing labels, pattern-match flag pairs), rendered through the harness port of `TestInlayHints` with the dotty base's all-flags bitset |
| modules/ls-pc/test/src/ls/pc/corpus/SelectionRangeCorpusSuite.scala | CORPUS | island-only: the FULL scala3 3.8.4 `tests/SelectionRangeSuite.scala` (18 cases) plus `tests/SelectionRangeCommentSuite.scala` (`comment-` prefixed), asserting each expected `>>region>><<region<<` step of the innermost-first chain |
| modules/ls-postings/test/src/ls/postings/CorruptionTest.scala | PORTED | `ls-store/tests/segment.rs`, `validation.rs`, `store.rs` corruption cases |
| modules/ls-postings/test/src/ls/postings/CurrentSnapshotFileSuite.scala | PORTED | `ls-store/tests/store.rs::publish_then_reopen_preserves_pair`, `second_publish_increments_generation` (current.json → manifest+generation) |
| modules/ls-postings/test/src/ls/postings/HandBuiltCorpusTest.scala | PORTED | `ls-store/tests/segment.rs`, `symbol_at.rs` |
| modules/ls-postings/test/src/ls/postings/IntervalIndexTest.scala | PORTED | `ls-store/tests/symbol_at.rs::interval_block_pruning`, `segment.rs::multi_block_group_chunks_and_skips` |
| modules/ls-postings/test/src/ls/postings/Jdk25GuardSuite.scala | NON-PORT | Rust store is not Java-25-bound; guard obsolete (distinct from the sqlite `Jdk25GuardSuite`) |
| modules/ls-postings/test/src/ls/postings/RandomCorpusTest.scala | PORTED | `ls-store/tests/random_corpus.rs` (seeded differential; added R15) |
| modules/ls-postings/test/src/ls/postings/SnapshotJanitorTest.scala | PORTED | `ls-store/tests/store.rs::janitor_defers_deletion_until_snapshot_drops`, `publishes_auto_reclaim_drained_superseded_generations` |
| modules/ls-postings/test/src/ls/postings/SnapshotLifecycleTest.scala | PORTED | `ls-store/tests/store.rs::retained_snapshot_survives_publish`, `second_publish_increments_generation` |
| modules/ls-postings/test/src/ls/postings/TestSupport.scala | SUPPORT | `ls-store/tests/*` helpers |
| modules/ls-rename/test/src/ls/rename/FixtureWorkspace.scala | SUPPORT | → `ls-engine/tests/fixture/mod.rs` |
| modules/ls-rename/test/src/ls/rename/IngestCheckpointSuite.scala | NON-PORT | asserts the old SQLite-WAL publish-tail checkpoint (`IngestCheckpointSuite.scala:5,17,30`); WAL checkpointing disappeared with SQLite (decision-table row 7). The segment store's equivalent durability obligations — bounded committed generations, publish-time janitor, and manifest/state durability — are `ls-store/tests/store.rs::{publishes_auto_reclaim_drained_superseded_generations, janitor_defers_deletion_until_snapshot_drops, crash_after_state_before_manifest_recovers_old, torn_state_tmp_recovers_old}` |
| modules/ls-rename/test/src/ls/rename/IngestJanitorSuite.scala | PORTED | `ls-store/tests/store.rs::publishes_auto_reclaim_drained_superseded_generations` (publish-time) + `janitor_defers_deletion_until_snapshot_drops` (held-then-released); live `real_bsp_e2e.rs::real_bsp_repeated_saves_keep_a_single_committed_segment_dir` |
| modules/ls-rename/test/src/ls/rename/RawPathSuite.scala | PORTED | `ls-engine/tests/engine.rs::raw_path_write_through_runs_inline_and_heals`, `references_raw_fallback_serves_same_doc` |
| modules/ls-rename/test/src/ls/rename/ReferencesAndQuerySuite.scala | PORTED | `ls-engine/tests/references_matrix.rs` — assertion-for-assertion parity (see §18.1 section) |
| modules/ls-rename/test/src/ls/rename/RenameMutationSuite.scala | PORTED | `ls-engine/tests/rename_matrix.rs` (`stale_md5_*`, `fresh_snapshot_*`, `shared_source_disagreement_rejected`) |
| modules/ls-rename/test/src/ls/rename/RenameSuite.scala | PORTED | `ls-engine/tests/rename_matrix.rs` (exact edit spans + rejection reasons) |
| modules/ls-rename/test/src/ls/rename/UnitSuites.scala | PORTED | identifier backtick-quoting `ls-engine/tests/engine.rs` (`identifiers::encode`); symbol string round-trip `engine.rs` + `ls-semanticdb/tests/symbols.rs`; reverse-dependency closure + duplicate-bspId refusal `ls-bsp/tests/project_model.rs`, `ls-engine/src/targets.rs` |
| modules/ls-semanticdb/test/src/ls/semanticdb/GroupsSuite.scala | PORTED | `ls-semanticdb/tests/groups.rs`, `pipeline.rs` |
| modules/ls-semanticdb/test/src/ls/semanticdb/LocatorSuite.scala | PORTED | `ls-semanticdb/tests/locator.rs` |
| modules/ls-semanticdb/test/src/ls/semanticdb/Md5Suite.scala | PORTED | `ls-semanticdb/tests/md5.rs`, `pipeline.rs` |
| modules/ls-semanticdb/test/src/ls/semanticdb/ProtoTestWriter.scala | SUPPORT | `ls-semanticdb/tests/common/mod.rs`, `ls-engine/tests/common/mod.rs` |
| modules/ls-semanticdb/test/src/ls/semanticdb/ScalacIntegrationSuite.scala | PORTED | `ls-semanticdb/tests/scalac_integration.rs` (incl. `derives_clause_case_class_defined_and_derived_given_synthetic_only`) |
| modules/ls-semanticdb/test/src/ls/semanticdb/SymbolStringsSuite.scala | PORTED | `ls-semanticdb/tests/symbols.rs` |
| modules/ls-semanticdb/test/src/ls/semanticdb/WireDecoderSuite.scala | PORTED | `ls-semanticdb/tests/wire.rs` |
| modules/ls-sqlite-ffm/test/src/ls/sqlite/DbSuite.scala | NON-PORT | SQLite DB/WAL removed (decision-table row 7) |
| modules/ls-sqlite-ffm/test/src/ls/sqlite/FuzzyRankSuite.scala | PORTED | `ls-store/tests/search.rs` (FuzzyRank moved to the segment search section) |
| modules/ls-sqlite-ffm/test/src/ls/sqlite/Jdk25GuardSuite.scala | NON-PORT | SQLite module removed (decision-table row 7); distinct from the ls-postings `Jdk25GuardSuite` |
| modules/ls-sqlite-ffm/test/src/ls/sqlite/MetaStoreSuite.scala | NON-PORT | SQLite metastore/FTS removed; search → `ls-store/tests/search.rs`, persistence → the segment store |
| modules/ls-sqlite-ffm/test/src/ls/sqlite/ReaderPoolSuite.scala | NON-PORT | SQLite reader-connection pool removed (mmap segments need none) |
| modules/ls-sqlite-ffm/test/src/ls/sqlite/SchemaSuite.scala | NON-PORT | SQLite schema/migration removed; superseded by the manifest/state pairing tests |
| modules/ls-sqlite-ffm/test/src/ls/sqlite/Sqlite3Suite.scala | NON-PORT | SQLite FFM binding removed (decision-table row 7) |
| modules/ls-sqlite-ffm/test/src/ls/sqlite/TempDbFixture.scala | SUPPORT | SQLite test fixture (module removed) |
| modules/ls-zaozi-pcplugin/test/src/ls/zaozi/pcplugin/ZaoziPcCrossFileSuite.scala | PORTED | boundary re-pointed at the vtable: `ls-jvm/tests/live_zaozi.rs` — Dynamic field + normal member both reach the compiled-dependency library SOURCE through the index-backed `symbol_definition` resolver; the no-resolver baseline is implied by asserting the resolver produced the location |
| modules/ls-zaozi-pcplugin/test/src/ls/zaozi/pcplugin/ZaoziPcForkedSuite.scala | NON-PORT | tests the forked PC worker transport (`--plugin-config` IPC), which is deleted (embedded island). The still-relevant `pc-plugins.json` compiler-plugin loading is proven at the boundary by `ls-jvm/tests/live_zaozi.rs`; plugin status by the doctor PC-Plugins section |
| modules/ls-zaozi-pcplugin/test/src/ls/zaozi/pcplugin/ZaoziPcNavSuite.scala | PORTED+ISLAND | cross-file go-to, exact `val a` name range, hover round-trip, missing-field no-crash, and non-zaozi selectivity are re-pointed at the vtable in `ls-jvm/tests/live_zaozi.rs` (verified live). The single-buffer plugin-INTERNAL steering shapes (macro-expanded `getRefViaFieldValName`, nested/optional fields, writable receivers, applyDynamic identity, exact hover text) test the untouched Scala plugin and stay green there (AC-6.5, island) |

## §18.1 case coverage

Fully ported (22/22 families). Every safe family the Scala `ReferencesAndQuerySuite`/`RenameSuite`/`RenameMutationSuite` pins with an exact span set, and every unsafe family's exact rejection reason, is pinned identically in `crates/ls-engine/tests/{references_matrix,rename_matrix}.rs`; the SemanticDB characterizations (`ScalacIntegrationSuite`) are in `crates/ls-semanticdb/tests/scalac_integration.rs`.

The six families class-unify / apply-sugar / trait / object / enum / method-overload are asserted **inclusion-only in the Scala oracle itself** (`ReferencesAndQuerySuite` lines 56/75/84/89/93/100 use `assert(containsToken)` + non-empty + critical exclusions, not `assertEquals(...toSet)`), because they carry synthetic companion/constructor/`apply` occurrences whose exact set is implementation-defined. `references_matrix.rs` mirrors that shape 1:1 — faithful parity, not a gap.

## E0–E8 row coverage

Every non-trimmed row is ported over live mill:

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

E8's no-BSP warm-restart half is DEC-1/DEC-2 trimmed (task22 deferral note); E9 AOT and E10 CI wiring are out of scope.

## Zaozi navigation: what is re-pointed at the vtable

The `ls-zaozi-pcplugin` module is **untouched** (decision table row 1). What the rewrite changed is the *transport*: navigation now flows through the embedded-island vtable + the Rust `symbol_definition` resolver instead of the deleted forked worker. So the **cross-file navigation** obligation is re-pointed at the production vtable in `crates/ls-jvm/tests/live_zaozi.rs` (verified live with the mill-built `zaoziPcplugin.jar`): the zaozi Dynamic field `io.a` and the normal member `io2.normalMethod()` both reach the compiled-dependency library SOURCE through the resolver; the definition lands on the exact `val a` name range; the `hover` vtable op round-trips with the plugin loaded; a missing zaozi field does not wedge the island; and a non-zaozi Dynamic access is left unchanged (plugin selectivity). The single-buffer plugin-internal steering shapes stay ISLAND (AC-6.5).

## Real gaps (found and closed)

1. **`RandomCorpusTest` → `crates/ls-store/tests/random_corpus.rs`** (R15): a dependency-free `splitmix64` differential (200 docs / 2000 symbols / ~20k occurrences) vs a brute-force oracle across all scans, epoch-stale dropping, both dictionaries, rename profiles, and 2000 `symbol_at` probes.
2. **`IngestJanitorSuite` publish-time auto-reclaim → `store.rs::publishes_auto_reclaim_drained_superseded_generations`** (R15): the publish-time half (a publish reclaiming a drained, unretained superseded generation) previously had only the gated E8 test.
3. **Zaozi normal-member cross-file / exact range / hover / missing-field → `live_zaozi.rs`** (R16): the boundary cases `ZaoziPcCrossFileSuite`/`ZaoziPcNavSuite` prove that were not yet re-pointed at the vtable.

No other mandatory behavior lacks a proving Rust test.

## Task22 obligations (recorded, not audited-as-covered)

- Reconcile `docs/traceability.md` (stale Scala-era doc: old AC-1..AC-20 numbering, SQLite ACs).
- Port the bench smoke (`BenchSuite`/`OfflineCompileGuardSuite`).
- Record the DEC-1/DEC-2 deferral notes in the cutover docs. (`pcPluginStatus` has since been implemented and advertised — the trim latitude was not used; the no-BSP warm-restart deferral note stands and stays absent from the advertised surface.)
- The deletion sweep of the retained Scala modules (incl. `ls-sqlite-ffm`, the forked-worker files, `AotTrain`).
