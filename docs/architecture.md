# Architecture

> Normative contract for contributors. Derived from `plan.md` sections 0–6, 9–13, 17, 21, 23.
> Contract types referenced below live in `modules/ls-index-model/src/ls/index/` (package `ls.index`).
> The on-disk postings binary format is specified separately in `docs/index-format.md`;
> the PC plugin boundary in `docs/plugin-spi.md`; the build/toolchain contract in `docs/nix-build.md`.

## 1. Positioning

`scala3-bsp-semantic-ls` is a special-purpose language server for **Scala 3 + BSP**
projects only. It does not pursue Metals-style generality; it pursues high accuracy and
high performance for exactly three global capabilities: `workspace/symbol`, whole-repo
`textDocument/references`, and cross-file `textDocument/rename`. All workspace-wide
semantic facts come exclusively from **scalac-generated SemanticDB**; SQLite and mmap
postings are nothing more than materialized indexes over that truth. The Scala 3
Presentation Compiler (PC) serves only interactive editing — completion, hover,
signature help, definition, dirty-buffer overlay, and PC-only plugin enhancements —
and never contributes to the persistent index.

## 2. Hard constraints

Language and runtime (plan 1.1):

```text
Java 25 only
Scala 3 only
JVM only
BSP only
Mill project build
Nix flake controlled toolchain
No Scala Native
No Scala 2
No Java language server
```

Java 25 is the only supported runtime. The project directly uses Java 25 FFM,
`MemorySegment` mmap, AOT cache, Compact Object Headers, and JFR profiling
(see `docs/nix-build.md`).

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
project, run only inside the PC worker, affect only PC request results, and must
never write SQLite, write mmap postings, or alter workspace-wide semantic truth.
When semantic truth is unavailable, requests fail with a typed `ls.index.LsError`
(e.g. `LsError.IndexUnavailable`, `LsError.StaleIndex`) rather than degrading to a
pretend-accurate answer.

## 3. Components

Plan section 5:

```text
Editor / IDE
  ⇅ LSP
scala3-bsp-semantic-ls, Java 25
  ├─ LSP Layer
  ├─ BSP Client
  ├─ Project Model
  ├─ SemanticDB Locator
  ├─ SemanticDB Ingestor
  ├─ SQLite FFM Metadata Store
  ├─ Mmap Postings Store
  ├─ Snapshot Manager
  ├─ Query Orchestrator
  ├─ PC Worker Manager
  ├─ PC Plugin Manager
  ├─ Rename Safety Engine
  ├─ Doctor
  └─ Bench / JFR Harness
```

Mill module mapping (see `build.mill`): `indexModel` → `modules/ls-index-model`
(shared contract), `semanticdb` → `modules/ls-semanticdb` (locator, protobuf parser,
normalization, group building), `sqliteFfm` → `modules/ls-sqlite-ffm`, `postings` →
`modules/ls-postings`, `bsp` → `modules/ls-bsp`, `pc` → `modules/ls-pc`, `rename` →
`modules/ls-rename` (references + rename engines), `doctor` → `modules/ls-doctor`,
`core` → `modules/ls-core` (LSP entry point, `ls.core.Main`), `bench` →
`modules/ls-bench`.

### 3.1 Main LS JVM responsibilities

```text
LSP protocol
BSP protocol
SemanticDB scanning / ingest
SQLite metadata
mmap postings snapshot
workspace symbol
references
rename
diagnostics forwarding
project state
index state
doctor
```

### 3.2 PC worker JVM responsibilities

```text
Scala 3 Presentation Compiler
PC compiler plugin loading
PC service plugin loading
synthetic source provider
dirty buffer overlay
completion / hover / signature / definition
prepareRename
```

A crashing user plugin must never corrupt the main index. All PC calls from the
LSP core route through the `ls.core.PcBackend` seam, which has two
implementations selected at boot:

- `InProcessPcBackend` — the presentation compiler runs in the LS JVM over
  `ls.pc.PcFacade`. Opt-in via `--in-process-pc`.
