# Traceability

The mechanical map from the accepted v2 rewrite mandates (the decision record in
`plan-rust.md` §0 and the ratified acceptance areas built on it) to the concrete
tests and checks that prove them. `scripts/check-docs.sh` verifies that every
test file and repo path named here exists and that every `file` :: "case" entry
resolves to a real test; `scripts/check-audit-inventory.sh` separately verifies
that `docs/coverage-audit.md` accounts for every retained Scala test file.

The suite-level port mapping (every retained-or-deleted Scala suite → the Rust
test that replaced it, including the §18.1 correctness matrix and the E0–E8
real-BSP rows) lives in `docs/coverage-audit.md`; this file maps the mandates.

## Mandate map

| Mandate | What it requires | Proving tests / checks |
|---|---|---|
| Storage core | segment write→mmap read round-trip, v1 format + `target-meta.bin`/`symbol-meta.bin`/`search.bin`, CRC rejection, `manifest.json` single commit point, generational workspace-state pairing, janitor, recovery matrix (torn manifest/state, kill −9 windows), ArcSwap snapshot retain/release | `crates/ls-store/tests/segment.rs`, `crates/ls-store/tests/store.rs`, `crates/ls-store/tests/validation.rs`, `crates/ls-store/tests/random_corpus.rs`, `crates/ls-index-model/tests/properties.rs` |
| SemanticDB ingest | prost-shape wire decode, md5 freshness, normalization, ref/rename group building with the exact `UnsafeReason` mask, scalac `-Xsemanticdb` fixtures | `crates/ls-semanticdb/tests/wire.rs`, `crates/ls-semanticdb/tests/md5.rs`, `crates/ls-semanticdb/tests/pipeline.rs`, `crates/ls-semanticdb/tests/groups.rs`, `crates/ls-semanticdb/tests/symbols.rs`, `crates/ls-semanticdb/tests/locator.rs`, `crates/ls-semanticdb/tests/scalac_integration.rs` |
| Engines + orchestrator | IndexPath / RawSemanticDBPath / PCPath, BestEffort/FreshPreferred/FreshRequired, write-through parity (inline full-generation heal), references fan-out + reverse-dependency-closure pruning + epoch filtering, rename FreshRequired ladder + `RenameProfile` gate | `crates/ls-engine/tests/engine.rs`, `crates/ls-store/tests/symbol_at.rs` |
| §18.1 correctness matrix | every safe family asserts exact span sets / exact edit spans; every unsafe family asserts its exact rejection reason | `crates/ls-engine/tests/references_matrix.rs`, `crates/ls-engine/tests/rename_matrix.rs` |
| Workspace-symbol search | deterministic tiering (exact > prefix > camel-hump/subsequence), bounded candidate pull, exact-name membership, multi-token/owner/package semantics | `crates/ls-store/tests/search.rs` |
| BSP client | `.bsp/*.json` discovery ordering + typed errors, handshake, buildTargets/sources (directory expansion), scalacOptions, compile outcomes, inverseSources gating + fallback, dependencySources/outputPaths best-effort, timeouts, stderr pump, shutdown ladder | `crates/ls-bsp/tests/discovery.rs`, `crates/ls-bsp/tests/launch.rs`, `crates/ls-bsp/tests/session.rs`, `crates/ls-bsp/tests/project_model.rs`, `crates/ls-bsp/tests/semanticdb_flags.rs`; live: `crates/ls-bsp/tests/mill_smoke.rs` (`scripts/it-mill-smoke.sh`) |
| C ABI | `#[repr(C)]` boundary structs, cbindgen header, two-sided layout canary, lossless encodings (nullable-vs-empty, origin tags), single-`alloc` response protocol, decode-boundary fuzzing | `crates/ls-pc-abi/tests/boundary.rs`, `crates/ls-pc-abi/tests/canary.rs`, `crates/ls-pc-abi/tests/roundtrip.rs`, `crates/ls-pc-abi/tests/fuzz.rs`; Java-side offset tests in `modules/ls-pc-host/test` |
| Embedded-JVM boot | dlopen + `JNI_CreateJavaVM` premain-only registration, rendezvous deadline, zero-JNIEnv discipline, loaned-thread dispatch | `crates/ls-jvm-spike/tests/scenarios.rs` (flake check `spike-boundary`), `crates/ls-jvm-spike/tests/no_jni_guard.rs`, `crates/ls-jvm/tests/live_boundary.rs` (flake check `pc-boundary`) |
| Dispatch recovery | watchdog deadline → PC cancel → `restart_instances` → `spawn_dispatch(gen+1)` + replay; bounded abandoned generations; cap-exceeded → orderly exit | `crates/ls-jvm/tests/live_recovery.rs` (flake check `pc-recovery`), `crates/ls-server/tests/real_bsp_pc_recovery.rs` |
| Cross-file navigation callback | `symbol_definition` resolver over the immutable snapshot, forward-closure pruning, zaozi plugin steering over the live vtable | `crates/ls-jvm/tests/live_definition.rs` (flake check `pc-definition`), `crates/ls-jvm/tests/live_zaozi.rs` (flake check `pc-zaozi`), `crates/ls-server/tests/live_pc.rs` (flake check `pc-server-definition`) |
| Island parity (retained Scala) | `pc-plugins.json` loading, plugin SPI self-test/disable-on-throw, `PcTargetConfig`, `Utf16Text`, LRU instance cap, SemanticDB-flag stripping | mill suites under `modules/ls-pc/test` (`PluginManagerSuite`, `PcQuerySuite`, `PcSymbolSearchSuite`, `Utf16TextSuite`, `PcWorkerManagerSuite`, `CompilerPluginConfigSuite`), `modules/ls-zaozi-pcplugin/test` (`ZaoziPcNavSuite`), `modules/ls-pc-host/test` |
| LSP server surface | capability set exactness, per-method pre-ready behavior, didChange buffering + reload, diagnostics router semantics, dirty-buffer overlay + PC-only detection, didSave debounce/single-flight, doctor section contract, CLI | unit tests in `crates/ls-server/src/` (capabilities, diagnostics, documents, pc_overlay, doctor, cli, convert), `crates/ls-server/tests/bootstrap.rs`, `crates/ls-server/tests/server_surface.rs` |
| Fake-BSP e2e | the end-to-end scenario set over a Rust-hosted fake BSP server | `crates/ls-server/tests/fake_bsp_e2e.rs` |
| PC wire surface (JVM-free) | completion, `completionItem/resolve`, hover, signatureHelp, definition/typeDefinition over the framed wire through the REAL serve loop + REAL `IndexBootstrap`, with the island replaced by the testkit fake PC through the `IndexBootstrap::with_pc` seam; gating (`require_semanticdb`, `withPcBuffer`, the resolve target gate) and response mapping pinned by insta snapshots | `crates/ls-server/tests/pc_wire.rs`, `crates/ls-testkit/src/fake_pc.rs` |
| Shared wire harness | the one copy of the framed-wire builders, the interactive wire client (in-process serve loop or spawned binary over stdio), the fake BSP server, and the fixture-corpus geometry consumed by the `ls-server` suites | `crates/ls-testkit/src/wire.rs`, `crates/ls-testkit/src/client.rs`, `crates/ls-testkit/src/fake_bsp.rs`, `crates/ls-testkit/src/fixtures.rs`, `crates/ls-testkit/src/positions.rs` |
| Black-box stdio e2e | the REAL `ls-server` binary spawned over stdio by an independent client (pytest-lsp), against the scriptable Python fake BSP server over the committed fixture corpus: capability exactness through lsprotocol's typed model, readiness, index queries, diagnostics publish/clear, typed unknown-method/command errors | `it/lsp-blackbox/` (`conftest.py`, `fake_bsp.py`, `test_lifecycle.py`, `test_index_queries.py`, `test_diagnostics.py`, `test_robustness.py`), flake check `lsp-blackbox`, `scripts/it-lsp-blackbox.sh` |
| Project-level editor e2e | a REAL editor (headless Neovim) attaches the production server to a REAL third-party repo (the pinned, SemanticDB-patched zaozi source, CIRCT-free `decoder` module): readiness over the real mill BSP session, reindex ingest, workspace/symbol, cross-file definition, references, and PC-backed hover booting the embedded island against the real project classpath | `it/nvim/e2e.lua`, `scripts/it-nvim-zaozi.sh`, CI job `nvim-zaozi-e2e` |
| Real-BSP e2e | E0–E8 equivalents over live mill on `it/sample-workspace`; cold-start zero-JVM hard assertion (`/proc/self/maps`) | `crates/ls-server/tests/real_bsp_e2e.rs`, `crates/ls-server/tests/real_bsp_pc.rs`, `crates/ls-server/tests/real_bsp_pc_recovery.rs` (`scripts/it-real-bsp-rs.sh`) |
| Packaging | offline `nix build .#default` (Rust binary + island jars), packaged `--version`/offline `--doctor`/`dump`, Linux-only systems, ivy-lock hygiene | flake checks `package`, `package-cli`, `rust-build`, `rust-test`, `rust-clippy`, `rust-fmt`, `java25-toolchain`, `ivy-lock-present`, `pc-host-agent`; `scripts/check-ivy-lock.sh`, `scripts/check-offline-compile.sh` |
| Bench | ingest + query measurements over the real storage layer, ground-truth cross-checked, CI smoke gate | `crates/ls-bench/tests/smoke.rs`, `crates/ls-bench/src/lib.rs` (`cargo run -p ls-bench -- --smoke`) |

