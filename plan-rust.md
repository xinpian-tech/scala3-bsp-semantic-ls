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