- `ForkedPcBackend` — the PC runs in an isolated child JVM (`ls.pc.PcWorkerMain`)
  proxied by `ls.pc.ForkedPcWorker` over a small JSON-RPC protocol
  (`ls.pc.PcWorkerApi`). The production default (also selectable with
  `--forked-pc`). A wedged or crashed child is killed and respawned with its
  targets/open buffers replayed, so a plugin crash is a latency blip, not index
  corruption. The doctor `PC` section reports "forked worker alive";
  `ForkedPcBackend.workerPid` is a fault-injection hook.

Process isolation is the production default: the flip from in-process to forked
landed once the real-BSP forked end-to-end acceptance test (a worker-kill respawn
over a live Mill BSP session) was green. `--in-process-pc` opts back into the same
JVM (e.g. AOT training, so the PC code paths are recorded into the cache).

### 3.3 Storage layout

```text
.scala3-bsp-semantic-ls/
  meta.sqlite            # metadata, manifest, FTS, dictionaries (WAL mode)
  meta.sqlite-wal
  meta.sqlite-shm
  postings/segment-N/    # immutable mmap postings segments (docs/index-format.md)
  snapshots/current.json
  pc/plugins/            # PC plugin staging
  pc/generated-sources/  # synthetic sources from PC plugins
```

SQLite holds metadata, the segment manifest, FTS5 workspace-symbol search, and the
symbol/document/target dictionaries. The references/rename hot path never touches a
SQLite occurrence table; occurrences live only in mmap postings.

### 3.4 LSP/BSP protocol-stack note (lsp4j 1.0.0)

`mtags-interfaces` (required by the Scala 3 presentation compiler artifact)
forces `org.eclipse.lsp4j` to 1.0.0 by coursier eviction; bsp4j 2.2.0-M2 was
built against lsp4j-jsonrpc 0.20.1 but runs correctly on 1.0.0 (proven by
`ls.doctor.BspLauncherCompatTest` and the `ls.core.LsEndToEndTest` suite).
Two lsp4j-1.0.0 constraints shape `ls.core.ScalaLs`:

1. One object may not implement `TextDocumentService` and `WorkspaceService`
   together (both declare `diagnostic`); the server uses `@JsonDelegate`
   inner service objects instead.
2. Scala 3's default `-Xmixin-force-forwarders` copies lsp4j's
   `@JsonRequest`/`@JsonNotification` annotations onto synthetic forwarders,
   which the jsonrpc endpoint scanners reject as duplicate RPC methods. The
   `core` module compiles with `-Xmixin-force-forwarders:false`; any Scala
   class implementing lsp4j interfaces outside `core` needs the same flag.

## 4. Query orchestrator: three paths, three consistency levels

Every request is routed through exactly one of three paths (plan 10):

```text
IndexPath:
  SQLite + mmap postings. The normal hot path.

RawSemanticDBPath:
  When the index is stale/missing for a document, read the .semanticdb file
  directly, validate md5, answer from it, and write the result through into
  SQLite + postings.

PCPath:
  Dirty buffers and interactive editing features. Never persisted.
```

Consistency levels are the enum `ls.index.ConsistencyLevel`:

| Level | Enum case | Used by | Meaning |
|---|---|---|---|
| Best effort | `ConsistencyLevel.BestEffort` | `workspace/symbol` | Answer from the current snapshot even if some documents are stale. |
| Fresh preferred | `ConsistencyLevel.FreshPreferred` | `textDocument/references` | Prefer fresh facts; refresh stale docs via RawSemanticDBPath when cheap; dirty buffers may add a PC overlay, clearly non-persistent. |
| Fresh required | `ConsistencyLevel.FreshRequired` | `textDocument/rename` | Must compile first, ingest fresh SemanticDB, and answer only from a fresh snapshot. No fresh truth ⇒ reject (`LsError.CompileFailed`, `LsError.StaleIndex`). |

## 5. Snapshots

### 5.1 Snapshot model and retain/release

The snapshot manager publishes an `AtomicReference[IndexSnapshot]`. `ls.index.IndexSnapshot`
is an immutable, reference-counted view over one postings generation plus its snapshot
dictionaries. Readers must bracket every query:

```scala
IndexSnapshot.using(currentSnapshot.get()) { snap =>
  // query snap
}
```

`retain()` returns `false` when the snapshot is already closed (the loan pattern in
`IndexSnapshot.using` throws in that case); `release()` decrements the count. The
backing mmap `Arena` closes only when the count drops to zero *and* the snapshot has
been superseded. Segments are immutable; there is no in-place update and no lock on
the read path.

### 5.2 Write pipeline (plan 9.2)

```text
 1. BSP compile succeeds.
 2. SemanticDB watcher finds changed files.
 3. RawSemanticDBPath parses TextDocuments (ls.index.NormalizedDocument).
 4. Validate md5.
 5. SQLite transaction interns symbols and updates metadata.
 6. Build new postings segment in temp directory.
 7. fsync segment.
 8. SQLite manifest transaction marks segment active.
 9. mmap new segment.
10. publish new snapshot.
11. old snapshot released after readers finish.
```

A reader can never observe a partially written segment: the segment becomes reachable
only through the manifest transaction (step 8) and the snapshot publish (step 10).

### 5.3 Epoch filtering (plan 9.3)

Every occurrence record in postings carries a `doc_epoch`. A query result is valid
only if:

```text
occ.doc_epoch == snapshot.epochOf(docOrd)
```

(`IndexSnapshot.epochOf` exposes the current per-document epoch; the epoch of each
occurrence is delivered through the `docEpoch` parameter of
`ls.index.OccurrenceSink.accept`.) Stale occurrences left behind in older segments are
therefore ignored on read, and a background compactor physically removes them later.
Postings scans push results through `OccurrenceSink`, a primitive-argument callback,
so the hot path performs no per-occurrence allocation.

## 6. ID layering (plan 6.1)

Two strictly separated identifier layers, both defined as opaque types in
`ls/index/ids.scala`:

| Concept | Stable id (persistent, SQLite) | Snapshot ordinal (dense `Int`, one snapshot only) |
|---|---|---|
| Symbol | `ls.index.SymbolId` (`Long`) | `ls.index.SymbolOrd` |
| Document | `ls.index.DocId` | `ls.index.DocOrd` |
| Build target | `ls.index.TargetId` | `ls.index.TargetOrd` |
| Reference group | `ls.index.RefGroupId` | `ls.index.RefGroupOrd` |
| Rename group | `ls.index.RenameGroupId` | `ls.index.RenameGroupOrd` |

Rules:

- Stable ids live in SQLite and survive snapshots. They are the only ids allowed in
  persistent storage.
- Snapshot ordinals are dense ints valid **only within one `IndexSnapshot`**, assigned
  at snapshot build time for O(1) array lookup on the query hot path (e.g.
  `refGroupIndex[refGroupOrd]` — no binary search). Never persist an ordinal; never
  carry an ordinal across a snapshot swap.
- Conversion happens only through snapshot dictionaries
  (`IndexSnapshot.targetIdOf`, `targetOrdOfId`, `docOrdOf`, `symbolOrdOf`, …).

Symbol identity follows the SemanticDB spec: a global symbol is unique per universe;
a local symbol is meaningful only together with its document. This is encoded by
`ls.index.SymbolKey(semanticSymbol, localDoc: Option[DocId])` with constructors
`SymbolKey.global` / `SymbolKey.local`.

## 7. Alias groups: reference group vs rename group (plan 6.2)

Two group notions are materialized separately and must never be conflated:

```text
reference group  (RefGroupId / RefGroupOrd)   — what "references" merges
rename group     (RenameGroupId / RenameGroupOrd) — what "rename" may edit together
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

Initial policy:

- **references**: build the most complete exact alias group possible.
- **rename**: group conservatively; reject unsafe families outright (recorded in the
  group's `unsafeReasonMask`, e.g. `UnsafeReason.UnsupportedSymbolFamily`,
  `UnsafeReason.OverrideFamily`).

Opaque type (v1 behavior): an `opaque type T` and its companion `object T` merge into
one group for *references* (references find the type, the companion, and every use),
but *rename* is conservatively **rejected** (`UnsafeReason.OpaqueType`) — renaming an
opaque type together with its companion and uses cannot be proven safe in v1. This
matches the `override` / `exported` / synthetic families, which also reject. The
opaque property is read from SemanticDB (`SymProps.Opaque`) at ingest.

## 8. References flow (plan 12)

```text
1. symbol-at-cursor
2. symbol -> ref_group
3. allowed target pruning
4. mmap ref postings query
5. optional dirty buffer PC overlay
6. dedupe
7. return LSP Location[]
```

### 8.1 symbol-at-cursor

```text
dirty file:           PCPath wins.
clean fresh file:     doc-postings mmap wins (IndexSnapshot.symbolAt -> OccurrenceHit).
index stale/missing:  RawSemanticDBPath reads .semanticdb, validates md5, writes through.
```

### 8.2 Target-graph exact pruning

The BSP build target graph (`ls.index.TargetGraph`) gives dependency edges. For a
symbol defined in target `T`, the allowed reference targets are
`TargetGraph.reverseDependencyClosure(T)` = `T` plus all targets transitively
depending on `T`. This is an **exact upper bound derived from the build graph, not an
approximation**. Per snapshot it is converted to a `ls.index.TargetBitset` over target
ordinals — an exact membership bitset, explicitly not a probabilistic filter — which
is also what block-level skip metadata intersects against
(`TargetBitset.intersectsWords`).

### 8.3 Flow in terms of the real snapshot API

```scala
IndexSnapshot.using(currentSnapshot.get()) { snap =>
  val hit      = snap.symbolAt(docOrd, line, character) // OccurrenceHit or LsError.NoSymbolAtCursor
  val group    = snap.refGroupOf(hit.symbolOrd)
  val defT     = snap.definitionTargetOf(hit.symbolOrd)
  val allowed  = /* TargetBitset from TargetGraph.reverseDependencyClosure(defT) */
  snap.scanReferences(group, allowed, sink)             // epoch-checked via sink
  if includeDeclaration then snap.scanDefinitions(group, sink)
  // + optional PC dirty-buffer overlay, then dedupe, then LSP Location conversion
}
```

The dirty-buffer overlay fan-out is **group-keyed**: references query the overlay
for *every* member symbol of the cursor's ref group (`IndexSnapshot.refGroupSymbols`),
not just the symbol under the cursor, so an overlay occurrence keyed to a companion
member, `apply` forwarder, or getter/setter alias is still surfaced. The fan-out is
gated on `DirtyBufferOverlay.contributesOccurrences`; the production `PcOverlay`
(`ls.core`) leaves it `false` and its `occurrencesOf` is a deliberate no-op — the PC
worker contributes symbol-at-cursor for dirty files but no extra reference
occurrences yet — so the group-keyed query costs nothing in production and is
exercised only by test overlays that opt in.

`references(includeDeclaration = false)` reads only reference postings;
`includeDeclaration = true` additionally reads definition postings. Occurrence roles
mirror SemanticDB (`ls.index.Role.Reference` / `Role.Definition`); per-occurrence
flags are the exact bits of `ls.index.OccFlags`
(`Definition`, `Editable`, `Generated`, `Readonly`, `Synthetic`). Positions flow
through the sink in the columnar packed encoding `ls.index.Span.pack`
(`line << 12 | char`), unpacked into `ls.index.Span` / `ls.index.Loc` only at the LSP
conversion boundary. Positions are zero-based line/character with exclusive end,
matching both SemanticDB `Range` and LSP `Position` semantics (`ls.index.Pos`).

## 9. Rename flow and safety (plan 13)

Rename runs at `ConsistencyLevel.FreshRequired`:

```text
 1. PC prepareRename
 2. dirty buffer check
 3. BSP buildTarget/compile over the affected target domain
 4. ingest fresh SemanticDB
 5. publish fresh snapshot
 6. symbol-at-cursor
 7. symbol -> rename_group (IndexSnapshot.renameGroupOf)
 8. read editable rename postings (IndexSnapshot.scanRenameEdits)
 9. safety validation (IndexSnapshot.renameProfileOf)
