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
| C ABI | `#[repr(C)]` boundary structs, cbindgen header, two-sided layout canary, the v2 22-op PC vtable + 10-slot Rust vtable (seven payload-query ops — inlay hints, semantic tokens, selection range, code action, auto-imports, PC diagnostics, folding range — plus the `definition_source_toplevels` callback, all provider-backed; a transport-first future op would answer `STATUS_NOT_YET`), lossless encodings (nullable-vs-empty, origin tags, opaque `data` bytes), single-`alloc` response protocol, decode-boundary fuzzing, golden-vector generator `crates/ls-pc-abi/examples/codec_vectors.rs` | `crates/ls-pc-abi/tests/boundary.rs`, `crates/ls-pc-abi/tests/canary.rs`, `crates/ls-pc-abi/tests/roundtrip.rs`, `crates/ls-pc-abi/tests/fuzz.rs`; Java-side offset tests in `modules/ls-pc-host/test` |
| PC payload-query providers (ABI v2) | island providers for the seven payload-query ops: inlay hints (`InlayHintsParams` built from the `inlay_hint_flags` bitset — bit assignment documented at `crates/ls-pc-abi/src/payloads.rs` and mirrored by the island `PcInlayHintFlags`), semantic tokens, selection-range chains (innermost-first parent walks), code-action id dispatch to the typed dotty entry points with `DisplayableException` mapped to the refusal-message result field (data, not status), auto-imports, PC diagnostics through the facade `didChange` path, and the CUSTOM parser-only folding walker `modules/ls-pc/src/ls/pc/FoldingRangeProvider.scala` (untpd parse with error recovery — templates/defs/match+cases/blocks/multi-line arg lists, comment blocks + `//`-runs, import runs, nested `// region` markers; never the typer) | `modules/ls-pc/test/src/ls/pc/FoldingRangeProviderSuite.scala`, `modules/ls-pc/test/src/ls/pc/PcV2OpsSuite.scala`, and the ported dotty corpora `modules/ls-pc/test/src/ls/pc/corpus/InlayHintsCorpusSuite.scala` + `modules/ls-pc/test/src/ls/pc/corpus/SelectionRangeCorpusSuite.scala` + `modules/ls-pc/test/src/ls/pc/corpus/SemanticTokensCorpusSuite.scala` (24 curated cases through the harness port of `TestSemanticTokens.pcSemanticString`) + `modules/ls-pc/test/src/ls/pc/corpus/DiagnosticsCorpusSuite.scala` (the dotty diagnostics-provider cases + the `-explain` variant under a second registered target) (mill `pc.test`); live payload-op proof in `crates/ls-jvm/tests/live_definition.rs` (flake check `pc-definition`) |
| Semantic-tokens legend contract | the island's `Node.tokenType`/`tokenModifier` ints index the PC-vendored `scala.meta.internal.pc.SemanticTokens.TokenTypes`/`TokenModifiers` lists (scala3-presentation-compiler 3.8.4); the server advertises EXACTLY those lists as its legend (`crates/ls-server/src/capabilities.rs` over the pinned constants in `crates/ls-server/src/pc_lsp.rs`), and the contract is pinned CROSS-LANGUAGE — the same 23/10 lists plus the golden anchors ("method" = type index 13, "declaration" = modifier bit 0) asserted Rust-side, island-side against the vendored object itself, and blackbox-side through lsprotocol — so legend drift breaks every build | `crates/ls-server/src/pc_lsp.rs` (the `legend` module + tests), `crates/ls-server/src/capabilities.rs`, `modules/ls-pc/test/src/ls/pc/SemanticTokensLegendSuite.scala` (mill `pc.test`), `it/lsp-blackbox/test_semantic_tokens.py`, `crates/ls-server/tests/server_surface.rs` |
| Semantic tokens (full + full/delta + range) | `textDocument/semanticTokens/full` + `/full/delta` + `/range` gated like hover (`require_semanticdb` hard error, `withPcBuffer` → null — spec `SemanticTokens \| null`, an open buffer always answers a stream, empty included); the island's `[start, end)` UTF-16 offset nodes positioned against the OPEN BUFFER TEXT (`line-index` line split, astral-safe UTF-16 columns), unclassified `-1`/zero-width/hypothetical multi-line nodes dropped (dotty nodes are single-line symbol-name spans — pinned), sorted and LSP delta-encoded. `full` advertises `{delta: true}`: every `/full` response carries a `resultId` (monotonic per-URI counter) and caches its encoded stream (latest result per URI, dropped on didClose, LRU across URIs cap 32 — `SemanticTokensCache`); `/full/delta` recomputes the current stream and answers the single minimal prefix/suffix splice against the cached base (the rust-analyzer `diff_tokens` algorithm, token-granular with ×5 raw-u32 `start`/`deleteCount` — `pc_lsp::semantic_tokens_edits`) as `SemanticTokensDelta {resultId, edits}`, or a FULL `SemanticTokens` resync (spec-legal union) on an unknown/stale/evicted `previousResultId`; `/range` slices the node list server-side before encoding so the first token's deltas restart from the document origin, never cached, no resultId | encoder + diff units in `crates/ls-server/src/pc_lsp.rs` (`tokens_diff_*`), cache units + gate + dispatch in `crates/ls-server/src/services.rs` (`semantic_tokens_cache_*`), wire snapshots + the two-stream delta round trip in `crates/ls-server/tests/pc_wire.rs` (`the_semantic_tokens_full_delta_round_trip_is_served_jvm_free`), cold fallbacks + the typed full→didChange→delta round trip in `it/lsp-blackbox/test_semantic_tokens.py`, real-island stream facts + the empty-delta/stale-resync probe in `it/nvim/e2e.lua` |
| Code actions (`textDocument/codeAction`) | the ASSEMBLY layer over the live island ops — literal `lsp_types::CodeAction`s with inline `WorkspaceEdit`s, EAGERLY resolved at assembly time (each offered action ran its `code_action`/`auto_imports` op during assembly; a typed refusal — `DisplayableException`-as-data — or an empty edit list DROPS the action; no `codeAction/resolve`, no executeCommand round trip; documented at `services::code_action`): the missing-symbol import quickfix parsing exactly the dotty `Not found: (type )?X` message shapes (the `value X is not a member` family deliberately unparsed), preferred when the candidate is unique; the seven refactor probes (insert inferred type, implement all members — quickfix on a `needs to be abstract` / `object creation impossible` context diagnostic, else rewrite —, convert to named arguments — probed at range.start THEN range.end (the dotty provider resolves the call in front of the cursor) with every argument index and an already-named-insert guard —, inline value, create method from usage — only against a Not-found context diagnostic —, extract method — non-empty range only, extraction end = range.end, anchor derived island-side (`PcFacade.extractionAnchor`) —, convert to named lambda parameters); hierarchical `context.only` filtering applied before probing; a 20-action cap; the hover gate ladder (`require_semanticdb` hard error, `withPcBuffer` → `[]`); edits array-ordered per the LSP tied-start rule (`pc_lsp::workspace_edit`); capability `codeActionProvider: {codeActionKinds: [quickfix, refactor.rewrite, refactor.extract, refactor.inline], resolveProvider: false}` | assembly units in `crates/ls-server/src/services.rs` (message-parse matrix, only-filter, eager drop, cap, end-probe, already-named guard), `crates/ls-server/src/pc_lsp.rs` (`workspace_edit` shape + tied-start order), capability pins in `crates/ls-server/src/capabilities.rs` + `crates/ls-server/tests/server_surface.rs` + `it/lsp-blackbox/test_lifecycle.py`, wire snapshots + gates in `crates/ls-server/tests/pc_wire.rs`, cold fallbacks in `it/lsp-blackbox/test_pc_payload.py`, the ported dotty edit corpora `modules/ls-pc/test/src/ls/pc/corpus/{AutoImportsCorpusSuite,InsertInferredTypeCorpusSuite,AutoImplementAbstractMembersCorpusSuite,ExtractMethodCorpusSuite,InlineValueCorpusSuite,ConvertToNamedArgumentsCorpusSuite,ConvertToNamedLambdaParametersCorpusSuite,InsertInferredMethodCorpusSuite}.scala` (mill `pc.test`), and the real-island "Insert type annotation" probe in `it/nvim/e2e.lua` |
| PC live-typing diagnostics | on `textDocument/didChange` (ready path, after the PC mirror update) a DEBOUNCED per-URI last-write-wins `pc_diagnostics` pull (300ms fixed window, `BuildScheduler`-pattern worker, never blocking the loop, NEVER booting a cold island — `PcQueryService::booted` gates the pull, so typing alone keeps the zero-JVM invariant); merge policy: BSP compile diagnostics primary, PC diagnostics publish under the distinct source tag `"scala3-pc (typing)"` ONLY for the open dirty buffer, cleared on didSave/didClose or when a routed BSP publish arrives for the URI — one `PcDiagnosticsLayer` next to the `DiagnosticRouter` (whose reset semantics stay untouched) merges both streams per URI with the clear-once discipline | `crates/ls-server/src/pc_diagnostics.rs` (module doc = the policy; layer + scheduler units), hook wiring in `crates/ls-server/src/services.rs`, the wire flow in `crates/ls-server/tests/pc_wire.rs`, the cold-island non-flow in `it/lsp-blackbox/test_diagnostics.py`, the real edit → tagged publish → revert → clear flow in `it/nvim/e2e.lua` |
| Definition-source toplevels callback | `definition_source_toplevels` resolver over the immutable snapshot (the `symbol_definition` resolution discipline byte-for-byte: exactness filter + requesting-forward-closure visibility; first visible defining doc by lowest target ord with doc-ord tie-break; source-order toplevels via `scan_doc`, locals and nested members excluded, first-seen dedupe), the bootstrap wiring into the island, and a live exhaustive-`match` completion whose case order follows the resolver's list; the island-facade seam consumption is additionally pinned at corpus level (which sealed shapes consult the seam: Java-enum children without source positions do — `match@@` and lambda `case@@` — positioned Scala children never do) | `crates/ls-engine/tests/engine.rs` (the `definition_source_toplevels_*` cases), `crates/ls-server/src/bootstrap.rs`, `crates/ls-jvm/tests/live_definition.rs` (flake check `pc-definition`), `modules/ls-pc/test/src/ls/pc/corpus/CompletionMatchCorpusSuite.scala` (mill `pc.test`) |
| Embedded-JVM boot | dlopen + `JNI_CreateJavaVM` premain-only registration, rendezvous deadline, zero-JNIEnv discipline, loaned-thread dispatch | `crates/ls-jvm-spike/tests/scenarios.rs` (flake check `spike-boundary`), `crates/ls-jvm-spike/tests/no_jni_guard.rs`, `crates/ls-jvm/tests/live_boundary.rs` (flake check `pc-boundary`) |
| Dispatch recovery | watchdog deadline → PC cancel → `restart_instances` → `spawn_dispatch(gen+1)` + replay; bounded abandoned generations; cap-exceeded → orderly exit | `crates/ls-jvm/tests/live_recovery.rs` (flake check `pc-recovery`), `crates/ls-server/tests/real_bsp_pc_recovery.rs` |
| Cross-file navigation callback | `symbol_definition` resolver over the immutable snapshot, forward-closure pruning, zaozi plugin steering over the live vtable | `crates/ls-jvm/tests/live_definition.rs` (flake check `pc-definition`), `crates/ls-jvm/tests/live_zaozi.rs` (flake check `pc-zaozi`), `crates/ls-server/tests/live_pc.rs` (flake check `pc-server-definition`) |
| Workspace method-search callback | `search_methods` resolver over the immutable snapshot (method-descriptor candidates, metals-`Fuzzy` name matching with empty-query-matches-all, forward closure of the requesting PC target, `symbol_definition`-exact def spans), the `MethodHitsResult` payload + RustVtable slot 7 layout, and a live member completion discovering a workspace extension method only reachable through the slot | `crates/ls-engine/tests/engine.rs` (the `search_methods_*` cases), `crates/ls-pc-abi/tests/roundtrip.rs`, `crates/ls-pc-abi/tests/fuzz.rs`, `crates/ls-pc-abi/tests/boundary.rs`, `crates/ls-jvm/tests/live_definition.rs` (flake check `pc-definition`), Java-side layout/codec parity in `modules/ls-pc-host/test` |
| Index navigation (`textDocument/documentSymbol` + `textDocument/implementation`) | documentSymbol: the index-backed NESTED outline (`QueryOrchestrator::document_symbols`) — source-order definition scan over the doc postings, owner-chain nesting with the companion fallback (enum cases under the enum class node), locals/parameters/constructors/setters excluded, `range == selectionRange` (name spans are all the index stores — the documented limitation), dirty buffers answer index truth (outline lags until save), NOT gated on `is_open` (closed files answer), `require_semanticdb` hard error, nested `DocumentSymbol[]` always sent (`hierarchicalDocumentSymbolSupport` not negotiated, flat fallback skipped), capability `documentSymbolProvider: true`, pre-ready `[]`. implementation: METHOD override families only (`QueryOrchestrator::implementations`) — the alias groups do NOT union override families (only the per-rename-group `has_override_family` flag is persisted; type symbols carry no SemanticDB override edges at all), so candidates come from the index (same method name, override-flagged groups — an unflagged cursor answers `[]` with zero file reads) and are verified against the `overridden_symbols` edges of their defining docs' raw `.semanticdb` (transitive chains resolve directly — dotty lists the full chain); def sites via the `symbol_definition` exactness filter, pruned to the requesting forward closure; typed references-style cursor errors; TYPE symbols answer the honest `[]` (no subtype edges anywhere, documented at `docs/architecture.md` §7.1); capability `implementationProvider: true`, pre-ready `[]` | `crates/ls-engine/tests/document_symbols.rs`, `crates/ls-engine/tests/implementations.rs`, unit pins in `crates/ls-server/src/services.rs` + `crates/ls-server/src/lifecycle.rs` + `crates/ls-server/src/server.rs`, wire snapshots + gates in `crates/ls-server/tests/index_nav_wire.rs`, capability pins in `crates/ls-server/src/capabilities.rs` + `crates/ls-server/tests/server_surface.rs` + `it/lsp-blackbox/test_lifecycle.py`, typed-model outline + implementation positive in `it/lsp-blackbox/test_index_queries.py`, real-project probes in `it/nvim/e2e.lua` (no dotty corpus applies — these are index features, not PC providers) |
| Call hierarchy (`textDocument/prepareCallHierarchy` + `callHierarchy/incomingCalls`/`outgoingCalls`) | USAGE-HIERARCHY semantics (ratified Plan C): a "call" is ANY reference occurrence of the item's reference group (the index persists no call-site facts, so eta-expansions/type-position uses count), with exactly ONE noise filter — a reference whose source line begins with the `import` token is dropped (best-effort, fails OPEN when the source cannot be read). prepare (`QueryOrchestrator::prepare_call_hierarchy`): the DEFINITION-side item for a callable cursor (a method descriptor — constructors/setters excluded — or a term whose indexed kind is METHOD/MACRO, i.e. member vals / enum cases); a non-callable cursor answers its ENCLOSING callable when one exists, else `None`; an externally-defined callable answers `None`; typed references-style cursor errors (`NoSymbolAtCursor`/`StaleIndex`/`PcOnlySymbol`); `require_semanticdb` hard error; null for no item; `range == selectionRange` (name spans only); the raw SemanticDB symbol round-trips through the item's `data` field. incoming (`QueryOrchestrator::incoming_calls`): the reference group scanned across ALL docs with NO closure pruning (the deliberate difference from `references` — downstream/disconnected callers are legitimate, pinned by the disconnected target-C caller), import-line refs dropped, grouped by ENCLOSING DEFINITION synthesized from the `document_symbols` entry set (deepest owner-chain entry at-or-before the ref; refs before any def → a synthetic file-level item; the name-span-only false positive — toplevel code after a class body attributes to the last member — pinned). outgoing (`QueryOrchestrator::outgoing_calls`): reference occurrences inside the item's SUCCESSOR-BASED extent (the §7.1-rejected-for-outlines heuristic, accepted here as a query best-effort — trailing code before the next entry is misattributed, pinned as actual behavior), resolved to their targets' definition items. capability `callHierarchyProvider: true`; pre-ready prepare → null, incoming/outgoing → `[]`. The precision upgrade (persisting call-site facts at ingest) is a Plan-A follow-up NOTE, deliberately not implemented (item 11 below) | `crates/ls-engine/tests/call_hierarchy.rs`, unit pins in `crates/ls-server/src/services.rs` + `crates/ls-server/src/lifecycle.rs` + `crates/ls-server/src/server.rs`, wire round trip + cold-island gate in `crates/ls-server/tests/index_nav_wire.rs`, capability + pre-ready pins in `crates/ls-server/src/capabilities.rs` + `crates/ls-server/tests/server_surface.rs` + `it/lsp-blackbox/test_lifecycle.py`, typed-model prepare→incoming/outgoing in `it/lsp-blackbox/test_call_hierarchy.py`, real-project probe in `it/nvim/e2e.lua` (usage-hierarchy semantics documented at `docs/architecture.md` §7.1 + `docs/deployment.md` §4.5) |
| Island parity (retained Scala) | `pc-plugins.json` loading, plugin SPI self-test/disable-on-throw, `PcTargetConfig`, `Utf16Text`, LRU instance cap, SemanticDB-flag stripping | mill suites under `modules/ls-pc/test` (`PluginManagerSuite`, `PcQuerySuite`, `PcSymbolSearchSuite`, `Utf16TextSuite`, `PcWorkerManagerSuite`, `CompilerPluginConfigSuite`), `modules/ls-zaozi-pcplugin/test` (`ZaoziPcNavSuite`), `modules/ls-pc-host/test` |
| LSP server surface | capability set exactness, per-method pre-ready behavior, didChange buffering + reload, diagnostics router semantics, dirty-buffer overlay + PC-only detection, didSave debounce/single-flight, doctor section contract, CLI, `$/cancelRequest` (reader-thread interception into the bounded cancel set, −32800 for a cancelled queued request, `initialize`/`shutdown` never cancelled, unknown/late cancels inert) | unit tests in `crates/ls-server/src/` (capabilities, diagnostics, documents, pc_overlay, doctor, cli, convert, server), `crates/ls-server/tests/bootstrap.rs`, `crates/ls-server/tests/server_surface.rs`, `crates/ls-server/tests/pc_wire.rs` |
| Formatting (`textDocument/formatting` via the scalafmt CLI) | the scalafmt COMMAND LINE over the OPEN buffer (never a scalafmt-core link): binary resolution config `scalafmt` > `LS_SCALAFMT` > `scalafmt` on PATH > the nix-baked wrapper default (`--set-default LS_SCALAFMT` in `nix/package.nix`, mirroring the Java-home baking); workspace-ROOT-only `.scalafmt.conf` discovery (scalafmt owns nested-config semantics) with the typed `no .scalafmt.conf` error; the open-buffer gate as a typed "not open" error (NO SemanticDB gate — pure syntax); the spawn `--stdin --config <ws>/.scalafmt.conf --non-interactive` with cwd = workspace root, `COURSIER_MODE=offline` (the offline stance: the one nix scalafmt version never downloads another — a conf pinning a different version fails typed, the stderr tail naming the unresolvable artifact), a 10s kill-on-expiry deadline, silent-stdout-means-unchanged, and non-zero exits carrying the noise-filtered stderr tail; minimal edits via the `dissimilar` diff→edits fold (the rust-analyzer port) with UTF-16 positions over the ORIGINAL text via `line-index`; LSP `options` ignored (`.scalafmt.conf` is the style authority); capability `documentFormattingProvider: true` with rangeFormatting DELIBERATELY not advertised (the CLI's hidden experimental `--range from=to` skips lines inside multi-line ranges — probed on the nix scalafmt); pre-ready `[]` | resolution/runner/tail/fold units in `crates/ls-server/src/formatting.rs`, handler gates in `crates/ls-server/src/services.rs`, capability + pre-ready pins in `crates/ls-server/src/capabilities.rs` + `crates/ls-server/src/lifecycle.rs` + `crates/ls-server/src/server.rs` + `crates/ls-server/tests/server_surface.rs`, the real-scalafmt wire round trip + typed errors in `crates/ls-server/tests/formatting_wire.rs`, blackbox round trip + typed errors + capability pin in `it/lsp-blackbox/test_formatting.py` + `it/lsp-blackbox/test_lifecycle.py` (flake check `lsp-blackbox` ships scalafmt on PATH), and the real-editor probe in `it/nvim/e2e.lua` (version-match branch formats, mismatch branch pins the typed offline error) |
| Fake-BSP e2e | the end-to-end scenario set over a Rust-hosted fake BSP server | `crates/ls-server/tests/fake_bsp_e2e.rs` |
| Watched files (dynamic registration) | the `initialize` capability read (`workspace.didChangeWatchedFiles.dynamicRegistration`), the one fire-and-forget `client/registerCapability` request after `initialized` (server-side `"ls-server/<n>"` string id space, reply consumed uncorrelated), the three watcher globs as one source of truth, the `globset` event filter (`.semanticdb` → debounced background reingest, workspace `config.json` → PC config re-read, `.bsp/*.json` → log-only), pre-ready events dropping silently | unit tests in `crates/ls-server/src/server.rs`, `crates/ls-server/src/services.rs`, `crates/ls-server/src/capabilities.rs`; wire: `crates/ls-server/tests/watched_files_wire.rs`; black-box stdio: `it/lsp-blackbox/test_watched_files.py`; editor probe: the registration gate in `it/nvim/e2e.lua` |
| PC wire surface (JVM-free) | completion, `completionItem/resolve`, hover, signatureHelp, definition/typeDefinition, and the payload-backed inlayHint/selectionRange/foldingRange methods over the framed wire through the REAL serve loop + REAL `IndexBootstrap`, with the island replaced by the testkit fake PC through the `IndexBootstrap::with_pc` seam; gating (`require_semanticdb` where it applies, `withPcBuffer`, the resolve target gate, the selection/folding no-SemanticDB-gate split — pure syntax stays answerable on a no-SemanticDB source) and response mapping pinned by insta snapshots. The three new methods' LSP shapes are `lsp-types` models bridged from the ABI carriers in `crates/ls-server/src/pc_lsp.rs` (label parts with location/tooltip, verbatim JSON `data`, linked selection parents from innermost-first chains, folding kind strings; capabilities `inlayHintProvider: {resolveProvider: false}`, `selectionRangeProvider`/`foldingRangeProvider: true`), with the server's default inlay-hint category bitset documented at `INLAY_HINT_FLAGS` in `crates/ls-server/src/services.rs` | `crates/ls-server/tests/pc_wire.rs`, `crates/ls-server/src/pc_lsp.rs`, `crates/ls-testkit/src/fake_pc.rs` |
| Shared wire harness | the one copy of the framed-wire builders, the interactive wire client (in-process serve loop or spawned binary over stdio), the fake BSP server, and the fixture-corpus geometry consumed by the `ls-server` suites | `crates/ls-testkit/src/wire.rs`, `crates/ls-testkit/src/client.rs`, `crates/ls-testkit/src/fake_bsp.rs`, `crates/ls-testkit/src/fixtures.rs`, `crates/ls-testkit/src/positions.rs` |
| Black-box stdio e2e | the REAL `ls-server` binary spawned over stdio by an independent client (pytest-lsp), against the scriptable Python fake BSP server over the committed fixture corpus: capability exactness through lsprotocol's typed model (the semantic-tokens legend and `full: {delta: true}` included), readiness, index queries, diagnostics publish/clear, typed unknown-method/command errors, the payload methods' graceful empty/null fallbacks over never-opened buffers (the island stays cold — asserted via the typed cold pcPluginStatus), the semantic-tokens full → ranged didChange → full/delta round trip (resultId + the edits-or-full union shape; token streams may be empty over the fixture corpus — the delta CONTENT is pinned by the fake-PC wire suite), and the live-typing non-flow (a didChange arms the pull, the cold island skips it: no crash, no publish, no boot) | `it/lsp-blackbox/` (`conftest.py`, `fake_bsp.py`, `test_lifecycle.py`, `test_index_queries.py`, `test_diagnostics.py`, `test_robustness.py`, `test_pc_payload.py`, `test_semantic_tokens.py`), flake check `lsp-blackbox`, `scripts/it-lsp-blackbox.sh` |
| Project-level editor e2e | a REAL editor (headless Neovim) attaches the production server to a REAL third-party repo (the pinned, SemanticDB-patched zaozi source, CIRCT-free `decoder` module): readiness over the real mill BSP session, reindex ingest, workspace/symbol, cross-file definition, references, PC-backed hover booting the embedded island against the real project classpath, and the payload probes over the booted island — foldingRange (≥1 well-formed range, kind facts), selectionRange (a widening parent chain containing the anchor), inlayHint (clean round-trip + hint shape), semanticTokens/full (non-empty five-word stream, every index inside the advertised legend, a resultId) plus full/delta (empty edit list over the unchanged buffer, full-data resync on a stale previousResultId), and the live-typing diagnostics flow (a real buffer edit introduces a type error → a `"scala3-pc (typing)"`-tagged publish on the probe line → the revert clears it) | `it/nvim/e2e.lua`, `scripts/it-nvim-zaozi.sh`, CI job `nvim-zaozi-e2e` |
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
- `crates/ls-server/src/server.rs` :: "a_cancelled_queued_request_answers_request_cancelled_without_dispatch"
- `crates/ls-server/tests/pc_wire.rs` :: "a_cancelled_queued_completion_answers_request_cancelled_over_the_wire"
- `crates/ls-server/tests/pc_wire.rs` :: "payload_methods_gate_on_the_buffer_and_split_on_semanticdb"
- `crates/ls-server/tests/pc_wire.rs` :: "the_code_action_wire_surface_assembles_and_drops_jvm_free"
- `crates/ls-server/tests/pc_wire.rs` :: "code_actions_gate_on_the_buffer_and_semanticdb"
- `crates/ls-server/src/services.rs` :: "missing_symbol_name_parses_exactly_the_not_found_shapes"
- `crates/ls-server/src/services.rs` :: "eager_resolution_drops_refused_and_empty_probes"
- `crates/ls-server/src/services.rs` :: "convert_to_named_arguments_probes_the_range_end_and_drops_named_args"
- `crates/ls-server/src/pc_lsp.rs` :: "workspace_edit_orders_a_tied_start_insert_before_the_replacement"
- `it/lsp-blackbox/test_pc_payload.py` :: "test_code_action_on_an_unopened_buffer_is_the_empty_list"
- `crates/ls-server/src/pc_lsp.rs` :: "selection_chain_links_innermost_first_into_parents"
- `crates/ls-server/src/pc_lsp.rs` :: "inlay_hint_maps_the_full_carrier_to_the_lsp_shape"
- `it/lsp-blackbox/test_robustness.py` :: "test_a_cancel_raced_against_a_live_request_yields_exactly_one_reply"
- `crates/ls-server/src/server.rs` :: "watched_files_registration_round_trips_once_with_the_three_globs"
- `crates/ls-server/tests/watched_files_wire.rs` :: "a_watched_semanticdb_event_drives_a_background_reingest_over_the_wire"
- `it/lsp-blackbox/test_watched_files.py` :: "test_registration_arrives_with_the_three_watcher_globs"
- `crates/ls-engine/tests/document_symbols.rs` :: "core_outline_nests_class_object_trait_and_enum_members"
- `crates/ls-server/src/formatting.rs` :: "resolution_prefers_the_workspace_config_over_env_and_path"
- `crates/ls-server/src/formatting.rs` :: "a_wedged_binary_is_killed_at_the_deadline_with_a_typed_error"
- `crates/ls-server/src/formatting.rs` :: "stderr_tail_keeps_the_version_mismatch_and_drops_jvm_noise"
- `crates/ls-server/src/formatting.rs` :: "edits_after_an_astral_char_use_utf16_columns"
- `crates/ls-server/tests/formatting_wire.rs` :: "formatting_round_trips_minimal_edits_against_a_real_scalafmt"
- `it/lsp-blackbox/test_formatting.py` :: "test_formatting_round_trips_minimal_edits_and_is_idempotent"
- `crates/ls-engine/tests/document_symbols.rs` :: "a_dirty_buffer_still_answers_the_indexed_outline"
- `crates/ls-engine/tests/implementations.rs` :: "an_upstream_buffer_is_pruned_to_its_forward_closure"
- `crates/ls-engine/tests/implementations.rs` :: "a_trait_type_symbol_answers_the_honest_empty"
- `crates/ls-server/tests/index_nav_wire.rs` :: "document_symbol_outlines_closed_files_from_the_index"
- `crates/ls-server/tests/index_nav_wire.rs` :: "implementation_resolves_the_corpus_override_family"
- `it/lsp-blackbox/test_index_queries.py` :: "test_implementation_resolves_the_override_family"
- `crates/ls-engine/tests/call_hierarchy.rs` :: "the_enclosing_definition_rule_matrix"
- `crates/ls-engine/tests/call_hierarchy.rs` :: "incoming_includes_disconnected_target_callers_without_closure_pruning"
- `crates/ls-engine/tests/call_hierarchy.rs` :: "outgoing_extent_heuristic_misattributes_trailing_code_after_the_body"
- `crates/ls-engine/tests/call_hierarchy.rs` :: "incoming_drops_import_line_references"
- `crates/ls-server/tests/index_nav_wire.rs` :: "call_hierarchy_prepares_then_round_trips_incoming_and_outgoing"
- `crates/ls-server/tests/real_bsp_e2e.rs` :: "zero"
- `crates/ls-server/tests/fake_bsp_e2e.rs` :: "diagnostics"
- `it/lsp-blackbox/test_lifecycle.py` :: "test_initialize_advertises_the_exact_capability_surface"
- `it/lsp-blackbox/test_diagnostics.py` :: "test_compile_diagnostics_publish_then_clear"
- `crates/ls-engine/tests/engine.rs` :: "symbol_definition_attributes_the_buffer_by_doc_row_under_a_shared_sourceroot"
- `crates/ls-engine/tests/engine.rs` :: "search_methods_prunes_to_the_forward_closure_under_a_shared_sourceroot"
- `crates/ls-jvm/tests/live_definition.rs` :: "the workspace extension method must reach the completion list"
- `crates/ls-engine/tests/engine.rs` :: "definition_source_toplevels_resolves_through_the_requesting_closure"
- `crates/ls-jvm/tests/live_definition.rs` :: "case order must follow the resolver's list"
- `crates/ls-jvm/tests/live_definition.rs` :: "the type error must surface as a live PC diagnostic"
- `modules/ls-pc/test/src/ls/pc/FoldingRangeProviderSuite.scala` :: "region markers pair up and nest via a stack"
- `modules/ls-pc/test/src/ls/pc/PcV2OpsSuite.scala` :: "DisplayableException comes back as data"
- `crates/ls-server/src/pc_lsp.rs` :: "the_legend_is_the_pc_vendored_token_lists"
- `crates/ls-server/src/pc_lsp.rs` :: "semantic_tokens_columns_count_utf16_units_around_astral_chars"
- `crates/ls-server/src/pc_lsp.rs` :: "semantic_tokens_range_slices_overlapping_tokens_and_restarts_deltas"
- `modules/ls-pc/test/src/ls/pc/SemanticTokensLegendSuite.scala` :: "golden anchors: 'method' is type index 13, 'declaration' is modifier bit 0"
- `it/lsp-blackbox/test_semantic_tokens.py` :: "test_the_legend_is_the_pc_vendored_token_lists"
- `crates/ls-server/tests/pc_wire.rs` :: "the_semantic_tokens_wire_surface_is_served_jvm_free"
- `crates/ls-server/src/pc_diagnostics.rs` :: "pc_overlay_merges_after_bsp_and_bsp_publish_supersedes_it"
- `crates/ls-server/src/pc_diagnostics.rs` :: "a_pull_against_a_cold_island_is_skipped_without_querying"
- `crates/ls-server/tests/pc_wire.rs` :: "a_did_change_publishes_pc_tagged_diagnostics_and_did_close_clears_them"
- `it/lsp-blackbox/test_diagnostics.py` :: "test_typing_on_a_cold_island_publishes_no_pc_diagnostics"

## Recorded trims and accepted evolutions

Deviations the rewrite's decision process ratified; recorded here so the docs
and the plan stay reconciled.

1. **`pcPluginStatus` command — implemented (trim latitude not used).**
   Advertised as the fourth executeCommand and routed end-to-end: the island's
   `PcFacade.pluginStatus` report crosses the flat-ABI `plugin_status`
   control-lane slot into the seam's `PcPluginStatusReport`
   (`crates/ls-server/src/pc.rs`), rendered as the Scala `PcStatusRender` text
   summary or the structured `{compilerPlugins, servicePlugins, disabled}`
   object with the doctor's `{"json": true}` argument. The inspection NEVER
   boots the island: a ready-but-cold island answers a typed
   "PC island not booted (cold)" status, and the doctor's `PC Plugins` section
   renders the live report once booted, the same cold reason before
   (`crates/ls-server/src/doctor.rs`). Proven by
   `crates/ls-server/tests/server_surface.rs` (advertised set == routed set),
   `crates/ls-server/tests/pc_wire.rs` (the wire round-trip over the fake PC),
   `crates/ls-jvm/tests/live_boundary.rs` (the live control-lane fetch), and
   `it/lsp-blackbox/test_robustness.py` (the ready-but-cold typed answer over
   real stdio).
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
11. **Call-site facts not persisted at ingest — Plan A follow-up.** Call
   hierarchy ships USAGE-HIERARCHY semantics (Plan C): an occurrence in the
   index records only that a symbol is REFERENCED at a position, never whether
   that reference is an APPLICATION, so the honest v1 answer is the usage graph
   (every reference of the item's group, minus import lines) rather than a
   true call graph. The precision upgrade — Plan A, persisting a per-occurrence
   "is-call-site" fact at ingest (from the SemanticDB synthetic-application
   occurrences the tree already carries) so incoming/outgoing could narrow to
   genuine applications — is recorded as a traceability follow-up and
   deliberately NOT implemented; the usage-hierarchy answer and its one Plan-C
   noise filter (the import-line drop) are the ratified v1 surface
   (`crates/ls-engine/src/call_hierarchy.rs`, pinned by
   `crates/ls-engine/tests/call_hierarchy.rs`).

## Historical

The v1 (Scala implementation) traceability map — its acceptance rows, E-row
table, rename-rule table, and benchmark map — described modules deleted at the
rewrite cutover and was superseded by this file plus `docs/coverage-audit.md`
(which preserves the full v1-suite → v2-test mapping, including rows for the
deleted files, as the port's evidence record).
