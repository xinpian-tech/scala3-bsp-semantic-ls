# v2 Rewrite: Rust Core + JVM PC Island

## Goal Description

Rewrite every part of `scala3-bsp-semantic-ls` except the presentation-compiler island in Rust, per the ratified decision record in `plan-rust.md` §0 (appended below as the original draft): single process, Rust main binary, lazily embedded in-process JVM connected exclusively through the Java FFM (Panama) API over a flat `#[repr(C)]` C ABI (JNI reduced to the single `JNI_CreateJavaVM` boot symbol, zero JNIEnv usage — absolute, no fallback), SQLite removed in favor of immutable mmap segments + atomic-rename manifest + generational workspace-state files, big-bang migration in the same repository. The rewrite is behavior-preserving for the observable server surface — advertised LSP capabilities, executeCommand set, bootstrap state machine, diagnostics routing semantics, dirty-buffer overlay behavior, doctor report contract, and CLI — with a user-granted trim latitude limited to `documentHighlight`, the `pcPluginStatus` command, and the no-BSP warm-restart mode (deferrable with a recorded note; see DEC-1/DEC-2). Feature semantics (consistency levels, alias groups, rename safety, §18.1 correctness matrix) are ported, not redesigned. Target platform is Linux only (DEC-5). The draft §9 numeric targets are directional, not gates; the "index-only session runs with zero JVM in the process" property is a hard assertion (DEC-8). `ls-tasty` (dependency index) is explicitly OUT of scope (v1.1 per draft §7).

## Acceptance Criteria

Following TDD philosophy, each criterion includes positive and negative tests for deterministic verification.

- AC-1: Storage core (`ls-store`) — segments, manifest, workspace-state, snapshots.
  - The postings segment format stays `docs/index-format.md` v1 (little-endian, CRC32C, same file set) extended with three new sections: `target-meta.bin`, `symbol-meta.bin`, `search.bin`. `manifest.json` (atomic tmp+fsync+rename+fsync-dir) replaces `segment_manifest`+`current.json` as the single commit point. Workspace state is GENERATIONAL with the same durability protocol as segments: each ingest writes `workspace-state-<generation>.bin.tmp`, fsyncs it, renames it into place, fsyncs the parent directory, and only THEN commits `manifest.json` (which records the paired state generation + checksum). One state generation is active at a time; boot accepts only a matching (segment, state) pair; the snapshot owns its paired state generation, prior generations are retained only while a live snapshot or recovery needs them, and the janitor deletes an old state file only after its snapshot drops. A crash between state publication and manifest commit therefore recovers the OLD manifest with the OLD state (never a mixed pairing).
  - Positive Tests (expected to PASS):
    - Segment write→mmap read round-trip preserves every record; publish/retire follows write-pipeline order (segment fsync before manifest commit before snapshot swap).
    - Superseded segments and state files are janitor-deleted only after the old snapshot drops; snapshot readers hold mmap alive across a concurrent publish (ArcSwap semantics).
    - Boot with no BSP re-publishes the manifest's active (segment, state) pair.
  - Negative Tests (expected to FAIL / be rejected):
    - CRC-corrupted file → whole segment rejected with a typed error, never partially served.
    - Torn `manifest.json` → previous generation stays active.
    - kill −9 injected around BOTH rename points (state-file rename/fsync and manifest rename/fsync) and anywhere during ingest → next boot serves the old generation with its own paired state, tmp debris removed.
    - A state file with a future schema version or a checksum/generation mismatch vs the manifest → typed refusal, not silent misread.
- AC-2: SemanticDB ingest (`ls-semanticdb`) — parse, normalize, group.
  - prost-decoded `semanticdb.proto` TextDocuments, md5 validation, normalization to the `NormalizedDocument` shape, ref-group vs rename-group building with the exact `UnsafeReason` mask semantics over the repo enum verbatim: `External`, `GeneratedOccurrence`, `ReadonlyOccurrence`, `OverrideFamily`, `SyntheticOnly`, `PcOnly`, `SharedSourceDisagreement`, `UnsupportedSymbolFamily`, `DependencySource`, `OpaqueType` (exported symbols fall under `UnsupportedSymbolFamily`).
  - Positive Tests: fixtures compiled by the pinned scalac with `-Xsemanticdb` reproduce the Scala suites' parse/normalize/group assertions (`WireDecoderSuite`, `SymbolStringsSuite`, `GroupsSuite`, `ScalacIntegrationSuite` equivalents); case-class `copy` and `derives` synthetics remain characterized as skipped-synthetics-payload only.
  - Negative Tests: md5 mismatch → document recorded stale, never served as fresh; truncated/malformed protobuf → typed per-file error, file skipped and counted, ingest continues.
- AC-3: Engines and orchestrator (`ls-engine`) — three paths, three consistency levels, §18.1.
  - Query orchestration ports IndexPath / RawSemanticDBPath / PCPath with `BestEffort`/`FreshPreferred`/`FreshRequired` routing; references implement group fan-out, reverse-dependency-closure target pruning, per-occurrence epoch filtering, dedupe, includeDeclaration; rename implements the FreshRequired ladder (compile → fresh ingest → fresh snapshot → cursor → rename group → editable postings → `RenameProfile`/`unsafeReasonMask` gate → WorkspaceEdit).
  - RawSemanticDBPath keeps write-through PARITY with today (DEC-3, decided): serve from the parsed `.semanticdb`, then run the full-generation ingest inline on the index thread (best-effort, same as the current `writeThroughRawPath`), clearing `needsReindex` on success. (Supersedes the draft §6 "bookkeeping-only" consequence.)
  - Positive Tests: the FULL §18.1 matrix ported — safe families assert exact reference span sets and exact rename edit spans; documentHighlight (if retained per DEC-1) is served from index doc-postings; shared-source primary-owner indexing preserved (first target in workspace order owns postings).
  - Negative Tests: every unsafe family asserts its exact rejection reason (external, opaque type, exported, override, synthetic-only, shared-source disagreement); a dirty cursor document degrades to `StaleIndex` (never answers from stale snapshot truth); a source in a no-SemanticDB target → hard `LsError.NoSemanticdb` (E1 parity); rename with failing compile → `LsError.CompileFailed`.
- AC-4: Workspace-symbol search (segment `search.bin` + `FuzzyRank` port).
  - Deterministic tiering replaces FTS5 bm25 (DEC-4, decided): exact > prefix > camel-hump/subsequence, hump-hit bonus, length tiebreak; candidate pull is bounded (cap ported from `MetaStore`), backed by sorted `normalized_name` + `initials` tables; exact-name membership query (`workspaceSymbolNameExists`) supported for the PC-only filter; `search.bin` indexes normalized display, owner, and package tokens, and multi-token queries (including quoted-token/punctuation handling) keep today's match semantics across those fields (`MetaStore`'s workspace-symbol query behavior is the spec, so FTS5 removal must not narrow which symbols match — only their ordering may change).
  - Positive Tests: `FuzzyRankSuite` + workspace-symbol query suites ported; ordering is stable across runs; multi-token/owner/package query behavior specified and tested.
  - Negative Tests: a query that is not a subsequence of any candidate returns empty; over-cap candidate sets stay bounded (no unbounded scan).