10. produce WorkspaceEdit
```

(BSP explicitly documents `buildTarget/compile` before `textDocument/rename` to
ensure workspace sources typecheck and are up to date.)

### 9.1 Safety rules (plan 13.1) — all must hold

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
(`LsError.CompileFailed`, `LsError.StaleIndex`). The rest are precomputed at ingest
into the rename group's unsafe-reason bitmask, `ls.index.UnsafeReason`:

| Bit | Rule enforced |
|---|---|
| `UnsafeReason.External` | edits must stay inside workspace sources |
| `UnsafeReason.GeneratedOccurrence` | no generated sources by default |
| `UnsafeReason.ReadonlyOccurrence` | no readonly sources |
| `UnsafeReason.DependencySource` | no dependency sources |
| `UnsafeReason.PcOnly` | no PC-only symbol |
| `UnsafeReason.SyntheticOnly` | no synthetic-only occurrence |
| `UnsafeReason.OverrideFamily` | no unsafe override family by default |
| `UnsafeReason.SharedSourceDisagreement` | shared-source targets must agree on the rename group |
| `UnsafeReason.UnsupportedSymbolFamily` | conservative rename grouping (e.g. apply/unapply, export) |

### 9.2 RenameProfile

`ls.index.RenameProfile` is precomputed at ingest and consulted at request time:

```scala
final case class RenameProfile(
    isLocal: Boolean,                 // local symbol (document-scoped)
    isExternal: Boolean,              // defined outside the workspace
    hasGeneratedOccurrences: Boolean, // some occurrence lives in a generated source
    hasReadonlyOccurrences: Boolean,  // some occurrence lives in a readonly source
    hasOverrideFamily: Boolean,       // participates in an override family
    hasCompanion: Boolean,            // has a companion class/object
    editableOccurrenceCount: Int,     // occurrences the rename would actually edit
    unsafeReasonMask: Long            // OR of UnsafeReason bits; 0 == safe
)
```

Request-time decision is a single integer test: `unsafeReasonMask != 0` ⇒ reject with
concrete reasons via `UnsafeReason.explain(mask)` wrapped in
`LsError.RenameRejected(reasons)`. `RenameProfile.isSafe` encodes the check;
`RenameProfile.empty` is the all-clear baseline. Rename edits are generated only from
the **editable** rename postings (occurrences flagged `OccFlags.Editable`), which by
construction exclude readonly/generated/dependency sources.

## 10. Performance design checklist (plan 17)

Mandatory from the first version:

```text
 1. dense snapshot ordinals
 2. exact ref_group / rename_group
 3. role-separated postings
 4. editable rename postings
 5. doc-postings interval index
 6. target graph exact pruning
 7. block-level exact skip metadata
 8. immutable segments
 9. snapshot retain/release