## Case map (spot anchors)

Selected load-bearing cases, mechanically checked (`file` :: "case substring"):

- `crates/ls-store/tests/store.rs` :: "crash_after_state_before_manifest_recovers_old"
- `crates/ls-store/tests/store.rs` :: "torn_manifest_tmp_recovers_old"
- `crates/ls-store/tests/store.rs` :: "state_generation_mismatch_rejected"
- `crates/ls-store/tests/segment.rs` :: "corrupt_data_file_crc_rejected"
- `crates/ls-store/tests/search.rs` :: "score_ranks_exact_above_prefix_above_subsequence"
- `crates/ls-semanticdb/tests/md5.rs` :: "validate_stale_when_text_changed"
- `crates/ls-engine/tests/engine.rs` :: "write_through"
- `crates/ls-engine/tests/rename_matrix.rs` :: "external"
- `crates/ls-engine/tests/rename_matrix.rs` :: "opaque"
- `crates/ls-bsp/tests/discovery.rs` :: "malformed"
- `crates/ls-jvm/tests/live_recovery.rs` :: "generation"
- `crates/ls-server/tests/server_surface.rs` :: "pre_ready_methods_take_their_per_method_fallbacks"
- `crates/ls-server/tests/real_bsp_e2e.rs` :: "zero"
- `crates/ls-server/tests/fake_bsp_e2e.rs` :: "diagnostics"
- `it/lsp-blackbox/test_lifecycle.py` :: "test_initialize_advertises_the_exact_capability_surface"
- `it/lsp-blackbox/test_diagnostics.py` :: "test_compile_diagnostics_publish_then_clear"
- `crates/ls-engine/tests/engine.rs` :: "symbol_definition_attributes_the_buffer_by_doc_row_under_a_shared_sourceroot"

