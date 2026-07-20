# Architecture

> Normative contract for contributors. Feature semantics derive from `plan.md`
> sections 0–6, 9–13, 17, 21, 23; topology, toolchain, and storage derive from
> the v2 decision record (`plan-rust.md` §0), which supersedes `plan.md` on
> those points. Contract types referenced below live in
> `crates/ls-index-model` (crate `ls_index_model`).
> The on-disk segment format is specified separately in `docs/index-format.md`
> (v1 core + v2 extension sections); the PC plugin boundary in
> `docs/plugin-spi.md`; the build/toolchain contract in `docs/nix-build.md`.

## 1. Positioning

`scala3-bsp-semantic-ls` is a special-purpose language server for **Scala 3 + BSP**
projects only. It does not pursue Metals-style generality; it pursues high accuracy and
high performance for exactly three global capabilities: `workspace/symbol`, whole-repo
`textDocument/references`, and cross-file `textDocument/rename`. All workspace-wide
semantic facts come exclusively from **scalac-generated SemanticDB**; the immutable
mmap segment store is nothing more than a materialized index over that truth. The
Scala 3 Presentation Compiler (PC) serves only interactive editing — completion,
hover, signature help, definition, dirty-buffer overlay, and PC-only plugin
enhancements — and never contributes to the persistent index.

The host process is a single Rust binary (`ls`, cargo workspace under
`crates/`). The PC is the one retained JVM component: an **embedded,
in-process JVM island** (mill modules under `modules/`), booted lazily on the
first PC request and reached over a flat `#[repr(C)]` C ABI via Java FFM
(Panama).

## 2. Hard constraints

Language and runtime, per the v2 decision record (plan-rust.md §0):

```text
Rust stable (pinned via the flake) for the host process
Java 25 only, and only for the PC island
Scala 3 only, exactly pinned (island + workspace support)
BSP only
Nix flake controlled toolchain (cargo workspace + mill, one flake)
No JNIEnv usage; JNI = the single boot symbol (JNI_CreateJavaVM via dlopen)
No SQLite; one storage idiom: immutable mmap segments
  + atomic-rename manifest.json + generational workspace-state files
Linux only
```

The Rust side uses `memmap2` snapshots behind `arc_swap`; the island side uses
Java 25 FFM (`MemorySegment`, upcall stubs, downcall handles), Compact Object
Headers, and no JNI beyond the boot symbol. There is no JNIEnv path anywhere in
the codebase — the JNIEnv fallback once contemplated for boot was retired; the
premain-inside-`JNI_CreateJavaVM` protocol (§3.2) is the only boot path.

Semantic-fact constraints (plan 1.3) — global features trust SemanticDB only:

```text
workspace symbol        -> SemanticDB index only
whole-repo references   -> SemanticDB index only, dirty-buffer PC overlay optional
cross-file rename       -> fresh SemanticDB + mmap postings only
```

Forbidden as sources of global truth:

```text
Bloom filter
name grep
syntax guessing
Metals V2 style source approximation
PC-generated persistent index
PC-only plugin synthetic symbol
```

Plugin boundary (plan 1.4): the SemanticDB compiler plugin belongs to the build
tool / BSP server / scalac configuration — this project only consumes
scalac-generated SemanticDB and never injects the plugin. PC plugins belong to this
project, run only inside the embedded PC island, affect only PC request results, and
must never write the segment store or alter workspace-wide semantic truth.
When semantic truth is unavailable, requests fail with a typed
`ls_index_model::LsError` (e.g. `LsError::NoSemanticdb` for a source whose target
emits no SemanticDB, `LsError::StaleIndex`) rather than degrading to a
pretend-accurate answer.

## 3. Components

```text
editor ⇅ LSP (stdio)
┌───────────────────────────────────────────────────────────────┐
│ ls (Rust binary)                                              │
│  lsp loop · bsp client · semanticdb ingest · segments/store   │
│  references/rename engines · orchestrator · doctor            │
│                                                               │
│  first PC request:                                            │
│    dlopen(libjvm) → JNI_CreateJavaVM(-javaagent:pc-host.jar)  │
│  ┌─────────────── JVM island (same process) ────────────────┐ │
│  │ ls-pc-host (premain, FFM stubs/handles, flat marshalling)│ │
│  │ ls-pc (PcFacade / PcInstance / plugin SPI)               │ │
│  │ scala3-presentation-compiler + zaozi pc-plugin           │ │
│  └──────────────────────────────────────────────────────────┘ │
│  boundary: C function-pointer vtables, both directions        │
└───────────────────────────────────────────────────────────────┘
```

Cargo workspace crate map (`crates/`):

