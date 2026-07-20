# Deployment Guide

> Practical operator guide: **package → wire into BSP + an editor → verify by
> hand.** It ties together the normative contracts in
> [nix-build.md](nix-build.md) (build/packaging) and
> [architecture.md](architecture.md) (subsystem behavior), and is derived from
> the real `flake.nix`, `nix/`, `scripts/`, and `crates/` sources — every flag,
> path, and command below is quoted from the tree, not invented.

The server is a **native Rust binary** (the cargo workspace under `crates/`,
built with crane). The only JVM components in the product are the
presentation-compiler island artifacts built by Mill: the PC-host agent jar (a
`-javaagent` premain assembly) and the zaozi PC compiler-plugin jar. The JVM is
embedded lazily — it boots in-process on the first presentation-compiler query,
so an index-only session runs with **zero JVM in the process**. Per the v2
decision record (plan-rust.md §0), the previous Scala/JVM core was replaced
wholesale; there is no AOT training and no forked PC worker process.

Contents:

1. [Prerequisites](#1-prerequisites)
2. [Packaging](#2-packaging)
3. [Server CLI](#3-server-cli)
4. [BSP + language-server integration](#4-bsp--language-server-integration)
5. [Manual testing / verification](#5-manual-testing--verification)
6. [Troubleshooting](#6-troubleshooting)

---

## 1. Prerequisites

### 1.1 Support envelope: Linux only

The flake builds for `x86_64-linux` and `aarch64-linux` **only**. macOS is
explicitly unsupported — a deliberate decision, not an omission: the embedded
libjvm boundary (`dlopen` + `JNI_CreateJavaVM` + `/proc/self/maps` assertions)
is exercised and supported on Linux exclusively, and `nix/package.nix` declares
`platforms = lib.platforms.linux`.

### 1.2 Toolchain

Everything is provided by the flake (see [nix-build.md §1](nix-build.md)):

```text
Rust (stable, nixpkgs)   crane-built cargo workspace; the deployable server binary
Java 25                  island-only: runs the embedded presentation compiler
Scala 3.8.4              pinned in build.mill (Deps.scalaVer); the bundled PC version
Mill 1.1.2               builds the island jars (from mill-ivy-fetcher's mill-overlay)
Nix >= 2.28              mill-ivy-fetcher requirement
```

`nix develop` is the only entry point for building, locking, and testing. The
repo-root `./mill` script is a thin launcher: it `exec`s `mill` from `PATH` and
refuses to bootstrap outside the dev shell. All commands below assume you
either ran `nix develop` first or prefix with `nix develop -c`.

The dev shell also exports the PC island boot inputs (`LS_LIBJVM`,
`PC_HOST_AGENT_JAR`, `LS_PC_TARGET_CLASSPATH`, `ZAOZI_PCPLUGIN_JAR`), so a
dev-built server and the live PC tests can boot the island without the packaged
wrapper. See [nix-build.md §6](nix-build.md) for the full dev-shell contract.

---

## 2. Packaging

### 2.1 The deployable artifact (`nix build`)

```bash
nix build .#default
./result/bin/scala3-bsp-semantic-ls --version   # -> scala3-bsp-semantic-ls 0.1.0
```

`packages.<system>.default` wraps the crane-built `ls-server` binary and ships
the island artifacts alongside it:

```text
result/bin/scala3-bsp-semantic-ls                                # makeWrapper launcher around the Rust ls-server binary
result/share/scala3-bsp-semantic-ls/pc-host-agent.jar            # PC island host agent (-javaagent premain assembly; mill pcHost.assembly)
result/share/scala3-bsp-semantic-ls/zaozi-pcplugin.jar           # shipped PC compiler plugin (scalac -Xplugin; zaozi bundle-field go-to/hover)
result/share/scala3-bsp-semantic-ls/default-plugin-schema.json   # JSON schema for pc-plugins.json (data, not required to run)
```

The wrapper bakes exactly three defaults, all via `--set-default` (so any value
already in the caller's environment wins):

| Wrapper setting                          | Purpose                                                       |
|------------------------------------------|---------------------------------------------------------------|
| `--set-default JAVA_HOME <jdk25>`        | the Java home the embedded island boots from, if nothing else is configured |
| `--set-default PC_HOST_AGENT_JAR <share/…/pc-host-agent.jar>` | the island host agent assembly loaded as `-javaagent` |
| `--set-default LS_SCALAFMT <scalafmt>/bin/scalafmt` | the scalafmt CLI `textDocument/formatting` shells out to (§4.9) |

There are no JVM launch flags in the wrapper — the binary is native. The island
JVM's own flags (`--enable-native-access=ALL-UNNAMED`,
`-XX:+UseCompactObjectHeaders`, the `-javaagent`) are composed inside the
server at island boot, not by the wrapper.

Package facts: `pname = scala3-bsp-semantic-ls`, `version = 0.1.0`,
`mainProgram = scala3-bsp-semantic-ls`, platforms **Linux only**.

### 2.2 Java-home resolution (config > env > nix-baked)

The embedded island needs a `libjvm`; it is located at
`<javaHome>/lib/server/libjvm.so`. Resolution precedence, first hit wins:

1. **Workspace config** — `<workspaceRoot>/.scala3-bsp-semantic-ls/config.json`
   with `{"javaHome": "/abs/path/to/jdk"}`.
2. **Environment** — `LS_LIBJVM` (an **exact** libjvm path, not a Java home),
   else `JAVA_HOME`.
3. **Nix-baked** — the wrapper's `--set-default JAVA_HOME` /
   `PC_HOST_AGENT_JAR`, which by construction only apply when the tiers above
   set nothing.

With no tier at all, the first presentation-compiler query fails with a typed
error (`no Java home for the PC island: set javaHome in
.scala3-bsp-semantic-ls/config.json, or LS_LIBJVM / JAVA_HOME in the
environment`); index-backed features are unaffected. The JVM boots **lazily** on
the first PC query — starting the server, indexing, and answering
references/rename/workspace-symbol never touch Java at all.

### 2.3 Other flake packages

| Package                     | Contents                                                              |
|-----------------------------|-----------------------------------------------------------------------|
| `.#rust-workspace`          | the crane-built cargo workspace (`bin/ls-server`, plus the spike/bench binaries) |
| `.#pc-host-agent-jar`       | the island host agent assembly on its own                             |
| `.#zaozi-pcplugin-jar`      | the zaozi PC compiler plugin jar on its own                           |
| `.#spike-agent-jar`         | the embedded-JVM boundary-spike agent jar (dev/verification artifact) |
| `.#mill`, `.#mill-ivy-fetcher` | the pinned Mill and `mif` used by the ivy-lock workflow            |
| `.#zaozi-src`               | the pinned + patched zaozi source tree (real-repo validation input)   |

### 2.4 Dev / raw-binary iteration

For iteration, build and run the server straight from cargo inside the dev
shell:

```bash
nix develop -c cargo build -p ls-server
nix develop -c cargo run -p ls-server -- --version
```

The dev shell's exported PC env (§1.2) stands in for the packaged wrapper's
baked defaults, so a cargo-built server can boot the island too.

### 2.5 Dependency locks & offline guarantee

Two locks, both committed:

- **`Cargo.lock`** — the Rust dependency closure; crane vendors it so the
  package build is fully offline.
- **`nix/ivy-lock.nix`** — the Maven/ivy closure for the **island modules
  only** (the Mill build). After any `build.mill` dependency change:

```bash
nix develop -c ./scripts/regen-ivy-lock.sh     # regenerate (determinism guards)
nix develop -c ./scripts/check-ivy-lock.sh     # CI gate: lock == build.mill
```

See [nix-build.md §4](nix-build.md) for the normative locking details and
[nix-build.md §5](nix-build.md) for the full `nix flake check` suite.

---

## 3. Server CLI

The binary is primarily the stdio LSP server; it has three offline modes. All
of them work pre-bootstrap and boot **no JVM**.

| Invocation                  | Meaning                                                                            |
|-----------------------------|------------------------------------------------------------------------------------|
| *(no arguments)*            | start the stdio LSP server                                                         |
| `--version`                 | print `scala3-bsp-semantic-ls 0.1.0` and exit                                      |
| `--doctor [dir] [--json]`   | print the offline doctor report for `dir` (default `.`) and exit; `--json` emits the structured object |
| `dump [dir]`                | print a read-only inspection of the on-disk index store at `dir` (manifest, active segment header, workspace-state) and exit |

**Anything else is a usage error** (non-zero exit, message on stderr) — never a
silent server start. That includes the v1 flags for AOT training and PC-backend
selection, which were deleted with the Scala core and now parse as unknown
arguments (the embedded island is the only PC backend).

`--doctor`/`dump` directories are resolved like the server's workspace root:
made absolute against the process cwd, then lexically normalized. `dump` on a
workspace with no store reports `no store at this workspace root` gracefully.

**stdout discipline:** in server mode, stdout carries only framed JSON-RPC
protocol; all logs and diagnostics go to stderr.

---

## 4. BSP + language-server integration

### 4.1 The SemanticDB prerequisite (mandatory)

Workspace-wide answers come **only** from scalac-generated SemanticDB. Every
Scala 3 build target the server indexes **must** be compiled with SemanticDB
enabled:

```text
-Xsemanticdb                 (alias -Ysemanticdb) — REQUIRED; emits .semanticdb per source
-sourceroot <workspaceRoot>  — set it to the workspace root (the recommended default,
                               and what the sample uses). The server reads each target's
                               -sourceroot from buildTarget/scalacOptions and maps
                               file:// <-> semanticdb URIs relative to it; the workspace
                               root keeps those URIs workspace-relative.
```

`.semanticdb` files land at `<targetroot>/META-INF/semanticdb/<source-rel>.semanticdb`,
where `<targetroot>` defaults to the class-output dir (or `-semanticdb-target:<path>`).

Mill configuration — set it in a shared `ScalaModule` trait (as
`it/sample-workspace` does):

```scala
// scalacOptions for every indexable module:
def scalacOptions = Seq("-Xsemanticdb", "-sourceroot", mill.api.BuildCtx.workspaceRoot.toString)
```

> This is Scala 3 only. `-Xsemanticdb` is built into the Scala 3 compiler (no
> separate plugin). Only Scala 3 targets are indexed — Scala 2 targets are
> ignored entirely.

**SemanticDB is a hard requirement, not graceful.** A live Scala 3 target with
no SemanticDB is an error surfaced two ways:

- the doctor's BSP section lists it under the SemanticDB-coverage error
  (`recompile with -Xsemanticdb`), and
- every document/position request on such a source is rejected with
  `<uri> has no SemanticDB output; every source must be compiled with -Xsemanticdb`
  — there is **no** presentation-compiler fallback.

It does **not** fail boot: the workspace still reaches `Ready` and indexable
targets still index.

> **Expect a permanent `mill-build` entry in the coverage error.** Mill always
> exposes its own build definition as a Scala 3 target (`.../mill-build`)
> compiled without `-Xsemanticdb`, so a clean Mill workspace *always* shows at
> least `mill-build` there. This is normal — only worry about *your* modules
> appearing there.

### 4.2 Install a BSP connection

The server does not run a build itself; it speaks BSP to one. For Mill, install
the connection once per workspace (writes `.bsp/mill-bsp.json`):

```bash
cd /path/to/your/workspace
mill mill.bsp.BSP/install     # -> .bsp/mill-bsp.json
mill __.compile               # optional pre-compile to emit SemanticDB up front
```

### 4.3 How discovery & bootstrap work

On the LSP `initialized` notification the server bootstraps asynchronously:

1. **Discover** — scan `<workspaceRoot>/.bsp/*.json`, parse each, drop files
   missing `name`/`argv`, sort deterministically by BSP server name (ties by
   file name), pick the first.
2. **Launch** — run the picked file's `argv` verbatim as a child process
   (cwd = workspace root; for Mill this is `mill --bsp`), and talk BSP over its
   stdio.
3. **Handshake** — `build/initialize` (languageIds `["scala"]`) →
   `build/initialized`.
4. **Load model** — `workspace/buildTargets` (filtered to Scala 3) →
   `buildTarget/sources` + `buildTarget/scalacOptions`. Compiles go through
   `buildTarget/compile`.
5. **Ingest** — parse SemanticDB for indexable targets into the immutable-segment
   store under `.scala3-bsp-semantic-ls/` (§4.7).

A server-initiated `buildTarget/didChange` reloads the project model over the
retained session (no rediscovery, no relaunch); a reload failure keeps serving
the previous ready snapshot.

> **No `.bsp` connection is a bootstrap failure, not a degraded mode.** With no
> usable connection file the bootstrap fails with `no build server connection
> found (the no-BSP warm-restart mode is deferred)`. The process stays up:
> requests answer with typed `workspace is bootstrap failed: …` errors and the
> doctor still renders, but nothing is served from the persisted index. (v1's
> BSP-less recovered-index serving was deliberately trimmed in the rewrite —
> faithful recovered serving needs the target dependency graph, which the
> persisted segment does not carry.)

> **BSP request timeout is 30 s.** A very large *first* compile over BSP can
> exceed it and surface as a request-timeout error; pre-compiling with
> `mill __.compile` before starting an editor session avoids the cold-compile
> spike.

### 4.4 Wire the server into an editor

The server is a **generic stdio LSP server** — no editor-specific launcher
ships in this repo, so integrate it as you would any stdio LSP:

- **Command:** the wrapped binary `scala3-bsp-semantic-ls` (from
  `nix build .#default`, a `nix profile install`, or the dev-shell cargo
  build). No CLI args for normal use.
- **Transport:** stdin = requests, **stdout = framed JSON-RPC only**, stderr =
  logs. A client must not write to the child's stdout, and must read logs from
  stderr.
- **Language / filetype:** `scala`.
- **Root:** send `initialize` with `rootUri` pointing at the workspace root
  (where `.bsp/` lives). With no `rootUri`/`workspaceFolder`, bootstrap fails
  with a `no workspace root` detail rather than guessing.
- **Warm-up:** heavy work runs asynchronously after `initialized`. Until the
  workspace is `Ready`, requests return typed "workspace is …" errors — never
  crashes. Clients must tolerate this brief warm-up.

Illustrative Neovim (`nvim-lspconfig` custom config — adjust paths):

```lua
-- Illustrative only; no official client config ships in-repo.
local configs = require("lspconfig.configs")
if not configs.scala3_bsp_semantic_ls then
  configs.scala3_bsp_semantic_ls = {
    default_config = {
      cmd = { "/abs/path/to/result/bin/scala3-bsp-semantic-ls" },
      filetypes = { "scala" },
      root_dir = require("lspconfig.util").root_pattern(".bsp", "build.mill"),
    },
  }
end
require("lspconfig").scala3_bsp_semantic_ls.setup({})
```

VS Code / Emacs `eglot` / any LSP client work the same way: register a stdio
server for `scala` whose command is the binary and whose root is the workspace.

### 4.5 Capabilities advertised

| Advertised                                      | Notes                                        |
|--------------------------------------------------|----------------------------------------------|
| Text sync = **Incremental**                      | ranged `contentChanges` folded server-side; `positionEncoding: utf-16` advertised |
| Completion (`resolveProvider`, trigger `.`)      | served by the embedded presentation compiler |
| Hover; SignatureHelp (triggers `(` `,`)          |                                              |
| Definition; TypeDefinition                       |                                              |
| References                                       | whole-repo, from the SemanticDB index        |
| Rename (with `prepareProvider`)                  | cross-file                                   |
| DocumentHighlight                                |                                              |
| workspace/symbol                                 | over the index (+ PC-only unsaved symbols)   |
| DocumentSymbol                                   | the index-backed NESTED outline (always `DocumentSymbol[]`; the client's `hierarchicalDocumentSymbolSupport` is not negotiated and the flat `SymbolInformation[]` fallback is not implemented). Works for closed files; a dirty buffer answers index truth (the outline lags until save). The index stores definition NAME spans only, so `range == selectionRange` on every node (spec-legal; breadcrumb enclosure degrades to the name line) |
| Implementation                                   | index-backed METHOD override families: the def sites of the cursor method's overriders (index candidates verified against the SemanticDB `overridden_symbols` edges of their defining documents), visibility-pruned like definition. A trait/class TYPE symbol answers `[]` — neither SemanticDB nor the index carries subtype edges |
| InlayHint (`resolveProvider: false`)             | presentation-compiler hints over the open buffer; the server's fixed default category set (inferred types, implicit params, by-name params, implicit conversions, named params — type params / xray / pattern-match / closing labels off); every hint ships complete, there is no `inlayHint/resolve` |
| CodeAction (kinds: `quickfix`, `refactor.rewrite`, `refactor.extract`, `refactor.inline`; `resolveProvider: false`) | the assembly layer over the presentation-compiler ops: the missing-symbol import quickfix (dotty `Not found: (type )?X` diagnostics → auto-import candidates) plus the refactor probes (insert inferred type, implement all members, convert to named arguments, inline value, create method from usage, extract method — non-empty selection only, convert to named lambda parameters). EAGERLY RESOLVED: every offered action ran its PC op during assembly and carries its `WorkspaceEdit` inline — a refused (`DisplayableException`-as-data) or empty op is dropped, and there is no `codeAction/resolve` and no executeCommand round trip. `[]` for a buffer the PC does not hold; capped at 20 actions |
| SelectionRange                                   | pure syntax over the open buffer (no SemanticDB needed); `null` for a buffer the PC does not hold |
| FoldingRange                                     | the parser-only folding walker over the open buffer (kinds `comment`/`imports`/`region`); `[]` for a buffer the PC does not hold |
| DocumentFormatting                               | the scalafmt COMMAND LINE over the **open buffer** (`scalafmt --stdin --config <ws>/.scalafmt.conf --non-interactive`, cwd = workspace root), answered as MINIMAL `TextEdit`s (the `dissimilar` diff→edits fold, UTF-16 positions over the original text; an already-formatted buffer answers `[]`). Requires a workspace-root `.scalafmt.conf` with a pinned `version` (typed error without one); a not-open file is a typed error; the LSP `options` field is ignored — `.scalafmt.conf` is the single style authority. 10 s deadline (kill + typed error); a non-zero exit is a typed error carrying the stderr tail. Binary resolution + offline stance in §4.9 |
| SemanticTokens (`full: {delta: true}`, `range: true`) | presentation-compiler symbol tokens over the open buffer; the legend is EXACTLY the PC-vendored `scala.meta.internal.pc.SemanticTokens` lists (23 types / 10 modifiers). Advertised unconditionally (every mainstream client sends the standard token capability; one that lacks it simply never asks). Every `/full` response carries a `resultId` (monotonic per-URI counter) and the server caches the encoded stream (latest per URI, dropped on didClose, LRU across URIs cap 32); `textDocument/semanticTokens/full/delta` answers the minimal single-splice edit list against the cached base (the rust-analyzer prefix/suffix diff), or a FULL result on an unknown/stale `previousResultId` (spec-legal union). `/range` responses carry no `resultId` (a range slice is never a delta base). `null` for a buffer the PC does not hold. NOTE: a client that auto-requests semantic tokens on open (VS Code, nvim 0.10+) thereby issues a PC query, which boots the embedded JVM island |
| executeCommand (4 command IDs — §4.6)            |                                              |
| **Diagnostics: push-only**                       | BSP `build/publishDiagnostics` is forwarded live as `textDocument/publishDiagnostics` (per-URI merge across targets, per-target reset); **no** pull `diagnosticProvider`. PLUS live-typing diagnostics: on `didChange` a debounced (300ms) presentation-compiler pull publishes secondary diagnostics under the source tag `scala3-pc (typing)` for the open **dirty** buffer only — merged after the BSP set, cleared on save/close or when a compile publish arrives for the file. The pull never boots a cold island: typing diagnostics activate once some PC query (hover, completion, semantic tokens) has booted it |

**Not advertised** (do not enable client-side):
rangeFormatting/onTypeFormatting, and pull diagnostics. Range formatting is a
deliberate refusal, not an omission: the scalafmt CLI's hidden `--range from=to`
option is experimental and demonstrably skips lines inside multi-line ranges
(probed on the shipped scalafmt: `--range 3=4` leaves line 4 untouched where
`--range 4=4` alone formats it), and a partially formatted selection is worse
than no provider.
Compile diagnostics appear only after bootstrap connects to a BSP build and a
compile runs; typing diagnostics appear only once the PC island is booted.

### 4.6 Server commands (`workspace/executeCommand`)

| Command ID                  | Effect                                                                          |
|-----------------------------|----------------------------------------------------------------------------------|
| `scala3SemanticLs.compile`  | BSP compile of indexable targets → `compile ok (N targets)` / `compile failed: <code>` |
| `scala3SemanticLs.reindex`  | re-ingest SemanticDB for workspace targets → an `ingest: …` summary             |
| `scala3SemanticLs.doctor`   | the **live** doctor report (§5.2) — begins with `state: …`; pass `arguments: [{"json": true}]` for the structured object |
| `scala3SemanticLs.pcPluginStatus` | the PC island's plugin report (compiler plugins, service plugins + self-tests, disabled plugins); pass `arguments: [{"json": true}]` for the structured object. A **cold** island answers the typed `pc plugin status unavailable: PC island not booted (cold); …` status — the inspection never boots the JVM |

An unknown command id is an `InvalidParams` error.

### 4.7 The workspace state directory (`.scala3-bsp-semantic-ls/`)

The server keeps all per-workspace state in one directory under the workspace
root:

```text
.scala3-bsp-semantic-ls/
  manifest.json                  # single commit point: names the active (segment, workspace-state) pair
  workspace-state-<gen>.bin      # generational binary workspace state (doc epochs + SemanticDB md5s)
  segments/segment-NNNNNN/       # immutable index segment: the postings files
                                 # (ref/definition/rename/doc postings) + tables
  pc-plugins.json                # OPTIONAL, user-authored: PC plugin config (§4.8)
  config.json                    # OPTIONAL, user-authored: {"javaHome": "...", "scalafmt": "..."} overrides (§2.2, §4.9)
```

The `manifest.json` / `workspace-state` / `segments` tree is the immutable-
segment index store that replaced the v1 SQLite database. It is written with an
atomic tmp+fsync+rename commit protocol, is safe to delete wholesale while the
server is not running (it is rebuilt on the next bootstrap), and is inspectable
offline with `dump` (§3). `pc-plugins.json` and `config.json` are user
configuration — the server only reads them. See
[index-format.md](index-format.md) for the on-disk format.

### 4.8 The PC island and its plugins

The presentation compiler runs on an embedded JVM **inside the server
process**: on the first PC-backed query the server `dlopen`s
`<javaHome>/lib/server/libjvm.so`, calls `JNI_CreateJavaVM`, and loads the
PC-host agent jar as `-javaagent` (its `premain` wires up the FFM boundary).
The island JVM is launched with `--enable-native-access=ALL-UNNAMED` and
`-XX:+UseCompactObjectHeaders`, and is handed the workspace root so it loads
`<workspaceRoot>/.scala3-bsp-semantic-ls/pc-plugins.json` at boot.

A wedged PC query is recovered by the dispatch-generation watchdog (the island
respawns its dispatch; a generation cap turns repeated wedges into a fatal
island error) — exercised for real by the `pc-recovery` flake check.

**PC plugin configuration** (`pc-plugins.json` — `compilerPlugins` and
`servicePluginJars`; JSON schema shipped at
`share/scala3-bsp-semantic-ls/default-plugin-schema.json`; full contract in
[plugin-spi.md](plugin-spi.md)):

```json
{
  "compilerPlugins": [
    { "jars": ["/abs/path/to/plugin.jar"], "options": ["myPlugin:key:value"] }
  ],
  "servicePluginJars": []
}
```

Each `jars` entry becomes a `-Xplugin:<jar>` and each `options` entry a `-P:`
option on the island's compiler instances. The packaged server ships one such
plugin at `share/scala3-bsp-semantic-ls/zaozi-pcplugin.jar`: point a
workspace's `compilerPlugins` at it to make go-to-definition and hover resolve
zaozi's dynamic `io.field` bundle accesses to the real field declaration (see
[plugin-spi.md §2.1](plugin-spi.md)). It keys strictly on the zaozi API and is
inert on every other codebase.

### 4.9 Formatting (the scalafmt command line)

`textDocument/formatting` shells out to the **scalafmt CLI** — the server
never links scalafmt-core. Per request it runs

```bash
scalafmt --stdin --config <workspaceRoot>/.scalafmt.conf --non-interactive   # cwd = workspace root
```

over the **open buffer** text and folds the output into minimal `TextEdit`s
(the `dissimilar` diff rust-analyzer uses for formatting diffs; positions
UTF-16 over the original text). Facts an operator needs:

- **Binary resolution — config > env > nix-baked**, mirroring the Java home
  (§2.2), first hit wins:
  1. **Workspace config** — `.scala3-bsp-semantic-ls/config.json` with
     `{"scalafmt": "/abs/path/to/scalafmt"}`.
  2. **Environment** — `LS_SCALAFMT`, else the first executable `scalafmt` on
     `PATH` (the dev shell provides one).
  3. **Nix-baked** — the wrapper's `--set-default LS_SCALAFMT
     <scalafmt>/bin/scalafmt` (§2.1), applied only when the caller's
     environment sets nothing.
- **`.scalafmt.conf` is mandatory, workspace root only.** scalafmt requires a
  pinned `version` in the config; without the root file the request fails with
  the typed `no .scalafmt.conf in the workspace (scalafmt requires a pinned
  version)`. Nested-config semantics (`project.*` includes, `fileOverride`)
  are scalafmt's own business — the server hands it the one root config.
- **Offline stance: the shipped scalafmt is ONE fixed version and never
  downloads another.** The scalafmt CLI is the "dynamic" flavor that would
  fetch a `.scalafmt.conf`-pinned core version from Maven Central; the server
  spawns it with `COURSIER_MODE=offline`, so a workspace pinning a different
  version fails **fast and typed** (the error's stderr tail names the
  unresolvable `scalafmt-core` artifact) instead of downloading jars behind
  the editor's back. Fix: set the conf's `version` to the shipped scalafmt's
  (`scalafmt --version`), or point the `scalafmt` config key / `LS_SCALAFMT`
  at a binary of the pinned version.
- **Open buffers only.** A file the editor has not opened is a typed
  `… is not open` error — the server formats what you see, never the disk
  file behind the editor's back.
- **10-second deadline.** The spawn is killed past it and the request fails
  typed. The format request blocks the request loop while scalafmt runs (the
  same accepted class as a PC cold boot); it stays cancellable while queued.
- The request's LSP `options` (tab size, insert-spaces) are **ignored** —
  `.scalafmt.conf` governs.

### 4.10 Lifecycle & behaviors worth knowing

- **Async bootstrap** on `initialized`; `initialize` returns capabilities
  synchronously.
- **Re-index triggers:** `textDocument/didSave` schedules a debounced (500 ms),
  single-flight compile-first build job over the saved target's
  reverse-dependency closure; BSP `buildTarget/didChange` reloads the model.
  The server consumes only the document notifications
  (`didOpen`/`didChange`/`didClose`/`didSave`) — it does not consume client
  file watchers.
- **Dirty buffers:** open-buffer text is overlaid on PC queries and
  `workspace/symbol`; index answers still come from the last ingest.
- **Freshness:** SemanticDB is trusted only when its stored md5 matches the
  current source bytes. After editing a file you must recompile (save-driven or
  via the `compile`/`reindex` commands) before index features reflect the
  change.
- **Shutdown:** follow the standard LSP `shutdown` then `exit` sequence. A late
  bootstrap result delivered after `shutdown` is discarded, never resurrected.
- **Cold start:** there is no warm-up cache; startup is native-binary fast. (A
  PC-island-only AOT cache remains a possible future option, but nothing in the
  current tree builds or consumes one.)

---

## 5. Manual testing / verification

### 5.1 Quick sanity (offline, no JVM)

```bash
./result/bin/scala3-bsp-semantic-ls --version                 # scala3-bsp-semantic-ls 0.1.0
./result/bin/scala3-bsp-semantic-ls --doctor /abs/workspace   # offline doctor report
./result/bin/scala3-bsp-semantic-ls dump /abs/workspace       # read-only store inspection
```

The offline `--doctor` prints a header
(`state: offline (--doctor): build server and presentation compiler not
connected`, plus the workspace path) and renders real data for the **Runtime**,
**Nix**, and **Store** sections; the live-only sections render
`unavailable: <reason>`. To see the live sections, run the
`scala3SemanticLs.doctor` command over a connected LSP session.

### 5.2 Doctor report reference

The doctor renders **seven sections in fixed order** (text and `--json`):

| Section        | Key lines / meaning                                                                                   |
|----------------|-------------------------------------------------------------------------------------------------------|
| **Runtime**    | `Java:` (read from `$JAVA_HOME/release`, no process launched) · the island's static launch policy (native access, compact object headers) |
| **Nix**        | `flake detected:` · `mill-ivy-fetcher input:` · `ivy lock:` · `lock status: fresh/stale/unknown` (mtime heuristic; CI owns the authoritative check) |
| **BSP**        | `server:` (build-server identity) · `targets:` · Scala 3 targets · SemanticDB-coverage errors          |
| **SemanticDB** | per-target semanticdb root (exists/missing, file count) · doc freshness (fresh/stale/missing) · stale targets |
| **Store**      | manifest (schema, segment, state generation, docs, symbols) · active segment header · workspace-state — the same facts `dump` prints. Replaced the v1 SQLite section. |
| **PC**         | `worker status: booted / not booted (cold)` · active/registered targets                                |
| **PC Plugins** | the booted island's plugin report (compiler plugins loaded, service plugins + self-tests, disabled plugins with reasons); `unavailable: PC island not booted (cold); …` until the first PC query boots the island |

Gathering is **non-invasive**: the store is opened strictly read-only, the
PC status is read from `/proc/self/maps` (is libjvm mapped?), and the PC-plugin
report is fetched only from an **already-booted** island (over its control
lane), so the doctor never boots the JVM and never disturbs a live server that
owns the same store.
A cold, index-only session reports `worker status: not booted (cold)` — that is
the zero-JVM property, not a failure.

### 5.3 Real-BSP end-to-end (`scripts/it-real-bsp-rs.sh`)

The primary manual/CI end-to-end: drives the whole Rust `ls-server` over the
framed LSP wire against a **real** Mill BSP server built from
`it/sample-workspace` — production discovery → mill launch → model load →
compile → diagnostics → rename-through-compile → teardown — plus the embedded
presentation-compiler rows, the dispatch-generation recovery row, and the
zaozi-plugin navigation row. It also asserts the **cold-start zero-JVM
property** via `/proc/self/maps`: no libjvm is mapped until the first PC query.

```bash
nix develop -c ./scripts/it-real-bsp-rs.sh
```

Run it under `nix develop`: the PC rows need `LS_LIBJVM`,
`PC_HOST_AGENT_JAR`, and `LS_PC_TARGET_CLASSPATH` (the dev shell exports
them), and the script fails loudly if they are missing rather than silently
skipping. Set `LS_REAL_BSP_SKIP_PC=1` for an index-only smoke run.

On the sample workspace, expect module `c` **and** `mill-build` in the
SemanticDB-coverage error — `c` is the deliberate demonstration of the
mandatory-SemanticDB policy, not a failure.

### 5.4 Live Mill BSP smoke (`scripts/it-mill-smoke.sh`)

A narrower gate on the BSP client layer alone: discovery → launch →
initialize → project model → compile → a forced diagnostic, against real
`mill --bsp`:

```bash
nix develop -c ./scripts/it-mill-smoke.sh
```

### 5.5 Flake checks

`nix flake check` runs the whole hermetic suite — Rust
build/test/clippy/fmt, the offline package build, toolchain/lock hygiene, the
packaged-CLI check (`--version`/`--doctor`/`dump` offline against the real
`result/bin` binary), and the **live PC checks**, which boot the production
island against a real JVM inside the sandbox: `pc-boundary` (register/open/
completion/hover through the vtable), `pc-recovery` (watchdog recovery through
a real wedged completion), `pc-definition` (cross-file go-to through the
symbol-resolver round-trip), `pc-zaozi` (the shipped zaozi plugin steering
go-to), and `pc-server-definition` (`textDocument/definition` through the real
ls-server dispatch). The full list with one line each is in
[nix-build.md §5](nix-build.md).

### 5.6 Guards

```bash
./scripts/check-docs.sh                             # pure bash; docs/traceability + stale-claim checker
./scripts/check-audit-inventory.sh                  # coverage-audit accounts for every retained Scala suite
nix develop -c ./scripts/check-offline-compile.sh   # island build resolves entirely from the locked ivy cache
nix develop -c ./scripts/check-offline-compile.sh --self-test   # proves the guard rejects an unlocked dep
nix develop -c ./scripts/check-ivy-lock.sh          # committed ivy lock matches build.mill
```

The full CI command set is in [nix-build.md §7](nix-build.md).

---

## 6. Troubleshooting

| Symptom                                                                    | Cause                                                                 | Fix                                                                                     |
|----------------------------------------------------------------------------|----------------------------------------------------------------------|----------------------------------------------------------------------------------------|
| `<uri> has no SemanticDB output; every source must be compiled with -Xsemanticdb` | the file's target is not compiled with `-Xsemanticdb`         | add `-Xsemanticdb -sourceroot <workspaceRoot>` to that module's `scalacOptions`, recompile |
| A module (or `mill-build`) always in the SemanticDB-coverage error          | `mill-build` (and any flag-less module) emits no SemanticDB          | expected for `mill-build` — ignore it; for your own modules, add the flags (row above)  |
| `workspace is bootstrap failed: no build server connection found …`        | no usable `.bsp/*.json` in the workspace root                        | `mill mill.bsp.BSP/install` in the workspace, then restart the session                  |
| Request-timeout on the first/large compile                                 | BSP request timeout is 30 s                                          | pre-compile with `mill __.compile` before the session so the first BSP compile is warm  |
| `no Java home for the PC island: …` on the first completion/hover          | no config/env/baked Java home (e.g. raw cargo binary outside the dev shell) | set `javaHome` in `.scala3-bsp-semantic-ls/config.json`, or `LS_LIBJVM`/`JAVA_HOME`, or use the packaged wrapper (bakes the defaults) |
| Completion/hover fail but references/rename work                           | the PC island failed to boot (bad javaHome, missing agent jar)       | check stderr for the island boot error; verify `PC_HOST_AGENT_JAR` points at the packaged `share/…/pc-host-agent.jar` |
| Garbled / failed JSON-RPC in the client                                    | something wrote to the server's stdout, or the client reads logs from stdout | stdout is protocol-only; logs are on **stderr** — do not write to the child's stdout    |
| "workspace is …" errors right after opening                                | bootstrap still running (async on `initialized`)                    | wait for `Ready` (doctor `state: ready`); clients must tolerate warm-up                 |
| Server exits non-zero immediately with an argument message                 | unknown flag (including the removed v1 flags)                        | the only CLI surface is `--version`, `--doctor [dir] [--json]`, `dump [dir]` (§3)       |
| Bootstrap fails with `no workspace root in the initialize params`          | client sent no `rootUri`/`workspaceFolder`                          | send `initialize` with `rootUri` = workspace root (where `.bsp/` lives)                  |
| `error: mill not found on PATH`                                            | ran `./mill` (or a script) outside the dev shell                    | run inside `nix develop`, or prefix commands with `nix develop -c`                       |
| Offline island build / `check-ivy-lock.sh` fails after a dep change        | `nix/ivy-lock.nix` not regenerated                                  | `nix develop -c ./scripts/regen-ivy-lock.sh` and commit the lock                         |
| Zaozi `io.field` go-to lands on `selectDynamic`                            | the zaozi PC plugin is not configured for the workspace              | point `pc-plugins.json` `compilerPlugins` at `share/…/zaozi-pcplugin.jar` (§4.8)        |
| `no .scalafmt.conf in the workspace (scalafmt requires a pinned version)`  | the workspace root has no `.scalafmt.conf`                           | add one with `version = "<scalafmt --version>"` and `runner.dialect = scala3` (§4.9)    |
| Formatting fails with `scalafmt failed …` naming a `scalafmt-core` version | `.scalafmt.conf` pins a version other than the shipped scalafmt; the offline stance never downloads it | pin the conf's `version` to the shipped scalafmt's, or point config `scalafmt` / `LS_SCALAFMT` at a matching binary (§4.9) |
| `formatting: <uri> is not open`                                            | the client requested formatting for a file it never opened           | formatting serves the open buffer only — open the file in the editor first (§4.9)       |