- AC-5: BSP client (`ls-bsp`).
  - `.bsp/*.json` discovery (candidates sorted by (name, filename), first usable wins; a malformed file yields a typed `InvalidConnectionFile` for that file without disqualifying valid siblings; `NoConnectionFile` only when no usable candidate exists), initialize/initialized handshake, workspace/buildTargets, buildTarget/sources with DIRECTORY-item expansion, scalacOptions, compile with typed outcome, inverseSources (capability-gated with local project-model fallback), dependencySources + outputPaths (capability-gated, best-effort, absorb errors to None), diagnostics params surfaced to the router, per-request timeout (default 30 s) with typed `RequestTimeout`, child stderr pumped line-wise to the log, shutdown ladder (buildShutdown → exit → terminate → destroyForcibly).
  - Positive Tests: ported fake-BSP harness suites (`BspDiscoveryTest`, `BspSessionTest`, `BspProjectModelTest`, `SemanticdbFlagsTest` equivalents) green; live mill smoke (discovery → targets → scalacOptions → compile → diagnostics) green against `it/sample-workspace`.
  - Negative Tests: missing `.bsp` dir → typed `NoConnectionFile`; request timeout → typed error and future cancelled; non-advertised capability → `None`, never a crash.
- AC-6: C ABI + island (`ls-pc-abi`, `ls-jvm`, `ls-pc-host`).
  - AC-6.1: Boot — dlopen(libjvm) + `JNI_CreateJavaVM` with JVM options including `-cp <pc-host assembly>`, `--enable-native-access=ALL-UNNAMED` (required for the island's FFM downcalls/upcalls), `-XX:+UseCompactObjectHeaders`, and `-javaagent:<pc-host.jar>=0x<vtable>`; premain builds downcalls from raw addresses, registers the PC vtable (the draft's 14 ops plus `spawn_dispatch` added in convergence — 15 total), spawns the Java-loaned dispatch + control threads; Rust rendezvous with deadline. Premain registration is the ONLY supported boot path: if the M0 spike disproves it on JDK 25, the milestone reports a blocked no-go finding and a user decision is required before any fallback is considered (no JNIEnv fallback is auto-activated; decision record #3 stands).
    - Positive: spike binary boots, echo op round-trips, cold process shows no libjvm mapping before the first PC request, island boots without native-access warnings/denials.
    - Negative: rendezvous timeout → typed boot error + island log captured in doctor; perturbed layout canary → boot refused.
  - AC-6.2: ABI encoding — every boundary struct `#[repr(C)]` with a cbindgen-generated reference header; two-sided layout verification (boot canary over sizeof+offsets, plus Java-side offset unit tests and a Rust `const` assertion set); flat encodings for all 15 op payloads and `symbol_definition` results are LOSSLESS vs today's carriers, including nullable-vs-empty distinctions (hover null, prepareRename null, empty completion list) and `DefinitionResult` per-location origin tags. Response memory: callee measures, calls `alloc` once, writes, returns; consumer frees; error paths free eagerly.
    - Positive: round-trip property tests per payload; repeated-request leak test stays flat.
    - Negative: malformed lengths/counts from a fuzzer → typed decode error, no UB; a Java upcall that throws → status error, VM alive; a Rust callback that panics → status error, process alive.
  - AC-6.3: Dispatch + recovery — single pc-dispatch loaned thread serializes PC ops (today's `InProcessPcWorker` semantics); pc-control thread serves `plugin_status`/`restart_instances`/`shutdown`/`spawn_dispatch` while dispatch is busy. Recovery is a DISPATCH-GENERATION ladder for non-cooperative wedges: on deadline the watchdog fails the LSP request, asks the island (via pc-control) to attempt the PC's own cancellation, then `restart_instances`, and — if the dispatch lane still does not return — requests a fresh loaned dispatch thread (`spawn_dispatch(generation+1)`); Rust re-points the request channel to the new generation and replays registered targets + open buffers from the Rust-side mirror; the abandoned thread is parked and its generation counted. Abandoned generations are BOUNDED (small fixed cap); exceeding the cap is island-fatal → orderly process exit (editor restarts against the crash-safe store).
    - Positive: a cooperative wedge recovers via cancel/restart_instances and a NON-COOPERATIVE wedge (injected busy-loop hook) recovers via a new dispatch generation — subsequent completion works without reopening buffers (E7-equivalent); zaozi cross-file nav suite re-pointed at the vtable passes (symbol_definition parity incl. forward-closure pruning by the requesting buffer's target).
    - Negative: a request exceeding its deadline fails typed without deadlocking dispatch; exceeding the generation cap exits the process rather than accumulating wedged threads.
  - AC-6.4: stdout protection — before `JNI_CreateJavaVM`, fd 1 is re-pointed (dup2) to stderr and the LSP writer keeps a private dup of the original stdout; premain additionally re-points `System.out`.
    - Positive: island prints (plugins, compiler) never corrupt the LSP stream.
    - Negative: a plugin that spams System.out during a request → protocol stream stays parseable.
  - AC-6.5: Island parity — `pc-plugins.json` loading (compilerPlugins jars/options + servicePluginJars, ServiceLoader SPI, self-test + disable-on-throw semantics), `PcTargetConfig` fields, `Utf16Text` conversion, LRU instance cap — unchanged in the island; PC options strip SemanticDB flags exactly as today's `Bootstrap.pcOptions`: `-Xsemanticdb`, `-Ysemanticdb`, and both the colon and two-token forms of `-semanticdb-target` and `-sourceroot`.
    - Positive: island suites (`PluginManagerSuite`, `PcQuerySuite`, `Utf16TextSuite`, `PcWorkerManagerSuite` equivalents) stay green on the retained Scala code.
    - Negative: a service plugin whose hook throws is disabled with a recorded reason; the request completes as identity.
- AC-7: LSP server (`ls-server`) — protocol surface and lifecycle parity.
  - Trim latitude (DEC-1/DEC-2, decided): `documentHighlight`, the `pcPluginStatus` command, and the no-BSP warm-restart mode MAY be deferred past v1 with a recorded deferral note in the cutover docs; if any is trimmed it must be absent from the advertised surface (never advertised-and-broken). Everything else in AC-7 is mandatory — the didSave/diagnostics/doctor flows depend on the `compile`/`reindex`/`doctor` commands.
  - AC-7.1: Capabilities — initialize returns exactly the implemented set, which by default is today's: full text sync; completion (trigger ".", resolveProvider); hover; signatureHelp (triggers "(", ","); definition; typeDefinition; references; rename (prepareProvider); documentHighlight (trimmable); workspaceSymbol; executeCommand with `scala3SemanticLs.doctor|reindex|compile` (+ `pcPluginStatus`, trimmable); semanticTokens/inlayHint deliberately absent.
    - Positive: `CapabilitiesSuite` equivalent asserts the JSON matches the implemented set exactly.
    - Negative: unknown executeCommand → today's error shape.
  - AC-7.2: Bootstrap state machine — initialize synchronous → NotReady; initialized spawns async bootstrap. Pre-ready behavior is per-method, exactly as today: references and rename → typed "workspace is not ready" error; documentHighlight → empty; prepareRename → null; workspace/symbol → empty; PC requests (completion/hover/signatureHelp/definition/typeDefinition) → empty/null fallbacks; executeCommand preserves today's pre-ready string/doctor behavior. `buildTarget/didChange` pre-ready is buffered and drained post-bootstrap; post-ready didChange refetches the model, re-registers PC targets, replays open buffers, reingests. No-BSP warm restart (trimmable per DEC-2) reaches Ready over the recovered index with PC disabled, compile/rename typed-failing, references/symbol served (E8 parity).
    - Positive: state-machine suite ports (`BuildTargetsChangeBufferingSuite`, `BootstrapRecoverySuite` equivalents) green.
    - Negative: pre-ready references/rename → the typed not-ready error, not a crash or empty lie.
  - AC-7.3: Diagnostics — BSP diagnostics merged per URI across targets with per-target reset semantics and clear-once suppression (`DiagnosticRouterSuite` parity, E2 parity).
    - Positive: multi-target union publish; per-target reset replaces only that target's list.
    - Negative: clean recompile of a never-published uri emits no spurious empty publish.
  - AC-7.4: Dirty buffers — `DocumentStore` dirty = buffer≠disk re-read per check; `PcOverlay` symbolAt via PC prepareRename-span + definition-symbol with token-span fallback; pcOnly detection (all definition origins non-Workspace); PC-only top-level decls appear in workspace/symbol with container marker `unsaved buffer (PC-only)` gated on exact-name index membership; `contributesOccurrences` stays false.
    - Positive: `PcOverlaySuite` equivalent green; type-and-undo-to-disk yields clean.
    - Negative: PC-only symbol reaching references/rename → rejected (`PcOnly` reason), never edited.
  - AC-7.5: didSave flow — debounced (default 500 ms) single-flight compile of the reverse-dependency closure + full reingest, queue-collapsed (E3 parity).
    - Positive: repeated saves collapse to one queued job; new positions served without an explicit reindex.
    - Negative: compile failure surfaces diagnostics and leaves the previous snapshot serving.
  - AC-7.6: Doctor — report keeps the section contract with `SQLite` replaced by a `Store` section (manifest/active segment/doc+symbol counts/state-file facts); Runtime section drops JVM-flag facts in favor of host facts + island status; the island status read is NON-INVASIVE (reports "island not started" without booting the JVM); offline `--doctor` works pre-bootstrap; JSON rendering keys updated accordingly; `ls dump` subcommand provides ad-hoc store inspection.
    - Positive: doctor renders all sections live and offline; `ls dump` prints manifest/state/segment-header facts.
    - Negative: doctor on a cold process leaves it JVM-free.
  - AC-7.7: CLI — `--version`, `--doctor [dir]` preserved; `--in-process-pc`/`--forked-pc` removed; stdout carries only protocol bytes.
    - Positive: CLI suite green.
    - Negative: unknown flags produce a usage error, not a silent start.
- AC-8: End-to-end, packaging, cutover.
  - AC-8.1: Fake-BSP e2e — `LsEndToEndTest` scenario set ported against a Rust-hosted fake BSP server (capabilities, diagnostics, references, rename, PC completion, dirty overlay, PC-only symbol, didChange reload, shutdown).
    - Positive: all scenarios green. Negative: E1-style no-SemanticDB target scenarios stay hard errors.
  - AC-8.2: Real-BSP e2e — E0–E8 equivalents over live mill on `it/sample-workspace` (E7 re-targeted to dispatch-generation fault injection; E8 conditional on DEC-2 retention; E9/AOT dropped with rationale recorded). Cold start: initialize+index queries succeed with zero JVM in the process (hard assertion via /proc/self/maps, per DEC-8).
    - Positive: suite green against real mill. Negative: zero-JVM assertion fails the build if libjvm maps before the first PC request.
  - AC-8.3: Packaging — nix package builds the cargo workspace (crane or naersk) + mill `ls-pc-host` assembly (Premain-Class manifest) + zaozi plugin jar offline; the binary resolves JAVA_HOME (config > env > nix-baked); `nix flake check` green including updated checks (rust toolchain, ivy-lock shrunk to island deps, package build); `LS_SQLITE_LIB` and sqlite inputs removed; flake systems narrowed to Linux (DEC-5) with macOS explicitly unsupported.
    - Positive: offline `nix build .#default` succeeds; packaged `--version` and offline `--doctor` work.
    - Negative: ivy-lock drift → `scripts/check-ivy-lock.sh` fails.
  - AC-8.4: Cutover — Scala modules listed in draft §1.2 deleted; `worker.scala`/`ForkedPcWorker`/`PcWorkerMain` deleted; `AotTrain`+`aot-train.sh` deleted; no dangling references (search sweep clean); `architecture.md`/`deployment.md`/`index-format.md` addendum/`plan.md` supersession updated; any DEC-1/DEC-2 trims recorded as deferral notes.
    - Positive: full build+test green after deletion. Negative: a lingering reference to a deleted module fails the sweep.

## Path Boundaries

Path boundaries define the acceptable range of implementation quality and choices.

### Upper Bound (Maximum Acceptable Scope)
Everything in AC-1..AC-8 with the full observable surface retained (no DEC-1/DEC-2 trims exercised), plus: ported bench harness (criterion ingest+query benches with a smoke gate), ABI fuzzing beyond property tests, `ls dump` with per-section detail, doctor JSON schema documentation. No ls-tasty, no delta segments, no file watcher, no island AOT cache, no semanticTokens/inlayHint, no PC-diagnostics LSP surface, no macOS support work, no new LSP features beyond parity.

### Lower Bound (Minimum Acceptable Scope)
All AC-1..AC-8 green with: the DEC-1/DEC-2 trim latitude exercised (documentHighlight, `pcPluginStatus` command, and/or no-BSP warm restart deferred with recorded notes and removed from the advertised surface); bench reduced to an ingest smoke benchmark; ABI fuzz coverage limited to decode-boundary property tests; `ls dump` limited to manifest+state+segment header dumps. All other AC-7 parity items are NOT trimmable.

### Allowed Choices
- Can use: hand-rolled or `lsp-server`-style synchronous JSON-RPC over threads+channels for LSP and BSP; `prost` or `protobuf` codegen for SemanticDB; any maintained zip/memmap/CRC crates (`memmap2`, `arc-swap`, `crossbeam` expected); crane or naersk for nix; test frameworks of choice (libtest + insta acceptable).
- Cannot use: SQLite in any form; the Rust `jni` crate or any JNIEnv call — absolute, no boot contingency (if premain-only boot fails in M0, stop with a blocked no-go finding requiring a fresh user decision); JSON anywhere on the PC C ABI; a forked child JVM for the PC; an async runtime for the core request path; Bloom filters / source grep / PC-derived persistent truth (architecture.md forbidden list carries over); macOS-specific support work (Linux only per DEC-5).
- Fixed per decision record (no choice): the eight §0 decisions of plan-rust.md, including flat `#[repr(C)]` boundary structs and big-bang migration.

> **Note on Deterministic Designs**: the draft fixes topology, interop, encoding, migration strategy, storage, and platform; the boundaries above are intentionally narrow on those axes, and latitude exists only where marked (crate choices, trim latitude, bench/fuzz depth).

## Feasibility Hints and Suggestions

> **Note**: This section is for reference and understanding only. These are conceptual suggestions, not prescriptive requirements.

### Conceptual Approach
Milestone 0 de-risks the only novel-physics item (embedded-JVM boundary) with a standalone spike before storage work begins. The port then proceeds bottom-up along the crate dependency order, each crate carrying its Scala test suite's ported equivalent as the gate. The Scala tree stays in place (read-only reference) until AC-8, so every ported suite can be diffed against its source suite; deletion is the last step.

Boundary response protocol: Java measures the payload (single pass over the carrier), calls `alloc(size)` once, writes header+records+string blob, returns the pointer via out-param; Rust decodes zero-copy and `free`s. Rust→Java requests are the mirror image with caller-owned arenas. All 15 op encodings live in one `ls-pc-abi` crate consumed by both cbindgen (header) and the Java layout mirror.

### Relevant References
- `docs/index-format.md`, `docs/architecture.md`, `docs/plugin-spi.md` — normative contracts (format bytes, semantics, island SPI).
- `modules/ls-core/src/ls/core/ScalaLs.scala`, `WorkspaceState.scala`, `DiagnosticRouter.scala`, `DocumentStore.scala`, `PcOverlay.scala`, `IndexPcDefinitionResolver.scala`, `Uris.scala`, `LspConvert.scala`, `DoctorCommand.scala` — LSP surface + lifecycle + overlay semantics to port.
- `modules/ls-rename/src/ls/rename/` (engines, `ingest/IngestPipeline.scala`, `ingest/WorkspaceTargets.scala`) — pipeline and engine spec.
- `modules/ls-postings/src/` — segment writer/reader, snapshot manager, janitor.
- `modules/ls-sqlite-ffm/src/ls/sqlite/{Schema,MetaStore,FuzzyRank}.scala` — the state/search semantics being redistributed.
- `modules/ls-bsp/src/ls/bsp/{discovery,session,loader}.scala` — BSP client behavior incl. gating and timeouts.
- `modules/ls-pc/src/ls/pc/` — island (kept) + carriers being replaced by the ABI.
- `modules/ls-core/test/src/ls/core/{E2eSupport,RealBspFixture}.scala`, `it/sample-workspace`, `scripts/it-real-bsp.sh` — e2e harnesses to port/re-point.
- `flake.nix`, `nix/{package,dev-shell,checks}.nix`, `scripts/check-ivy-lock.sh` — packaging to extend.

## Dependencies and Sequence

### Milestones
1. M0 Boundary viability spike: standalone crate boots an embedded JVM via the boot protocol, premain registration, echo op on loaned threads, Throwable/panic containment, timeout injection. M0 validates the premain-only boot path; a failure is a blocked no-go finding requiring a fresh user decision, not an internal fallback choice.
2. M1 Foundations: cargo workspace scaffold + CI + flake rust toolchain; `ls-index-model`.
3. M2 Storage: `ls-store` (segments + new sections, manifest, generational workspace-state, snapshots, janitor, recovery matrix, search + FuzzyRank).
4. M3 Semantics: `ls-semanticdb`; `ls-engine` + §18.1 port; `ls-doctor` core.
5. M4 Protocol: `ls-bsp` + fake harness + live mill smoke.
6. M5 Boundary: `ls-pc-abi` structs + canary; `ls-jvm` (boot, dispatch generations, watchdog); `ls-pc-host` (premain, layouts, upcalls); island cleanup; zaozi nav e2e.
7. M6 Assembly: `ls-server` (LSP wiring, state machine, diagnostics, commands, doctor, CLI); fake-BSP e2e; real-BSP e2e.
8. M7 Cutover: packaging, flake checks, deletion sweep, docs v2.

M0 gates M5 design; M1→M2→M3 are strict; M4 depends only on M1; M5 depends on M0+M1 (and M3 for symbol_definition e2e); M6 depends on M2–M5; M7 last.

## Task Breakdown

Each task must include exactly one routing tag:
- `coding`: implemented by Claude
- `analyze`: executed via Codex (`/humanize:ask-codex`)

| Task ID | Description | Target AC | Tag (`coding`/`analyze`) | Depends On |
|---------|-------------|-----------|----------------------------|------------|
| task1 | M0 spike: embedded-JVM boot + premain registration + echo + containment + timeout injection; on failure report a blocked no-go finding for user decision (no fallback auto-activated) | AC-6.1 | coding | - |
| task2 | Cargo workspace scaffold, CI (fmt/clippy/test), flake rust toolchain + crane wiring | AC-8.3 | coding | - |
| task3 | `ls-index-model`: ids/span/flags/roles/errors + property tests | AC-1 | coding | task2 |
| task4 | `ls-store` segments: v1 reader/writer + target-meta/symbol-meta sections + CRC | AC-1 | coding | task3 |
| task5 | `ls-store` manifest + generational workspace-state + snapshot lifecycle + janitor + recovery matrix tests | AC-1 | coding | task4 |
| task6 | `ls-store` search section + FuzzyRank port + membership query | AC-4 | coding | task4 |
| task7 | `ls-semanticdb`: prost decode + normalize + groups + scalac fixtures | AC-2 | coding | task3 |
| task8 | `ls-engine`: orchestrator (3 paths/levels) + ingest pipeline + write-through parity | AC-3 | coding | task5, task7 |
| task9 | §18.1 matrix port (references/rename/highlight suites) | AC-3 | coding | task8 |
| task10 | `ls-bsp`: discovery/session/model + fake harness port | AC-5 | coding | task3 |
| task11 | Live mill BSP smoke on it/sample-workspace | AC-5 | coding | task10 |
| task12 | `ls-pc-abi`: all boundary structs + cbindgen header + canary + encode/decode property tests | AC-6.2 | coding | task1, task3 |
| task13 | `ls-jvm`: dlopen/boot/rendezvous, dispatch-generation channel, watchdog, stdout fd guard | AC-6.1, AC-6.3, AC-6.4 | coding | task12 |
| task14 | `ls-pc-host` (Java): premain, layouts mirror + offset tests, upcall stubs, System.out guard; delete worker-protocol usage from island wiring | AC-6.2, AC-6.4, AC-6.5 | coding | task12 |
| task15 | Boundary integration: all 15 vtable ops end-to-end, restart/replay + dispatch-generation fault injection, symbol_definition + zaozi nav e2e | AC-6.3, AC-6.5 | coding | task13, task14, task8 |
| task16 | `ls-server`: LSP wiring, bootstrap state machine, diagnostics router, executeCommand, doctor + `ls dump`, CLI | AC-7 | coding | task8, task10, task15 |
| task17 | Fake-BSP e2e port (LsEndToEndTest scenarios) | AC-8.1 | coding | task15, task16 |
| task18 | Real-BSP e2e port (E0–E8 equivalents, cold-start zero-JVM assertion) | AC-8.2 | coding | task17, task11 |
| task19 | Adversarial review of the C ABI (ownership, containment, layout drift, fuzz gaps) | AC-6.2 | analyze | task12, task13, task14 |
| task20 | Coverage audit: ported suites vs Scala suite inventory + §18.1 + E0–E8; list gaps | AC-3, AC-8 | analyze | task9, task17 |
| task21 | Packaging: nix package (rust+jar), checks update, ivy-lock shrink, Linux-only systems | AC-8.3 | coding | task14, task16 |
| task22 | Cutover: deletion sweep, docs v2, deferral notes for any DEC-1/DEC-2 trims, bench smoke port | AC-8.4 | coding | task18, task20, task21 |

## Claude-Codex Deliberation

### Codex First-Pass Findings (analysis v1)
- CORE_RISKS (top-ranked): the embedded-JVM boundary is novel and on the critical path (→ moved to M0); the observable server surface is far wider than "three capabilities + PC six ops" (→ AC-7); SQLite removal removes a full state-store contract, not just persistence (→ AC-1 generational state); big-bang makes fixture coverage existential (→ AC-8 + task20 audit); the flat C ABI for LSP-shaped payloads needs lossless encodings + fuzz/round-trip tests (→ AC-6.2); single-process JVM changes the failure model (→ AC-6.3); deterministic search is a behavior change to pin explicitly (→ AC-4, DEC-4); TASTy v1.1 dependencies leak into BSP/storage design (→ dependencySources/outputPaths kept best-effort in AC-5).
- MISSING_REQUIREMENTS surfaced and absorbed: exact capability list, executeCommand names, bootstrap per-method semantics, no-BSP warm restart, dirty-overlay rules, diagnostics routing, didChange buffering, CLI/stdout protection, UTF-16 conversion, doctor section contract.
- QUESTIONS_FOR_USER became DEC-1..DEC-7 (plus Claude's DEC-8 for metrics); all now decided or convergence-resolved (see below).

### Agreements
- Boundary spike ahead of storage work (M0/task1).
- Full observable-surface parity as the compatibility contract (AC-7), with the user's trim latitude bounded to three named items.
- Typed/versioned/checksummed generational workspace-state (AC-1).
- Two-sided ABI layout verification + lossless nullable/empty encodings + leak/fuzz tests (AC-6.2).
- BSP edge semantics (gating, inverseSources fallback, timeouts, stderr) as explicit requirements (AC-5).
- Porting the Scala suite inventory + §18.1 + E0–E8 as the acceptance gates.

### Resolved Disagreements
- Write-through (round 1): draft §6 accepted a bookkeeping-only regression; investigation showed current write-through IS a full-generation inline ingest, so parity is free. Plan restores parity (AC-3); user ratified (DEC-3).
- JNIEnv boot contingency (rounds 1–2): Codex held the draft §3 contingency contradicts fixed decision #3 (FFM-only); Claude accepted — no auto-activated fallback; an M0 failure is a blocked finding requiring a fresh user decision. The contingency text remains only in the appended draft as historical context.
- Workspace-state crash pairing (rounds 1–2): Codex showed crash windows could pair mismatched manifest/state; Claude adopted generational state files with the segment durability protocol, paired by generation+checksum in the manifest (AC-1).
- Pre-ready behavior (round 1): blanket "typed not-ready" was wrong; per-method behavior adopted verbatim from `ScalaLs` (AC-7.2).
- Non-cooperative dispatch wedges (round 2): `restart_instances` alone cannot free a busy dispatch lane; dispatch-generation ladder adopted (cancel → restart_instances → `spawn_dispatch(gen+1)` + replay, bounded generations, cap-exceeded → orderly exit) with a busy-loop fault test (AC-6.3); `spawn_dispatch` counted as the 15th vtable op.
- Also adopted from rounds 1–2: native-access boot flag, task-graph deps (task16→task15, task21→task14), exact `UnsafeReason` enum naming, BSP discovery wording, `pcOptions` stripping forms, multi-token search semantics, doctor non-invasive island status, search token fields.

### Convergence Status
- Final Status: `converged` (round 3: no required changes, no high-impact disagreements; rounds executed: first-pass analysis + 3 convergence reviews)

## Pending User Decisions

- DEC-1: Preserve documentHighlight + all four executeCommand commands in v1?
  - Claude Position: preserve all (advertised + e2e-tested today).
  - Codex Position: make them explicit v1 compatibility features or trim deliberately.
  - Tradeoff Summary: full parity vs rewrite scope.
  - Decision Status: Decided 2026-07-05 — trim latitude granted: `documentHighlight` and the `pcPluginStatus` command may be deferred with recorded notes and removed from the advertised surface; `doctor`/`reindex`/`compile` commands stay mandatory.
- DEC-2: Is the no-BSP warm-restart mode normative in v1?
  - Claude Position: yes, port it (E8 pins it).
  - Codex Position: open question.
  - Tradeoff Summary: recovery UX vs extra bootstrap path.
  - Decision Status: Decided 2026-07-05 — trimmable under the same latitude as DEC-1 (deferrable with a recorded note); if kept, E8 parity applies.
- DEC-3: Restore RawSemanticDBPath write-through parity (serve + inline full ingest) instead of the draft's bookkeeping-only variant?
  - Claude Position: yes — parity is free (current write-through is already a full-generation ingest).
  - Codex Position: agreed after code inspection.
  - Tradeoff Summary: none (behavior preserved, no SQLite needed).
  - Decision Status: Decided 2026-07-05 — restore parity (AC-3).
- DEC-4: Accept deterministic search ranking as v2 semantics (replacing FTS5 bm25 ordering; match set unchanged)?
  - Claude Position: accept — FuzzyRank is already the authoritative ranker.
  - Codex Position: declare it as intentional v2 semantics with explicit ordering tests.
  - Tradeoff Summary: determinism + simplicity vs exact bm25 mimicry.
  - Decision Status: Decided 2026-07-05 — accepted (AC-4).
- DEC-5: Platform envelope for embedded libjvm?
  - Claude Position: Linux first, macOS compiling-but-ungated.
  - Codex Position: asked for an explicit envelope.
  - Tradeoff Summary: CI cost + dyld/proc semantics vs reach.
  - Decision Status: Decided 2026-07-05 — Linux only; flake systems narrowed; macOS explicitly unsupported (AC-8.3).
- DEC-6: Doctor report freedom vs fixed contract?
  - Claude Position: keep section order/keys, swap `SQLite`→`Store`, allow wording drift.
  - Codex Position: raised as open question; did not dispute the commitment in later rounds.
  - Tradeoff Summary: operator familiarity vs redesign freedom.
  - Decision Status: Resolved in convergence — keep the section contract with the Store swap (AC-7.6).
- DEC-7: Include `PcFacade.diagnostics` in the v1 ABI?
  - Claude Position: no — it is not on today's LSP surface; keep island-internal.
  - Codex Position: raised as open question; did not dispute in later rounds.
  - Tradeoff Summary: ABI surface minimalism vs future plugin-diagnostics parity.
  - Decision Status: Resolved in convergence — island-internal, not in the v1 ABI.
- DEC-8: Are the draft §9 targets (<10 ms initialize, <50 MB idle RSS, sub-second island boot, ≥ hot-path parity) hard requirements?
  - Claude Position: directional, with the zero-JVM cold-start property as a hard assertion.
  - Codex Position: N/A - open question.
  - Tradeoff Summary: CI benchmark gates are environment-sensitive and flaky; the qualitative zero-JVM property is cheaply assertable.
  - Decision Status: Decided 2026-07-05 — directional; zero-JVM cold start stays a hard assertion (AC-8.2).

## Implementation Notes

### Code Style Requirements
- Implementation code and comments must NOT contain plan-specific terminology such as "AC-", "Milestone", "Step", "Phase", "task N", "DEC-", or similar workflow markers
- These terms are for plan documentation only, not for the resulting codebase
- Use descriptive, domain-appropriate naming in code instead; ported tests may cite the originating Scala suite name in test names for traceability (e.g. a `diagnostic_router` test module mirroring `DiagnosticRouterSuite` cases) but not plan markers

## Output File Convention

This template is used to produce the main output file (e.g., `plan.md`).

### Translated Language Variant

When `alternative_plan_language` resolves to a supported language name through merged config loading, a translated variant of the output file is also written after the main file. Humanize loads config from merged layers in this order: default config, optional user config, then optional project config; `alternative_plan_language` may be set at any of those layers. The variant filename is constructed by inserting `_<code>` (the ISO 639-1 code from the built-in mapping table) immediately before the file extension:

- `plan.md` becomes `plan_<code>.md` (e.g. `plan_zh.md` for Chinese, `plan_ko.md` for Korean)
- `docs/my-plan.md` becomes `docs/my-plan_<code>.md`
- `output` (no extension) becomes `output_<code>`

The translated variant file contains a full translation of the main plan file's current content in the configured language. All identifiers (`AC-*`, task IDs, file paths, API names, command flags) remain unchanged, as they are language-neutral.

When `alternative_plan_language` is empty, absent, set to `"English"`, or set to an unsupported language, no translated variant is written. Humanize does not auto-create `.humanize/config.json` when no project config file is present.

--- Original Design Draft Start ---

# plan-rust.md — v2: Rust core + JVM PC island

> Normative plan of record for the v2 rewrite. Supersedes the topology and
> toolchain constraints of `plan.md` §1.1 and `docs/architecture.md` §2/§3;
> feature semantics (consistency levels, groups, rename safety, §18.1
> correctness matrix) remain normative as written in `docs/architecture.md`
> §4–§9/§13 and are ported, not redesigned. The on-disk postings format stays
> `docs/index-format.md` v1, extended per §6 below.

## 0. Decision record (2026-07-05)

| # | Decision | Choice |
|---|----------|--------|
| 1 | Scope | Everything except the presentation-compiler island is rewritten in Rust. JVM keeps: `ls-pc` facade/instances/plugin SPI, `ls-zaozi-pcplugin`, new `ls-pc-host` (Panama boundary). |
| 2 | Topology | Single process. Rust is the main process; the JVM is embedded lazily, in-process. **No forked PC backend** — the JSON-RPC worker protocol (`PcWorkerApi`, `ForkedPcWorker`, `PcWorkerMain`, `worker.scala` carriers) is deleted, not ported. |
| 3 | Interop | FFM (Panama) only. No JNIEnv usage, no `jni.h`, no Rust `jni` crate. The JNI Invocation API is reduced to **one boot symbol** (`JNI_CreateJavaVM` via `dlopen`), which has no FFM replacement as of JDK 25. |
| 4 | Encoding | Flat `#[repr(C)]` structs both directions from day one. No JSON on the boundary. |
| 5 | Migration | Big bang. The Scala implementation is not kept running as an oracle; the §18.1 fixture matrix is ported as the test spec. |
| 6 | Repo | Same repository: `crates/` (cargo workspace) next to `modules/` (mill, JVM island only). |
| 7 | SQLite | **Removed.** Single storage idiom: immutable mmap segments + atomic-rename manifest + one small workspace-state file. FTS5 is replaced by a segment-resident search section + a Rust port of `FuzzyRank`. |
| 8 | TASTy | Read by **Rust ingest only** (`ls-tasty`, v1.1): a navigation-grade scanner feeding a readonly **dependency index** (separate namespace, mmap, content-hash cached per jar). The island never parses TASTy for the LS (the PC keeps consuming it internally via dotty — invisible to this plan). Core capabilities stay SemanticDB-only. The version envelope is closed by scalac itself (it refuses TASTy newer than the pinned compiler), so the scanner supports exactly `[28.0 .. 28.<pinned minor>]`. Not big-bang scope. |

## 1. Target architecture

```
editor ⇅ LSP (stdio)
┌───────────────────────────────────────────────────────────────┐
│ ls (Rust binary)                                              │
│  lsp loop · bsp client · semanticdb ingest · segments/store   │
│  references/rename engines · orchestrator · doctor            │
│                                                               │
│  first PC request:                                            │
│    dlopen(libjvm) → JNI_CreateJavaVM(-javaagent:pc-host.jar)  │
│  ┌─────────────── JVM island (same process) ────────────────┐ │
│  │ ls-pc-host (FFM layouts, upcall stubs, downcall handles) │ │
│  │ ls-pc (PcFacade / PcInstance / plugin SPI)               │ │
│  │ scala3-presentation-compiler + zaozi pc-plugin           │ │
│  └───────────────────────────────────────────────────────────┘ │
│  boundary: C function-pointer vtables, both directions        │
└───────────────────────────────────────────────────────────────┘
```

### 1.1 Crate map

| Crate | Contents | Replaces |
|---|---|---|
| `ls-index-model` | opaque-newtype ordinals, `Span`/`Pos` packing, `OccFlags`, `Role`, `UnsafeReason`, `LsError` | `modules/ls-index-model` |
| `ls-semanticdb` | prost-generated `semanticdb.proto` parse, normalization (`NormalizedDocument`), ref/rename group builder | `modules/ls-semanticdb` |
| `ls-store` | segment reader/writer (`index-format.md` v1 + §6 sections), `memmap2` snapshots behind `arc_swap::ArcSwap<Arc<Snapshot>>`, manifest file, workspace-state file, symbol search (`FuzzyRank` port) | `modules/ls-postings` + `modules/ls-sqlite-ffm` |
| `ls-bsp` | `.bsp/*.json` discovery, JSON-RPC client, target graph (`reverseDependencyClosure` / forward closure), diagnostics forwarding | `modules/ls-bsp` |
| `ls-engine` | ingest pipeline, references + rename engines, query orchestrator (IndexPath / RawSemanticDBPath / PCPath, three consistency levels) | `modules/ls-rename`, orchestration half of `ls-core` |
| `ls-doctor` | doctor report | `modules/ls-doctor` |
| `ls-pc-abi` | every cross-boundary `#[repr(C)]` type + `cbindgen` header (the single contract source) | `worker.scala` carriers |
| `ls-jvm` | libjvm dlopen, single-symbol boot, vtable registry, request channel to the loaned dispatcher threads, watchdog | `ForkedPcWorker` / `PcWorkerMain` |
| `ls-server` | `lsp-server` main loop, LSP wiring, CLI (`--version`, `--doctor`), `main` | LSP half of `ls-core` |
| `ls-bench` | criterion/divan ingest + query benchmarks | `modules/ls-bench` |
| `ls-tasty` *(v1.1, not big-bang)* | navigation-grade TASTy reader + dependency index ingest (§7) | — (new capability) |

JVM island (mill keeps building): `ls-pc` (minus the worker protocol files),
`ls-pc-host` (new), `ls-zaozi-pcplugin` (untouched). `docs/plugin-spi.md`
remains normative for the island.

### 1.2 Deleted at completion

Scala modules `ls-core`, `ls-rename`, `ls-sqlite-ffm`, `ls-postings`,
`ls-bsp`, `ls-semanticdb`, `ls-index-model`, `ls-doctor`, `ls-bench`;
`ls-pc/{worker.scala, ForkedPcWorker.scala, PcWorkerMain.scala}`;
`AotTrain.scala` + `scripts/aot-train.sh` (core AOT machinery — the island may
re-grow an optional PC-only AOT cache later); lsp4j/bsp4j and their
workarounds (`-Xmixin-force-forwarders:false`, gson pin, `@JsonDelegate`
split).

## 2. Hard constraints v2 (supersedes architecture.md §2 runtime block)

```text
Rust stable (pinned via flake) for the host process
Java 25 only, for the PC island only
Scala 3 only, exactly pinned (island + workspace support)
BSP only
Nix flake controlled toolchain (cargo workspace + mill, one flake)
No JNIEnv usage; JNI = the single boot symbol
No SQLite; one storage idiom (immutable segments + atomic rename)
```

Unchanged: SemanticDB is the only global semantic truth (hard
`LsError.NoSemanticdb`, no fallback); PC never writes the persistent index;
the forbidden-approximation list of architecture.md §2.

## 3. Boot protocol (zero-JNIEnv)

1. First PC-feature request → resolve `JAVA_HOME` (config > env > nix-baked
   default) → `dlopen($JAVA_HOME/lib/server/libjvm.so)`.
2. Call `JNI_CreateJavaVM` — the only JNI artifact. Its arg structs
   (`JavaVMInitArgs`, `JavaVMOption`) are hand-declared `#[repr(C)]` in
   `ls-jvm` (3 structs, no `jni.h`, no bindgen). Options:
   `-cp <pc-host assembly>`, `--enable-native-access=ALL-UNNAMED`,
   `-XX:+UseCompactObjectHeaders`,
   `-javaagent:<pc-host assembly>=0x<rust_vtable_addr>`.
3. `PcHost.premain` fires inside `JNI_CreateJavaVM` (JVMTI VMInit → java
   agent premain; no main class exists or is needed):
   reads the Rust vtable address from the agent args, builds FFM downcall
   handles from raw addresses (`MemorySegment.ofAddress` + `Linker`), builds
   the PC vtable as upcall stubs in `Arena.global()`, downcalls
   `register_pc_vtable`, then spawns the **loaned dispatcher threads** (§5).
4. Rust blocks on a rendezvous (condvar, deadline) until `register_pc_vtable`
   lands; on timeout the request fails with a typed boot error and the doctor
   reports the captured island log. The `JNIEnv*`/`JavaVM*` returned by
   `JNI_CreateJavaVM` are ignored; teardown is process exit (the island lives
   as long as the process — `DestroyJavaVM` is never called and re-creation is
   impossible anyway).

Contingency (phase-5 spike, first task): if premain-inside-CreateJavaVM
timing does not hold on JDK 25, fall back to two quarantined JNIEnv calls
(`FindClass` + `CallStaticVoidMethod`) in `ls-jvm`; nothing else changes.

## 4. C-ABI contract (`ls-pc-abi`)

Conventions:

* Every boundary type is `#[repr(C)]`, mirrored in Java as hand-written
  `MemoryLayout`s (the `ls-sqlite-ffm` skill, one-tenth the surface).
* Strings: `LsStr { ptr: *const u8, len: u32 }`, UTF-8, no NUL. Lists:
  `{ ptr, count }` of fixed-width records; variable payloads use
  `header + records + string blob (offset,len)` — the same idiom as the
  segment files.
* Memory sovereignty is single-sided: **all cross-boundary allocation is
  Rust's** (`alloc`/`free` in the Rust vtable). Request memory is caller-owned
  and valid only for the call; responses are written by the callee into
  `alloc`-obtained buffers and freed by the consumer via `free`. Java never
  returns pointers to Java-managed memory.
* Every function returns `i32` status (0 = ok; nonzero indexes a shared error
  enum, optional detail string in the response buffer). Every Java upcall body
  is wrapped `catch Throwable` (an escaping exception through an upcall stub
  aborts the VM); every Rust export is wrapped `catch_unwind`.
* Versioning: `abi_version: u32` checked at registration, plus a **layout
  canary**: both sides compute a checksum over `(sizeof, field offsets)` of
  every ABI struct at bootstrap; mismatch is a hard boot error. `cbindgen`
  regenerates the reference header in CI; drift fails the build.

### 4.1 Rust vtable (passed to premain)

```
abi_version, layout_canary
alloc(size) -> ptr            free(ptr)
log(level, LsStr)
register_pc_vtable(pc_vtable*) -> status
pc_dispatch_loop(worker_index) -> !   // entered by Java-loaned threads
symbol_definition(LsStr symbol, LsStr from_uri, *out LocationList) -> status
```

`symbol_definition` is the index-backed cross-file go-to callback (the
`PcDefinitionResolver` seam). Its implementation receives a `SnapshotReader`
handle only — `ArcSwap` load of the immutable snapshot, which after §6 also
carries target `sourceroot`/`bsp_id` — so the entire FFI-visible read surface
is one immutable object; the index writer is unreachable from island threads
by construction.

### 4.2 PC vtable (14 ops, upcall stubs)

`register_target, did_open, did_change, did_close, completion,
completion_resolve, hover, signature_help, definition, type_definition,
prepare_rename, plugin_status, restart_instances, shutdown`

Semantics are 1:1 with today's `PcBackend`/`PcWorkerApi` operations;
`restart_instances` replaces `restartWorker` (facade-level
`PcInstance` shutdown+recreate — the only recovery tier that exists without a
forked worker). `bufferText` / `activeTargets` / `registeredTargets` stay a
Rust-side mirror exactly as `ForkedPcBackend` mirrors them today; they never
cross the boundary.

Response payloads (completion lists, hover markup, signature help, definition
locations + origins, plugin status report) are flat-struct encodings of the
current carrier classes; `DefinitionResult` keeps its `origin` tag per
location.

## 5. Threading model

```
Rust threads:  lsp main loop · single index writer · scheduler/watchdog
Java-loaned:   pc-dispatch (1) + pc-control (1)
```

* At registration the island spawns two platform threads that immediately
  downcall into `pc_dispatch_loop` and never return: **Java loans threads to
  Rust**. The dispatch loop pops requests from an mpsc channel and invokes the
  PC vtable directly — the calling thread was born attached, so upcalls carry
  no implicit attach/detach cost and PC thread-locals/classloader context have
  a stable host. (This is the FFM-native replacement for
  `AttachCurrentThread`.)
* pc-dispatch is single-threaded → PC requests serialize exactly as today's
  `InProcessPcWorker` executor.
* pc-control executes `restart_instances`/`shutdown`/`plugin_status` so a
  wedged compiler can be recovered while dispatch is stuck; the Rust watchdog
  enforces per-request deadlines (the `ForkedPcWorker.orTimeout` semantics),
  fails the LSP request, and escalates: deadline → cancel →
  `restart_instances` → (JVM hard-crash only) process death, which the editor
  restarts against crash-safe on-disk state.
* Island→index rules are unchanged in spirit (`IndexPcDefinitionResolver`
  doc): callbacks read the immutable snapshot only, never block on the index
  writer or the pc queue — now enforced by the `SnapshotReader`-only handle
  rather than by comment discipline.

## 6. Storage v2 (no SQLite)

`docs/index-format.md` v1 stays normative for the existing files. The schema
that lived in `meta.sqlite` is redistributed:

| Was (schema v2) | Becomes |
|---|---|
| `segment_manifest` + `snapshots/current.json` | one `manifest.json`, atomic-rename protocol (write tmp, fsync, rename, fsync dir) — the single commit point |
| `targets` | new `target-meta.bin` segment section (bsp_id, scala_version, sourceroot, semanticdb_root, hashes) — snapshot-resident |
| `documents` (uri, md5, epoch, mtime, generated/readonly) | segment doc-index already carries uri+epoch; the **cross-generation** residue (uri → epoch counter, md5, mtime, flags) moves to `workspace-state.bin`, a few-MB flat file atomically rewritten per ingest |
| `symbol_intern`, `symbol_to_{ref,rename}_group` | retired: segments already carry the sorted symbol dictionary and group indexes per generation |
| `symbol_metadata` | new `symbol-meta.bin` segment section (display/owner/package, kind, properties, definition span) |
| `workspace_symbols_fts` (FTS5) + `workspace_symbol_fuzzy` | new `search.bin` segment section: rows sorted by `normalized_name` + a parallel `initials` table; prefix tier = binary-search range scan; ranking = Rust port of `FuzzyRank` (exact > prefix > hump-hits − length) replacing bm25 with a deterministic tiering |

Consequences, accepted:

* **ID contract change (supersedes architecture.md §6.1):** stable numeric ids
  (`SymbolId`/`DocId`/`TargetId`) existed to live in SQLite; the stable keys
  are now the strings themselves (uri, semantic symbol) via
  `workspace-state.bin`, and dense ordinals remain the only runtime ids.
  `SymbolKey(symbol, localDoc)` semantics unchanged.
* **Write-through becomes bookkeeping-only:** RawSemanticDBPath serves the
  request from the parsed `.semanticdb`, updates `workspace-state.bin`
  md5/epoch, keeps an in-memory raw-doc cache, and converges at the next full
  reindex (v1 is full-rescan anyway). Per-document persistence returns with
  v2 delta segments, which the epoch/layering machinery in the format already
  reserves.
* Search and postings are now the same generation — `workspace/symbol` and
  `references` can no longer disagree about the world.
* Recovery matrix (replaces WAL): torn `manifest.json` (rename is atomic →
  old manifest wins), manifest → missing/corrupt segment (reject, heal on next
  ingest — keeps the `BootstrapRecoverySuite` case), torn
  `workspace-state.bin` (atomic rename; stale state only forces re-ingest).
  A `ls dump` doctor subcommand replaces `sqlite3` ad-hoc inspection.

## 7. TASTy policy (v1.1: Rust-side dependency index)

* The three global capabilities consume SemanticDB only — unchanged. The
  dependency index below is a **separate readonly namespace**: `references`
  never scans it, `rename` still rejects `UnsafeReason.External`, and
  `workspace/symbol` may include it only behind an explicit toggle.
* The PC keeps consuming TASTy natively inside the island for compilation
  (dependency symbol types, `inline` bodies) — dotty-internal, invisible here.
  The island parses no TASTy on behalf of the LS.
* **`ls-tasty` (Rust, v1.1, ~3–4.5k lines + tests)** — a navigation-grade
  reader: header + version gate, name table, structural tree walk (the tag
  category ranges make skipping generic; only
  PACKAGE/TYPEDEF/DEFDEF/VALDEF/TEMPLATE + modifier tags are inspected, with
  owner chain + declaration-order overload counters), positions section,
  SemanticDB symbol-string synthesis. Explicitly NOT a semantic reader: no
  types, no cross-file linking, no signature reconstruction (tasty-query
  territory, unneeded). `TastyFormat.scala` of the pinned compiler is the
  spec.
* TASTy is an **ingest source**, not an on-demand parse: ingest scans each
  target's dependency classpath (jars are immutable → content-hash keyed
  cache, one scan per dependency set, parallel) and writes a mmap
  **dep-index**: semanticdb-symbol → (path inside `-sources.jar`, offset
  span, kind, display name). Request time is an index hit:
  - external go-to-definition: `symbol_definition` misses the workspace
    index → dep-index lookup → extract the source file from the BSP
    `dependencySources` jar into `.scala3-bsp-semantic-ls/dep-sources/`
    (readonly), convert offset → line/char against the extracted content —
    works with the JVM cold;
  - auto-import (`SymbolSearch.search` callback) and dependency-aware
    `workspace/symbol` read the same index (post-v1.1 toggles).
  This is the house principle — precompute exact truth into mmap-friendly
  structures — applied to dependencies.
* Version discipline: TASTy minor tracks the Scala minor and **scalac itself
  refuses dependencies with newer TASTy than the pinned compiler**, so the
  supported envelope is exactly `[28.0 .. 28.<pinned>]` and reading older
  minors is the compatible direction. Bumping `Deps.scalaVer` requires a
  `TastyFormat.scala` diff review + golden-corpus regeneration (CI gate).
  Experimental TASTy versions and unreadable files are skipped with typed
  errors and counted by the doctor (`dependency index: N jars, M files
  skipped`) — never guessed at.
* Correctness oracle: fixtures compile workspace code that REFERENCES library
  symbols with `-Xsemanticdb`; each occurrence's symbol string must hit the
  dep-index key byte-for-byte and the position must land on the matching
  token in the extracted source. Overload disambiguation follows the
  SemanticDB declaration-order rules (TASTy Template statement order
  preserves declaration order); where synthesis is ambiguous the lookup
  returns all candidates rather than guessing.
* Known limits (doctor-visible, not errors): Scala 2.13 / Java dependencies
  carry no TASTy (no entries); a missing `-sources.jar` yields no location in
  v1.1 (TASTy-rendered readonly stubs are a v1.2 option).
* v1 (big-bang) behavior is unchanged: external definition returns empty and
  the doctor prints `dependency navigation: not available`.

## 8. Phases and acceptance criteria

| # | Deliverable | Accepted when |
|---|---|---|
| 1 | `ls-index-model` + `ls-store` (segments, manifest, workspace-state, search section, `FuzzyRank` port) | format round-trip + CRC property tests; recovery matrix tests (torn manifest/state, missing segment, kill −9 during ingest); search tier tests |
| 2 | `ls-semanticdb` | scalac `-Xsemanticdb` fixture-driven parse/normalize/group tests (fixtures copied from the Scala suites) |
| 3 | `ls-engine` + `ls-doctor` | **full §18.1 matrix ported** — safe families assert exact span sets, unsafe families assert exact rejection reasons; consistency-level routing tests |
| 4 | `ls-bsp` | fake-BSP harness ported; live `mill` BSP smoke (discovery → buildTargets → scalacOptions → compile → diagnostics) |
| 5 | `ls-pc-abi` + `ls-jvm` + `ls-pc-host` | premain-boot spike green on JDK 25 (else contingency §3); echo-op round-trip; layout-canary test; loaned-thread dispatch + watchdog/restart_instances fault-injection; `symbol_definition` callback e2e (the zaozi cross-file nav test re-pointed at the vtable) |
| 6 | `ls-server` wiring + `it/` | black-box LSP e2e over a live mill BSP session: the three capabilities + PC six ops + doctor; cold start (no PC) serves index queries with **no JVM in the process** |
| 7 | deletion + docs | Scala modules removed per §1.2; `architecture.md`/`deployment.md`/`index-format.md` addendum updated; `plan.md` marked superseded-by for topology; nix package = `ls` binary + pc-host jar + JDK 25 runtime reference |

Order is dependency order; phases 5–6 carry the novel risk (FFI boundary,
protocol re-hardening), 1–3 are the tailwind. Nix: crane (or naersk) builds
the workspace; mill's remaining outputs are the pc-host assembly (with
`Premain-Class` manifest) and the zaozi plugin jar; `ivy-lock` shrinks to the
island's dependencies.

## 9. Indicative targets

Cold start to `initialize` response, no PC touched: < 10 ms, zero JVM.
Idle RSS (index-only session): < 50 MB + mmap. First PC request pays JVM boot
(sub-second, lazy; PC-only AOT cache optional later). Query hot paths: ≥
parity with the warmed JVM implementation, minus warmup and GC tails.

## 10. Risks

| Risk | Mitigation |
|---|---|
| premain boot timing on JDK 25 | phase-5 spike first; two-call JNIEnv contingency quarantined in `ls-jvm` |
| upcall exception / Rust panic across ABI | mandatory catch-all wrappers both sides (§4), fault-injection tests in phase 5 |
| ABI struct drift | layout canary at boot + cbindgen header in CI |
| JVM hard crash kills the LS (no forked tier) | escalation ladder (§5); crash-safe store; editor auto-restart; `restart_instances` covers plugin/compiler wedges without JVM death |
| mill BSP quirks re-encountered in Rust | phase-4 live-mill smoke early; `it/` e2e as the gate |
| search semantics drift vs FTS5 | deterministic tiering is spec'd from `FuzzyRank` (already the authoritative ranker); workspace-symbol tests ported from the Scala suite |
| big-bang correctness regression | §18.1 fixture matrix is the ported spec; no feature merges without its cases green |
| `ls-tasty` format drift on Scala bump (v1.1) | envelope closed by scalac (rejects newer TASTy); bump ritual = `TastyFormat.scala` diff + golden-corpus regen as a CI gate |
| symbol-synthesis mismatch vs SemanticDB (v1.1) | declaration-order disambiguation per the SemanticDB spec; cross-oracle fixtures (workspace occurrence string must hit the dep-index key byte-for-byte); ambiguous lookups return all candidates |

--- Original Design Draft End ---