10. batch SemanticDB ingest
11. batch symbol interning
12. SQLite FTS5 workspace symbol
13. Java 25 FFM SQLite binding
14. Java 25 MemorySegment mmap
15. JFR benchmark harness
16. compactor
```

Forbidden performance shortcuts:

```text
Bloom filter for correctness
source token grep
syntax-only references
PC-generated persistent global index
```

## 11. Risks and mitigations (plan 21)

| Risk | Mitigation |
|---|---|
| BSP server produces no SemanticDB | Mark target `IndexUnavailable` (`LsError.IndexUnavailable`); Doctor reports what is missing; never fall back to an approximate index. |
| SemanticDB is stale | md5 check; per-document epoch check; compile-before-rename; stale target status. |
| PC plugin diverges from the real build | SemanticDB remains truth; PC plugins affect editing only; PC-only symbols can never be globally renamed (`LsError.PcOnlySymbol`). |
| mmap segment / SQLite manifest inconsistency | atomic rename; fsync; checksum; manifest transaction; startup recovery. |
| Rename edits the wrong thing | `FreshRequired`; editable postings only; `RenameProfile`; shared-source consistency check; unsafe-family rejection. |
| Nix/Mill lock drift | CI checks `nix/ivy-lock.nix` (`scripts/check-ivy-lock.sh`); PRs must include lock updates; `nix flake check` as gate. |

## 12. Final design principles (plan 23)

```text
BSP provides project facts.
scalac SemanticDB provides semantic facts.
SQLite provides metadata, FTS, manifest, and dictionaries.
mmap postings provide exact high-speed reference/rename lookup.
Scala 3 PC provides interactive editing.
PC plugins improve PC only.
Nix flake + Mill + mill-ivy-fetcher provide reproducible build and dependency management.
```

The two critical boundaries:

```text
SemanticDB plugin belongs to build/scalac, not this LS.
PC plugin belongs to this LS, but cannot write persistent index.
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
No Java 25, no runtime support.
```

## 13. Correctness-case coverage (plan §18.1)

Every plan §18.1 references/rename correctness case is pinned by ≥1 real-scalac test
(fixtures compiled with `-Xsemanticdb`). The safe families assert the exact reference
span set and the exact rename edit spans; the unsafe families (external, opaque,
exported, override, synthetic) assert the exact rejection reason; and the
synthetics-skip families (case-class `copy`, `derives`) assert the characterization
that scalac emits them only in the skipped synthetics payload (plan 4.3).

| plan §18.1 case | test(s) |
|-----------------|---------|
| export forwarder | `ScalacIntegrationSuite` export-shape/ref-group/rename-group cases; `ReferencesAndQuerySuite` "export forwarder references are exactly the definition and the forwarder call"; `RenameSuite` "rename of an exported symbol is rejected with the exported-symbol reason" |
| inline def | `ReferencesAndQuerySuite` "inline def references are exactly the definition and both call sites"; `RenameSuite` "rename an inline def edits its definition and every call site across targets" |
| macro-generated (case-class copy) | `ScalacIntegrationSuite` "synthetic-only case-class copy…"; `ReferencesAndQuerySuite` "case-class copy references resolve to the copy symbol call site only"; `RenameSuite` "synthetic-only symbol is rejected with the synthetic-only reason" |
| macro-generated (`derives`) | `ScalacIntegrationSuite` "derives clause: the case class is defined and the derived given is synthetic-only" (characterization: the derived given is emitted only in the skipped synthetics payload, plan 4.3) |
| private member | `ReferencesAndQuerySuite` "private member references are exactly the in-file definition and uses"; `RenameSuite` "rename a private method/val edits its definition and in-file uses only" |
| local val / local def | `ReferencesAndQuerySuite` "local val…" / "nested local def references stay inside the document"; `RenameSuite` "rename local val touches only its document" / "rename a nested local def touches only its document" |
| val member getter | `ReferencesAndQuerySuite` "cross-file val member references are exactly the definition and cross-file use"; `RenameSuite` "rename a val member edits its definition and cross-file uses" |
| var getter/setter | `ReferencesAndQuerySuite` "var getter, setter, and definition references are exactly all value tokens"; `RenameSuite` "rename var renames getter, setter site and definition together" |
| given / using | `ReferencesAndQuerySuite` "given references are exactly the by-name uses including the using-clause site"; `RenameSuite` "rename a given edits its definition and every by-name use across files and targets" |
| top-level def/val | `ReferencesAndQuerySuite` "top-level def and val references are exactly their definitions and cross-file uses"; `RenameSuite` "rename a top-level def/val edits its definition and cross-file uses" |
| opaque type | `ReferencesAndQuerySuite` "opaque type references are exactly the type, companion, and all in-file uses"; `ScalacIntegrationSuite` "opaque type carries the Opaque property and its rename group is flagged unsafe"; `RenameSuite` "rename of an opaque type is rejected (conservative policy)" |
| extension method | `ReferencesAndQuerySuite` "extension method references are exactly the definition and both call sites"; `RenameSuite` "rename an extension method edits its definition and call sites across targets" |
| external symbol reject | `RenameSuite` "rename of an external library symbol is rejected as outside the workspace" |
| fresh-snapshot stale index | `RenameMutationSuite` "fresh-snapshot stale index: the cursor document itself is edited after compile" |
| manifest → missing segment recovery | `BootstrapRecoverySuite` "a manifest pointing at a deleted segment degrades gracefully and heals on the next ingest" |