## Recorded trims and accepted evolutions

Deviations the rewrite's decision process ratified; recorded here so the docs
and the plan stay reconciled.

1. **`pcPluginStatus` command — deferred (trim latitude).** Not advertised, not
   handled (an unknown-command error, asserted by
   `crates/ls-server/tests/server_surface.rs`); the doctor's `PC Plugins`
   section reports the deferral reason instead of live plugin status
   (`crates/ls-server/src/doctor.rs`). The `doctor`/`reindex`/`compile`
   commands remain mandatory and implemented.
2. **No-BSP warm-restart mode — deferred (trim latitude).** A workspace with a
   recovered on-disk index but no usable `.bsp` connection reaches a typed
   failed bootstrap ("the no-BSP warm-restart mode is deferred"), never a
   half-alive server (`crates/ls-server/src/bootstrap.rs`,
   `crates/ls-server/tests/bootstrap.rs`). The store still recovers and
   re-publishes the previous generation; only the serve-from-recovery mode is
   deferred.
3. **`documentHighlight` — retained** (advertised and index-served;
   `crates/ls-server/src/capabilities.rs`, `crates/ls-engine/tests/references_matrix.rs`).
4. **Deterministic search ordering.** FTS5 bm25 ordering is replaced by the
   deterministic tiering of the ported `FuzzyRank` (exact > prefix >
   camel-hump/subsequence, hump-hit bonus, length tiebreak). The match SET is
   unchanged; only ordering may differ. Ratified; tested in
   `crates/ls-store/tests/search.rs`.