| Crate | Contents |
|---|---|
| `ls-index-model` | opaque-newtype ids/ordinals, `Span`/`Pos` packing, `occ_flags`, `Role`, `unsafe_reason`, `TargetBitset`, `LsError`, `file://` URI handling |
| `ls-semanticdb` | `semanticdb.proto` parse, md5 validation, locator, normalization (`NormalizedDocument`), ref/rename group builder, rename profiles |
| `ls-store` | segment writer/reader (`index-format.md` v1 + v2 sections), `memmap2` snapshots behind `arc_swap`, `manifest.json`, `workspace-state-<gen>.bin`, symbol search (`FuzzyRank` port) |
| `ls-bsp` | `.bsp/*.json` discovery, hand-rolled JSON-RPC client, project model + exact dependency-graph queries, SemanticDB-flag extraction, diagnostics forwarding |
| `ls-engine` | ingest pipeline, query orchestrator (three paths, three consistency levels), references + rename + documentHighlight engines, dirty-buffer overlay SPI |
| `ls-pc-abi` | every cross-boundary `#[repr(C)]` type + flat payload codecs + layout canary; `cbindgen` emits `boundary.h`, the single contract source |
| `ls-jvm` | libjvm dlopen, single-symbol boot, vtable registry + rendezvous, dispatch/control lanes, replay mirror, watchdog + recovery ladder, stdout guard |
| `ls-server` | stdio LSP loop (hand-rolled JSON-RPC), lifecycle state machine, bootstrap, capabilities, PC island service, diagnostics router, doctor + `ls dump`, CLI, `main` |
| `ls-bench` | ingest + query benchmarks with ground-truth consistency checks |

(`ls-jvm-spike`/`ls-pc-host-spike` are retained boot-protocol spike harnesses,
not part of the production binary.)

Retained JVM island (mill keeps building, see `build.mill`): `ls-pc`
(facade/instances/plugin SPI — the worker-protocol files are deleted),
`ls-pc-host` (premain agent, jextract-generated boundary bindings over
`boundary.h`, flat marshalling), `ls-zaozi-pcplugin`. `docs/plugin-spi.md`
remains normative for the island.

### 3.1 Rust main-process responsibilities

```text
LSP protocol (stdio, hand-rolled JSON-RPC)
BSP client (discovery, session, project model, compile)
SemanticDB scanning / parsing / ingest
immutable segment store + manifest + workspace-state
snapshot publish / recovery / janitor
workspace symbol (segment-resident search)
references
rename
diagnostics forwarding
project state
index state
doctor / ls dump
island boot, dispatch, watchdog, recovery
```

### 3.2 Embedded PC island responsibilities

```text
Scala 3 Presentation Compiler
PC compiler plugin loading
PC service plugin loading
synthetic source provider
dirty buffer overlay answers
completion / completion resolve / hover / signature / definition / type definition
prepareRename
plugin status
```

There is **no forked PC worker** (removed) and no PC child process. The island
is embedded in the `ls` process and booted lazily: document notifications only
update a Rust-side buffer mirror; the **first PC query** boots the JVM (an
index-only session stays zero-JVM for its whole life — asserted from
`/proc/self/maps`).

Boot protocol (zero-JNIEnv, `crates/ls-jvm/src/boot.rs`):

1. Resolve `JAVA_HOME` and `dlopen($JAVA_HOME/lib/server/libjvm.so)`.
2. Call `JNI_CreateJavaVM` — the **only JNI artifact** in the codebase; its arg
   structs are hand-declared `#[repr(C)]` in `ls-jvm` (no `jni.h`, no bindgen,
   no `jni` crate). Options: the pc-host assembly on the class path,
   `--enable-native-access=ALL-UNNAMED`, `-XX:+UseCompactObjectHeaders`,
   `-Dls.pc.host.workspace=<root>`, `-javaagent:<pc-host.jar>=0x<rust_vtable_addr>`.
3. `PcHostAgent.premain` fires **inside** the `JNI_CreateJavaVM` call (no main
   class exists): it reads the Rust vtable address from the agent argument,
   verifies `abi_version` and the layout canary (refusing to register on any
   mismatch), re-points `System.out` at real stderr, builds an FFM upcall stub
   for each of the 15 PC vtable slots in `Arena.global()`, downcalls
   `register_pc_vtable`, and loans two platform threads to Rust — `pc-dispatch`
   (worker 0) and `pc-control` (worker 1) — which enter `pc_dispatch_loop` and
   never return.
4. Rust blocks on a condvar rendezvous until registration and the dispatch lane
   land: an ABI-mismatch registration fails fast; a silent premain times out
   with the captured island log surfaced by the doctor. The `JavaVM*`/`JNIEnv*`
   returned by `JNI_CreateJavaVM` are ignored; teardown is process exit
   (`DestroyJavaVM` is never called).

Steady state (`crates/ls-jvm/src/watchdog.rs`): PC requests serialize on the
single loaned dispatch lane under a per-request deadline; control ops
(`restart_instances`/`shutdown`/`plugin_status`) run on the control lane so a
wedged compiler can be reached while dispatch is stuck. A nonzero PC status is
a typed error, no recovery. A deadline overrun fails the request typed and
escalates the **dispatch-generation recovery ladder**:

```text
deadline overrun
  → restart_instances (facade shutdown+recreate — the cooperative cancel;
    the 22-op vtable has no separate cancel op)
  → lane still wedged: spawn_dispatch(generation+1) — the island loans a fresh
    dispatch thread; the Rust replay mirror re-registers targets and replays
    open buffers into the new generation (the editor reopens nothing)
  → abandoned-generation cap exceeded, or a failed control op (JVM hard-crash
    territory): island-fatal → orderly process exit; the editor restarts the
    server against the crash-safe on-disk store
```

Failed lifecycle ops are never replayed: the mirror records only what the
island accepted (plus notifications observed during a wedge — editor facts).