5. **ID contract change.** Stable numeric ids (`SymbolId`/`DocId`/`TargetId`)
   existed to live in SQLite; the stable keys are now the strings themselves
   (uri, semantic symbol) carried by the generational workspace-state, and
   dense ordinals remain the only runtime ids. `SymbolKey(symbol, localDoc)`
   semantics unchanged.
6. **Write-through parity restored.** RawSemanticDBPath serves from the parsed
   `.semanticdb`, then runs the full-generation ingest inline (best-effort,
   clearing `needs_reindex` on success) — the pre-rewrite behavior, superseding
   the draft's bookkeeping-only variant.
7. **AOT training dropped** (`AotTrain`, `aot-train.sh`, the E9 row). The AOT
   cache trained the deleted Scala server JVM; the Rust binary has no JVM to
   train and the island boots lazily. A PC-island-only AOT cache remains a
   possible future addition.
8. **The Scala real-repo zaozi probe script retired.** The zaozi navigation
   obligations are carried by `crates/ls-jvm/tests/live_zaozi.rs` (live vtable,
   plugin loaded via a workspace `pc-plugins.json`) and the island-internal
   `ZaoziPcNavSuite`; the deleted script drove the removed Scala assembly's
   ad-hoc probe flags. The pinned, patched zaozi source stays exposed as
   `ZAOZI_SRC` (flake input) for manual real-repo validation with any LSP
   client.
9. **Fixed defect (found by the project-level editor e2e): shared-sourceroot
   target attribution pruned cross-file definitions.** Under the mill layout
   every target's `-sourceroot` is the workspace root, so
   `QueryOrchestrator::symbol_definition`'s deepest-sourceroot-prefix
   attribution of the requesting buffer tied across ALL targets and picked an
   arbitrary one — whose forward closure then pruned valid definitions
   (observed on zaozi: hover and references answered at a position where
   definition was empty). The fix attributes the buffer by its ingested doc
   row's own target first (exact), keeping the prefix heuristic only as the
   fallback for un-indexed files. Regression:
   `crates/ls-engine/tests/engine.rs` (shared-sourceroot + object-symbol
   cases) and the now-hard cross-file definition gates in `it/nvim/e2e.lua`.
10. **JNIEnv contingency retired.** The historical draft carried a two-call
   JNIEnv fallback for the island boot; the ratified decision is FFM-only with
   premain-only registration, proven on JDK 25 by the boundary spike
   (`crates/ls-jvm-spike/tests/no_jni_guard.rs` enforces the discipline).

## Historical

The v1 (Scala implementation) traceability map — its acceptance rows, E-row
table, rename-rule table, and benchmark map — described modules deleted at the
rewrite cutover and was superseded by this file plus `docs/coverage-audit.md`
(which preserves the full v1-suite → v2-test mapping, including rows for the
deleted files, as the port's evidence record).