Boundary contract (`crates/ls-pc-abi`): flat `#[repr(C)]` structs both
directions, no JSON. Strings are `LsStr { ptr, len }` (UTF-8, no NUL); variable
payloads use `header + fixed-width records + string blob (offset,len)` — the
segment-file idiom. All cross-boundary allocation is Rust's (`alloc`/`free` in
the Rust vtable); request memory is caller-owned, valid only for the call.
Every function returns an `i32` status; every Java upcall body is wrapped
`catch Throwable`, every Rust export `catch_unwind`. `abi_version` plus an
independently recomputed layout canary are checked at registration; `cbindgen`
regenerates `boundary.h` and jextract generates the island's Java bindings from
it, so drift is a boot refusal, not a corrupt call. The Rust vtable carries
`alloc`, `free`, `log`, `register_pc_vtable`, `pc_dispatch_loop`,
`symbol_definition` — the index-backed cross-file go-to-definition callback —
`search_methods` — the index-backed workspace method search behind the PC
`SymbolSearch.searchMethods` seam (member-mode extension-method / implicit-
class-member discovery) — and `definition_source_toplevels` — the index-backed
toplevel-symbol callback behind `SymbolSearch.definitionSourceToplevels`
(answering empty until its engine query lands). The index callbacks read
**only** the immutable snapshot (the index writer is unreachable from island
threads by construction) and scope results to the requesting target's forward
dependency closure (`symbol_definition` locates the target by the buffer uri;
`search_methods` is handed the PC target id directly). The PC vtable is the
22-op surface: `register_target, did_open, did_change, did_close, completion,
completion_resolve, hover, signature_help, definition, type_definition,
prepare_rename, plugin_status, restart_instances, shutdown, spawn_dispatch`,
plus the ABI-v2 payload-in/payload-out queries `inlay_hints, semantic_tokens,
selection_range, code_action, auto_imports, pc_diagnostics, folding_range`
(one shared `PcPayloadQueryFn` slot shape; every island provider is live — a
transport-first future op would answer the typed `STATUS_NOT_YET`, which the
Rust side degrades to the query's empty fallback). Five payload ops are
LSP-exposed today: `textDocument/inlayHint`, `textDocument/selectionRange`,
`textDocument/foldingRange` — bridged to `lsp-types` protocol shapes in
`crates/ls-server/src/pc_lsp.rs` (selection/folding are pure syntax and skip
the SemanticDB gate; inlayHint keeps the full hover-style gate discipline) —
plus `code_action` and `auto_imports`, which back `textDocument/codeAction`'s
ASSEMBLY layer (`crates/ls-server/src/services.rs`): literal actions with
inline `WorkspaceEdit`s, each op probed eagerly at assembly time so a typed
refusal (`DisplayableException`-as-data) or an empty edit list drops the
action before the client ever sees it.

stdout protection: the JVM and any PC plugin can write to fd 1, which would
corrupt the LSP stream. Before boot, the server's `StdoutGuard` duplicates the
real stdout to a private fd (handed to the LSP writer) and `dup2`s fd 1 onto
stderr; the premain closes the Java-level gap by re-pointing `System.out` at
stderr. Every stray island write lands on stderr.

### 3.3 Storage layout

```text
.scala3-bsp-semantic-ls/
  manifest.json                     # single commit point: names the active
                                    #   (segment, state) pair + checksum
  workspace-state-<generation>.bin  # cross-generation residue (uri -> epoch, md5)
  segments/segment-NNNNNN/          # immutable mmap segment, 6-digit id,
                                    #   14 CRC32C files (docs/index-format.md)
  tmp-*/, *.tmp                     # crash debris only; removed at open
  pc-plugins.json                   # PC plugin config, read by the island premain
  pc/generated-sources/             # synthetic sources from PC plugins
```

There is no `meta.sqlite` — SQLite was removed by the v2 decision record. What
its tables held is now segment-resident or generational: per-target metadata in
`target-meta.bin`, per-symbol metadata in `symbol-meta.bin`, workspace-symbol
search rows in `search.bin` (all three inside the segment, per the v2 extension
sections of `docs/index-format.md`), the manifest in `manifest.json`, and the
cross-generation document residue in `workspace-state-<generation>.bin`. The
references/rename hot path reads only mmap postings; `workspace/symbol` reads
the search and symbol-meta sections of the **same segment**, so search and
postings are always one generation and can never disagree about the world.

### 3.4 Protocol stack and server surface

Both protocol stacks are hand-rolled Rust: `Content-Length` framing plus the
request/notification/response model over `serde_json` (`ls-server::jsonrpc`
for LSP, `ls-bsp` for the BSP client). Neither lsp4j nor bsp4j exists in the
server (their eviction/forwarder workarounds went with them); the island
retains lsp4j **internally only** as the presentation compiler's carrier
types, converted to flat ABI payloads before anything crosses the boundary.

The LSP message loop (`ls-server::server::serve`) is a scoped reader thread
plus a single dispatch thread. The reader parses frames in order into an
in-process queue and intercepts `$/cancelRequest` into a bounded cancel set
(never enqueued), so a cancel is seen even while dispatch is deep in a slow
request — e.g. a cold-boot PC completion — with typed-ahead requests queued
behind it. Dispatch stays strictly single-threaded: the ready services, the
per-turn bootstrap/reload polling, and the shutdown/exit semantics are
unchanged from the synchronous loop. A queued request whose id was cancelled
answers `RequestCancelled` (−32800) without dispatching; an in-flight request
runs to completion and answers normally (spec-legal); `initialize` and
`shutdown` are never cancelled; a cancel for an unknown or already-answered id
is inert. (rust-analyzer's `lsp-server` scaffold was evaluated for reuse; its
`Connection` owns the transport and its `ReqQueue` tracks in-flight requests
for concurrent handlers — neither fits the borrowed-reader/serial-dispatch
architecture, so only its reader-thread discipline — stop after forwarding
`exit` — and the −32800 answer were borrowed conceptually.)

The capability surface is unchanged from v1 with one recorded trim, deferred
rather than silently dropped: the no-BSP warm-restart mode over a recovered
index (bootstrap fails cleanly: the persisted segment carries no target
dependency graph, and a permissive fallback would answer references across
unrelated identically-named symbols). `documentHighlight` is retained, and the
`pcPluginStatus` executeCommand is implemented over the island's flat-ABI
`plugin_status` control-lane slot — a still-cold island answers a typed
"not booted (cold)" status (the inspection never boots the JVM), and the
doctor's `PC Plugins` section renders the live report once the island boots.
The executeCommand set is `scala3SemanticLs.doctor` | `scala3SemanticLs.reindex`
| `scala3SemanticLs.compile` | `scala3SemanticLs.pcPluginStatus`. The CLI is
`--version`, `--doctor [dir] [--json]`,
and `dump [dir]` — the read-only store inspector that replaces ad-hoc `sqlite3`
poking of the removed metadata store; the PC-backend selection flags are gone.
One capability is registered dynamically: when the client's `initialize`
advertises `workspace.didChangeWatchedFiles.dynamicRegistration` — the
server's only client-capability read, a narrow typed flag rather than a
general capability model — the server registers three client-side file
watchers after `initialized` (the reingest-triggers paragraph in §5.2 has the
globs and reactions).

## 4. Query orchestrator: three paths, three consistency levels

Every request is routed through exactly one of three paths
(`crates/ls-engine/src/orchestrator.rs`):

```text
IndexPath:
  The mmap segment snapshot (postings, dictionaries, search sections).
  The normal hot path.

RawSemanticDBPath:
  When the snapshot is stale/missing for a document, read the .semanticdb file
  directly, validate md5, answer from it, and write through: the production
  orchestrator runs the full-generation ingest INLINE on the calling thread,
  republishing the segment and clearing needs_reindex before returning —
  write-through parity with the retired per-document SQLite write-through is
  preserved at generation granularity.

PCPath:
  Dirty buffers and interactive editing features. Never persisted.
```

The three consistency levels are routing behavior in the engines (there is no
runtime enum; each capability is hard-wired to its level):

| Level | Used by | Meaning |
|---|---|---|
| Best effort | `workspace/symbol` | Answer from the current snapshot even if some documents are stale. |
| Fresh preferred | `textDocument/references` | Prefer fresh facts; a stale/unindexed doc resolves via RawSemanticDBPath (healing inline); dirty buffers may add a PC overlay, clearly non-persistent. |
| Fresh required | `textDocument/rename` | Must compile first, ingest fresh SemanticDB, and answer only from a fresh snapshot. No fresh truth ⇒ reject (`LsError::CompileFailed`, `LsError::StaleIndex`). |

## 5. Snapshots

### 5.1 Snapshot model and lifecycle

`ls_store::Store` owns the active generation behind an
`arc_swap::ArcSwapOption<Snapshot>`. A `Snapshot` is the immutable, validated
(segment, workspace-state) pair: the mmapped segment reader plus the decoded
state payload. Readers clone the `Arc` (`store.current()` / `store.retain()`);
Rust ownership replaces the manual retain/release loan of the Scala
implementation — the mmap and state stay alive for as long as any clone is
held, across any number of concurrent publishes. Segments are immutable; there
is no in-place update and no lock on the read path. A superseded generation's
files are deleted by the **janitor** only after its snapshot's strong count
drops to zero.

### 5.2 Write pipeline

```text
 1. BSP compile succeeds (or an explicit reindex / background reingest runs).
 2. Full rescan of the target SemanticDB roots (one segment per generation; no
    incremental watcher — every ingest re-reads the SemanticDB tree).
 3. Parse, validate md5, assign per-document epochs from the previous
    generation's workspace state (bumping on md5 change), normalize, build
    exact alias groups (ls-semanticdb).
 4. Materialize the whole-workspace SegmentData with dense ordinals (ls-engine).
 5. Segment publish: write all files into tmp-<id>/, fsync every file and the
    tmp dir, atomic-rename into segments/segment-NNNNNN, fsync segments/.
 6. workspace-state-<generation>.bin: write tmp, fsync, rename, fsync dir.
 7. manifest.json commit — THE single commit point (write tmp, fsync, rename,
    fsync dir), atomically pairing segment id + dir, state generation, state
    payload CRC32C, and doc/symbol counts.
 8. Open + cross-validate the new (segment, state) pair; one ArcSwap swap
    publishes it.
 9. The old generation retires; the janitor deletes its segment directory and
    state file only after its snapshot Arc fully drops.
```

**Reingest triggers.** Every (re)ingest is the same full rescan; only the
trigger differs: (1) the bootstrap ingest on `initialized`; (2)
`textDocument/didSave` — the debounced, single-flight build job compiles the
saved file's reverse-dependency closure, then reingests
(`crates/ls-server/src/build_scheduler.rs`); (3) a RawSemanticDBPath answer
that could not heal inline schedules a reindex-only job on the same scheduler;
(4) `buildTarget/didChange` from the build server reloads the model over the
retained session and reingests; (5) the explicit `scala3SemanticLs.reindex`
command; (6) client-watched files. For (6): when the client's `initialize`
advertises `workspace.didChangeWatchedFiles.dynamicRegistration`, the server
sends ONE fire-and-forget `client/registerCapability` request after
`initialized` (its id from the server-side `"ls-server/<n>"` string id space,
disjoint from client ids; the reply is consumed uncorrelated) registering
three watchers: `**/*.semanticdb`, `**/.scala3-bsp-semantic-ls/config.json`,
and `**/.bsp/*.json`. A watched `.semanticdb` change — a build that ran
outside the editor — schedules the same debounced reindex-only job; a
`config.json` change nudges the PC island to re-read its configuration (the
didChangeConfiguration path); a `.bsp/*.json` change is only logged
("restart the server to reconnect" — re-bootstrapping a live session in place
is out of scope by decision). The event filter is the upstream ripgrep-family
`globset` matcher compiled once over the SAME registered globs
(`crates/ls-server/src/services.rs` over
`crates/ls-server/src/capabilities.rs::watch_globs`), so watched and reacted
globs cannot drift. Without the capability no registration is sent and the
manual reindex command stays the fallback; pre-ready watched events drop
silently — the bootstrap ingest reads the current files anyway.

A reader can never observe a partially written generation: a segment directory
under `segments/` is complete by construction (step 5), and the pair becomes
reachable only through the manifest commit (step 7) and the swap (step 8).
Ingests are serialized by the orchestrator's ingest lock — the single-writer
contract holds across the message loop, the inline write-through, and the
background build-job scheduler.

Crash recovery matrix (deterministically exercised via injected `Failpoint`s,
no real `kill -9` needed):

```text
torn manifest.json.tmp            -> rename is atomic: the old manifest wins,
                                     the old generation serves; debris removed
state durable, manifest old       -> old generation serves; orphan reclaimed
state/manifest generation or
  checksum mismatch               -> typed refusal (PairMismatch); a mixed
                                     pair is never served
manifest -> missing/corrupt/
  non-canonical segment           -> typed refusal; heals on the next ingest
future schema versions            -> typed refusal, never a guess
```

`Store::open_readonly` recovers the same pair without creating or cleaning
anything, so the doctor and `ls dump` can inspect a store a live server owns.

### 5.3 Epoch filtering

Every group-postings record carries a `doc_epoch`. A record is valid only if:

```text
record.doc_epoch == doc_dictionary[doc_ord].epoch
```

The filter is enforced inside the segment reader on every group scan. In the
one-segment-per-generation model the two always match by construction, but the
invariant (and the block-level `epoch_min`/`epoch_max` skip metadata) is
retained so layered delta segments remain a reader/manifest change, not a
format change (`docs/index-format.md`). Scans push results through
primitive-record callback sinks (`GroupRecord`), so the hot path performs no
per-occurrence allocation.

## 6. ID layering

The v2 decision record ratified an ID contract change: the stable numeric id
layer (`SymbolId`/`DocId`/`TargetId` as interned keys) existed to live in
SQLite, and was retired with it. The layering is now:

| Concept | Stable key (persistent) | Snapshot ordinal (dense int, one snapshot only) |
|---|---|---|
| Document | the SemanticDB uri string (workspace-state, doc dictionary) | `doc_ord` |
| Symbol | the semantic symbol string (symbol dictionary) | `symbol_ord` |
| Build target | the BSP target id string (`target-meta.bin`) | `target_ord` |
| Reference group | — (rebuilt per generation) | `ref_group_ord` |
| Rename group | — (rebuilt per generation) | `rename_group_ord` |

Rules:

- **The stable keys are the strings themselves** — uri and semantic symbol —
  carried in `workspace-state-<generation>.bin` and the segment dictionaries.
  They are the only identities that survive generations. The segment format's
  persistent-id fields (`doc_id`, `symbol_id`, `target_id`) are filled with
  deterministic FNV-1a hashes of those strings (`ls-engine/src/hash.rs`) —
  stable pure functions of the key, requiring no central intern store.
- Snapshot ordinals are dense ints valid **only within one snapshot**, assigned
  at segment build time for O(1) array lookup on the query hot path (e.g.
  `symbol_view(symbol_ord)` — no binary search). Never persist an ordinal;
  never carry an ordinal across a snapshot swap.
- Conversion happens only through the segment dictionaries
  (`find_symbol_ord`, `semantic_symbol_of`, `uri_of`, `symbol_view`,
  `target_meta`, …).

Symbol identity follows the SemanticDB spec, unchanged: a global symbol is
unique per universe; a local symbol is meaningful only together with its
document. `SymbolKey(semantic_symbol, local_doc)` encodes this; local symbols
are qualified by their document's stable id in the dictionary encoding
(`ls-engine/src/symbol_encoding.rs`).

## 7. Alias groups: reference group vs rename group

Two group notions are materialized separately and must never be conflated:

```text
reference group  (ref_group_ord)    — what "references" merges
rename group     (rename_group_ord) — what "rename" may edit together
```

Rationale: symbols that may legitimately be *merged for references* are not
necessarily *safe to rename together*. The families that force this split:

```text
class vs constructor
class vs companion object
val getter
var getter / setter
apply / unapply
method overload
extension method
opaque type
exported symbol
override family
```

Policy:

- **references**: build the most complete exact alias group possible.
- **rename**: group conservatively; reject unsafe families outright (recorded in
  the group's `unsafe_reason_mask`, e.g.
  `unsafe_reason::UNSUPPORTED_SYMBOL_FAMILY`, `unsafe_reason::OVERRIDE_FAMILY`).

Opaque type: an `opaque type T` and its companion `object T` merge into one
group for *references* (references find the type, the companion, and every
use), but *rename* is conservatively **rejected**
(`unsafe_reason::OPAQUE_TYPE`) — renaming an opaque type together with its
companion and uses cannot be proven safe. This matches the `override` /
`exported` / synthetic families, which also reject. The opaque property is read
from SemanticDB (`sym_props` Opaque bit) at ingest.

## 8. References flow

```text
1. symbol-at-cursor
2. symbol -> ref group
3. allowed target pruning
4. mmap ref postings scan
5. optional dirty buffer PC overlay
6. dedupe
7. return LSP Location[]
```

### 8.1 symbol-at-cursor

```text
dirty file:           PCPath wins — the overlay must answer; if it cannot, the
                      query degrades (LsError::StaleIndex) rather than guessing
                      from a snapshot that has not seen the buffer.
clean fresh file:     snapshot doc postings win (segment symbol_at, md5-gated
                      against the workspace-state record).
index stale/missing:  RawSemanticDBPath reads the .semanticdb, validates md5,
                      answers, and heals inline (full-generation write-through).
```

### 8.2 Target-graph exact pruning

The BSP build target graph (`WorkspaceTargets`) gives dependency edges. For a
symbol defined in target `T`, the allowed reference targets are
`reverse_dependency_closure(T)` = `T` plus all targets transitively depending
on `T`. This is an **exact upper bound derived from the build graph, not an
approximation**. Per snapshot it is converted to a `TargetBitset` over target
ordinals — an exact membership bitset, explicitly not a probabilistic filter —
which is also what block-level skip metadata intersects against.

### 8.3 Flow in terms of the real snapshot API

```rust
let cursor = orch.symbol_at_cursor(uri, line, character)?;  // overlay / snapshot / raw
let snap   = orch.current_snapshot().ok_or(...)?;
let seg    = snap.segment();
let ord    = seg.find_symbol_ord(&cursor.encoded_symbol()); // dictionary binary search
let group  = seg.symbol_view(ord).ref_group_ord;
let allowed = orch.allowed_targets_for(&snap, ord);         // reverse dependency closure
seg.scan_ref_group(group, Some(&allowed), &mut sink);       // epoch-checked in the reader
if include_declaration {
    seg.scan_def_group(group, &mut sink);                   // sink re-applies `allowed`
}
// + optional dirty-buffer overlay, dedupe by (uri, span), sort by (uri, position)
```

Definition scans are restricted to the same allowed set as references, so
disconnected targets that reuse symbol names never leak in. A fresh symbol that
is not in the snapshot yet is served for its own document by the raw
`.semanticdb` fallback.

The dirty-buffer overlay fan-out is **group-keyed**: references query the
overlay for *every* member symbol of the cursor's ref group, not just the
symbol under the cursor, so an overlay occurrence keyed to a companion member,
`apply` forwarder, or getter/setter alias is still surfaced. The fan-out is
gated on `DirtyBufferOverlay::contributes_occurrences`; the production
`PcOverlay` (`ls-server`) leaves it `false` and its `occurrences_of` is a
deliberate no-op — the island contributes symbol-at-cursor for dirty files but
no extra reference occurrences yet — so the group-keyed query costs nothing in
production and is exercised only by test overlays that opt in.

`references(includeDeclaration = false)` reads only reference postings;
`includeDeclaration = true` additionally reads definition postings. Occurrence
roles mirror SemanticDB (`Role::Reference` / `Role::Definition`);
per-occurrence flags are the exact bits of `occ_flags`
(`DEFINITION`, `EDITABLE`, `GENERATED`, `READONLY`, `SYNTHETIC`). Positions
flow through the sinks in the columnar packed encoding `Span::pack`
(`line << 12 | char`), unpacked into `Span` / `Loc` only at the LSP conversion
boundary. Positions are zero-based line/character with exclusive end, matching
both SemanticDB `Range` and LSP `Position` semantics.

## 9. Rename flow and safety

Rename runs at the fresh-required consistency level
(`crates/ls-engine/src/rename.rs`):

```text
 1. dirty-buffer / PC-only check
 2. new-name validation (identifier legality; keywords backtick-quoted)
 3. prepareRename pre-checks on the current state
 4. BSP buildTarget/compile over the definition target's reverse dependency closure
 5. full fresh SemanticDB ingest
 6. publish fresh snapshot
 7. re-resolve symbol-at-cursor on the FRESH snapshot
 8. symbol -> rename group; unsafe_reason_mask gate
 9. editable rename-postings scan
10. shared-source consistency check for every edited uri owned by several targets
11. md5 re-validation of every edited document immediately before emitting
12. produce WorkspaceEdit
```

(BSP explicitly documents `buildTarget/compile` before `textDocument/rename` to
ensure workspace sources typecheck and are up to date.)

### 9.1 Safety rules — all must hold

```text
fresh compile succeeded
fresh SemanticDB available
source md5 matches SemanticDB md5
all edits are workspace sources
no readonly sources
no dependency sources
no generated sources by default
no PC-only symbol
no synthetic-only occurrence
no unsafe override family by default
shared-source targets agree on same rename group
```

Violations of the first three are request-time failures
(`LsError::CompileFailed`, `LsError::StaleIndex`). The rest are precomputed at
ingest into the rename group's unsafe-reason bitmask, `unsafe_reason`:

| Bit | Rule enforced |
|---|---|
| `EXTERNAL` | edits must stay inside workspace sources |
| `GENERATED_OCCURRENCE` | no generated sources by default |
| `READONLY_OCCURRENCE` | no readonly sources |
| `DEPENDENCY_SOURCE` | no dependency sources |
| `PC_ONLY` | no PC-only symbol |
| `SYNTHETIC_ONLY` | no synthetic-only occurrence |
| `OVERRIDE_FAMILY` | no unsafe override family by default |
| `SHARED_SOURCE_DISAGREEMENT` | shared-source targets must agree on the rename group |
| `UNSUPPORTED_SYMBOL_FAMILY` | conservative rename grouping (e.g. apply/unapply, export) |
| `OPAQUE_TYPE` | conservative opaque-type rejection |

### 9.2 RenameProfile

The rename profile is precomputed at ingest, persisted per rename group in
`rename-group-index.bin` (the 16-byte profile entry, `docs/index-format.md`),
and consulted at request time:

```rust
pub struct RenameProfile {
    pub is_local: bool,                  // local symbol (document-scoped)
    pub is_external: bool,               // defined outside the workspace
    pub has_generated_occurrences: bool, // some occurrence lives in a generated source
    pub has_readonly_occurrences: bool,  // some occurrence lives in a readonly source
    pub has_override_family: bool,       // participates in an override family
    pub has_companion: bool,             // has a companion class/object
    pub editable_occurrence_count: i32,  // occurrences the rename would actually edit
    pub unsafe_reason_mask: i64,         // OR of unsafe_reason bits; 0 == safe
}
```

Request-time decision is a single integer test: `unsafe_reason_mask != 0` ⇒
reject with concrete reasons via `unsafe_reason::explain(mask)` wrapped in
`LsError::RenameRejected(reasons)`. Rename edits are generated only from the
**editable** rename postings (occurrences flagged `occ_flags::EDITABLE`), which
by construction exclude readonly/generated/dependency sources.

## 10. Performance design checklist

Mandatory from the first version:

```text
 1. dense snapshot ordinals
 2. exact ref group / rename group
 3. role-separated postings
 4. editable rename postings
 5. doc-postings interval index
 6. target graph exact pruning
 7. block-level exact skip metadata
 8. immutable segments
 9. Arc snapshot lifecycle (ArcSwap publish; janitor after drop)
10. batch SemanticDB ingest, single-writer serialized
11. segment-resident symbol/target dictionaries (no intern round-trips)
12. deterministic search tiering — the Rust FuzzyRank port over search.bin
    (exact > prefix > camel-hump/subsequence, bounded candidate cap),
    replacing FTS5 bm25 (removed) by ratified decision; the match SET is
    unchanged, only the ranking is the deterministic tiering
13. memmap2 mmap snapshots
14. bench harness with ground-truth consistency checks (ls-bench)
15. janitor reclamation of superseded generations
```

Forbidden performance shortcuts:

```text
Bloom filter for correctness
source token grep
syntax-only references
PC-generated persistent global index
```

## 11. Risks and mitigations

| Risk | Mitigation |
|---|---|
| BSP server produces no SemanticDB | SemanticDB is mandatory: every request on a source in such a target is a hard `LsError::NoSemanticdb` error (no PC fallback, no empty result) and the doctor surfaces the unavailable targets — for every target, including Mill's own `mill-build`; never fall back to an approximate index. |
| SemanticDB is stale | md5 check; per-document epoch check; compile-before-rename; stale target status; inline write-through heal. |
| PC plugin diverges from the real build | SemanticDB remains truth; PC plugins affect editing only; PC-only symbols can never be globally renamed (`LsError::PcOnlySymbol`). |
| segment / manifest / state inconsistency | atomic rename; fsync ordering; CRC32C everywhere; the manifest as the single commit point pairing segment + state generation + checksum; typed refusal on any mismatch; startup debris cleanup. |
| Rename edits the wrong thing | fresh-required ladder; editable postings only; `RenameProfile`; shared-source consistency check; unsafe-family rejection; pre-emit md5 re-validation. |
| PC wedge or plugin hang | per-request watchdog deadline; the recovery ladder (`restart_instances` → `spawn_dispatch(gen+1)` with target/buffer replay) recovers without JVM death — the replacement for the retired forked-worker respawn. |
| JVM hard crash kills the LS (no forked tier) | bounded ladder ends in an orderly process exit; the store is crash-safe (a reader-visible generation is always complete); the editor's restart lands on the recovered index. |
| ABI struct drift between Rust and the island | `abi_version` + layout canary at registration (mismatch refuses boot); cbindgen header + jextract bindings regenerated from one source; per-slot null checks on the registered vtable. |
| Nix/Mill lock drift | CI checks `nix/ivy-lock.nix` (`scripts/check-ivy-lock.sh`); PRs must include lock updates; `nix flake check` as gate. |

## 12. Final design principles

```text
BSP provides project facts.
scalac SemanticDB provides semantic facts.
Immutable mmap segments provide metadata, search, dictionaries, and exact
  high-speed reference/rename lookup; manifest.json provides the commit point;
  workspace-state files provide the cross-generation residue.
Scala 3 PC provides interactive editing, embedded as an in-process JVM island.
PC plugins improve PC only.
Nix flake + cargo workspace + Mill + mill-ivy-fetcher provide reproducible
  build and dependency management.
```

The critical boundaries:

```text
SemanticDB plugin belongs to build/scalac, not this LS.
PC plugin belongs to this LS, but cannot write the persistent index.
Rust owns the process, the store, and all cross-boundary memory;
the island owns only the compiler, behind the flat C ABI.
```

The critical performance principle:

```text
Do not approximate semantic truth.
Precompute exact truth into mmap-friendly structures.
```

The critical engineering principle:

```text
No Nix flake, no build.
No mill-ivy-fetcher lock, no dependency update.
No JNIEnv, no second process, no SQLite (removed) —
one binary, one storage idiom, one boot symbol.
```

## 13. Correctness-case coverage (plan §18.1)

Every plan §18.1 references/rename correctness case is pinned by ≥1 real-scalac
test (fixtures compiled with `-Xsemanticdb`), ported from the retired Scala
suites into the Rust matrix suites: `crates/ls-engine/tests/references_matrix.rs`
(REF), `crates/ls-engine/tests/rename_matrix.rs` (REN), and
`crates/ls-semanticdb/tests/scalac_integration.rs` (SDB). The safe families
assert the exact reference span set and the exact rename edit spans; the unsafe
families (external, opaque, exported, override, synthetic) assert the exact
rejection reason; and the synthetics-skip families (case-class `copy`,
`derives`) assert the characterization that scalac emits them only in the
skipped synthetics payload (plan 4.3).

| plan §18.1 case | test(s) |
|-----------------|---------|
| export forwarder | SDB `export_forwarder_symbol_exists_with_no_definition_occurrence`, `export_forwarder_call_sites_join_the_originals_ref_group`, `export_forwarder_marks_rename_group_unsupported_symbol_family`; REF `export_forwarder_exact_set`; REN `exported_symbol_rejected` |
| inline def | REF `inline_def_exact_set`; REN `rename_inline_def_across_targets` |
| macro-generated (case-class copy) | SDB `synthetic_only_copy_has_no_definition_but_defined_owner`; REF `case_class_copy_call_site_only`; REN `synthetic_only_rejected` |
| macro-generated (`derives`) | SDB `derives_clause_case_class_defined_and_derived_given_synthetic_only` (characterization: the derived given is emitted only in the skipped synthetics payload, plan 4.3) |
| private member | REF `private_member_in_file_only`; REN `rename_private_method_in_file_only`, `rename_private_val_in_file_only` |
| local val / local def | REF `local_val_stays_in_document`, `nested_local_def_stays_in_document`; REN `rename_local_val_touches_only_its_document`, `rename_nested_local_def_only_its_document` |
| val member getter | REF `cross_file_val_member_exact`; REN `rename_val_member_cross_file` |
| var getter/setter | REF `var_getter_setter_definition_exact`; REN `rename_var_getter_setter_definition_together` |
| given / using | REF `given_references_by_name`, `given_references_exact_by_name_uses`; REN `rename_given_every_by_name_use` |
| top-level def/val | REF `top_level_def_and_val_exact`; REN `rename_top_level_def_cross_file`, `rename_top_level_val_cross_file` |
| opaque type | REF `opaque_type_references_exact`; SDB `opaque_type_carries_opaque_property_and_group_flagged_unsafe`; REN `opaque_type_rejected` |
| extension method | REF `extension_method_exact_set`; REN `rename_extension_method_across_targets` |
| external symbol reject | REN `external_library_symbol_rejected` |
| fresh-snapshot stale index | REN `fresh_snapshot_stale_cursor_document_rejected`, `stale_md5_edited_downstream_file_rejected_before_emit` |
| manifest / store recovery | `crates/ls-store/tests/store.rs` — `torn_manifest_tmp_recovers_old`, `torn_state_tmp_recovers_old`, `crash_after_state_before_manifest_recovers_old`, `state_generation_mismatch_rejected`, `manifest_segment_id_mismatch_rejected` |

The full retained-Scala-suite → Rust mapping (including the live-BSP end-to-end
rows and the island-boundary tests) is `docs/coverage-audit.md`.
