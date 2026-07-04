# Deployment Guide

> Practical operator guide: **package → AOT → wire into BSP + an editor → verify
> by hand.** It ties together the normative contracts in
> [nix-build.md](nix-build.md) (build/packaging/AOT wrapper) and
> [architecture.md](architecture.md) (subsystem behavior), and is derived from the
> real `build.mill`, `nix/`, `scripts/`, and `modules/ls-core` sources — every
> flag, path, and command below is quoted from the tree, not invented.

Contents:

1. [Prerequisites](#1-prerequisites)
2. [Packaging](#2-packaging)
3. [AOT cache](#3-aot-cache)
4. [BSP + language-server integration](#4-bsp--language-server-integration)
5. [Manual testing / verification](#5-manual-testing--verification)
6. [Troubleshooting](#6-troubleshooting)

---

## 1. Prerequisites

The **only** supported toolchain (see [nix-build.md §1](nix-build.md)):

```text
Java 25 only          (FFM SQLite binding, MemorySegment mmap, AOT cache, Compact Object Headers)
Scala 3.8.4           (pinned in build.mill)
Mill 1.1.2            (from mill-ivy-fetcher's mill-overlay)
Nix >= 2.28           (mill-ivy-fetcher requirement)
```

`nix develop` is the only entry point for building, locking, and testing. The
repo-root `./mill` script is a thin launcher: it `exec`s `mill` from `PATH` and
refuses to bootstrap outside the dev shell. All commands below assume you either
ran `nix develop` first or prefix with `nix develop -c`.

The dev shell exports (contract — code relies on these):

| Variable          | Meaning                                                                 |
|-------------------|-------------------------------------------------------------------------|
| `JAVA_HOME`       | the Nix JDK 25; the only JDK the build/runtime may use                  |
| `LS_JAVA_VERSION` | `"25"` — asserted by tooling/doctor                                     |
| `LS_SQLITE_LIB`   | absolute path to the Nix `libsqlite3` the `ls-sqlite-ffm` FFM layer binds |

---

## 2. Packaging

### 2.1 The deployable artifact (`nix build`)

```bash
nix build .#default
./result/bin/scala3-bsp-semantic-ls --version   # -> scala3-bsp-semantic-ls 0.1.0
```

`packages.<system>.default` builds fully offline (`mill --no-daemon core.assembly`
against the pre-fetched ivy cache) and installs a self-contained launcher:

```text
result/bin/scala3-bsp-semantic-ls                              # makeWrapper launcher (the entry point)
result/lib/scala3-bsp-semantic-ls/scala3-bsp-semantic-ls.jar   # core.assembly fat jar (main: ls.core.Main)
result/share/scala3-bsp-semantic-ls/default-plugin-schema.json # PC plugin config schema
result/share/scala3-bsp-semantic-ls/zaozi-pcplugin.jar         # PC compiler plugin: zaozi Dynamic bundle-field go-to/hover
```

The wrapper is `makeWrapper ${jdk25}/bin/java` with these baked-in settings — you
get all of them for free when you launch the wrapped binary:

| Wrapper setting                                        | Purpose                                                        |
|--------------------------------------------------------|----------------------------------------------------------------|
| `--set JAVA_HOME <jdk25>`                               | Java 25 only runtime                                           |
| `--set-default LS_SQLITE_LIB <libsqlite3>`             | pins the Nix SQLite for the FFM binding (override via env)     |
| `--add-flags --enable-native-access=ALL-UNNAMED`       | grants FFM native access (the assembly runs on the class path) |
| `--add-flags -XX:+UseCompactObjectHeaders`             | Java 25 compact object headers for the object-dense index      |
| `--add-flags ${LS_AOT_CACHE:+-XX:AOTCache=$LS_AOT_CACHE}` | opt-in AOT cache — emitted only when `LS_AOT_CACHE` is set  |
| `--add-flags -jar .../scala3-bsp-semantic-ls.jar`      | runs the assembly                                             |

`default-plugin-schema.json` in `share/` is the JSON schema for the PC plugin config
file (`pc-plugins.json` — both `compilerPlugins` and `servicePluginJars`; see
[plugin-spi.md](plugin-spi.md)); it is data, not required to run. `zaozi-pcplugin.jar`
in `share/` is a shipped PC **compiler** plugin: point a workspace's `pc-plugins.json`
`compilerPlugins` at it to make go-to-definition and hover resolve zaozi's dynamic
`io.field` bundle accesses to the real field declaration (see [plugin-spi.md §2.1](plugin-spi.md)).

Package facts: `pname = scala3-bsp-semantic-ls`, `version = 0.1.0`,
`mainProgram = scala3-bsp-semantic-ls`, platforms Linux + Darwin.

### 2.2 Dev / raw-jar build (`mill core.assembly`)

For iteration you can build and run the fat jar directly:

```bash
nix develop -c mill core.assembly
# -> out/core/assembly.dest/out.jar   (main class ls.core.Main)
```

Running the raw jar means **you** must supply what the wrapper otherwise bakes in —
JDK 25, the native-access + compact-headers flags, and `LS_SQLITE_LIB` (the dev
shell already exports it):

```bash
java --enable-native-access=ALL-UNNAMED -XX:+UseCompactObjectHeaders \
  -jar out/core/assembly.dest/out.jar          # stdio LSP server (see §4)
```

Only the `core` module is the deployable server. `bench` also declares a `Main`
(`ls.bench.BenchMain`), but it is a benchmark/JFR harness, not the shipped artifact.

### 2.3 Dependency lock & offline guarantee

The full ivy/Maven closure is locked into `nix/ivy-lock.nix` (fixed-output
`fetchMaven` entries) and consumed offline by the package build. After **any**
change to `build.mill` dependencies you must regenerate and commit it, or the
offline `package` build and CI gate fail:

```bash
nix develop -c ./scripts/regen-ivy-lock.sh     # preferred (determinism guards)
./scripts/check-ivy-lock.sh                    # CI gate: lock == build.mill
```

`nix flake check` runs four gates: `java25-toolchain`, `ivy-lock-present`,
`mill-ivy-fetcher-input`, and the full offline `package` build. See
[nix-build.md §3–5](nix-build.md) for the normative details.

---

## 3. AOT cache

An optional JDK 25 AOT cache (`-XX:AOTCache`) speeds up cold start and the first
request. It is **opt-in** and never required — without it the server runs normally
and the doctor reports `AOT cache: missing (no -XX:AOTCache flag)`.

### 3.1 Build a cache — `scripts/aot-train.sh`

```bash
nix develop -c ./scripts/aot-train.sh --workspace it/sample-workspace \
                                      --out .scala3-bsp-semantic-ls/aot-cache.bin
```

| Flag          | Default                               | Meaning                          |
|---------------|---------------------------------------|----------------------------------|
| `--workspace` | `it/sample-workspace`                 | workspace to train against       |
| `--out`       | `.scala3-bsp-semantic-ls/aot-cache.bin` | output cache path              |

The script (a) builds/reuses the assembly jar, then (b) runs the **JDK-25 two-step**
with the same runtime flags as the production launcher (they must match — the
compact-object-header layout and native-access grant are baked into the cache):

```bash
# 1/2 record  -> writes an AOT configuration to a temp file
java -XX:AOTMode=record -XX:AOTConfiguration=<tmp> \
  --enable-native-access=ALL-UNNAMED -XX:+UseCompactObjectHeaders \
  -jar <assembly.jar> --aot-train <workspace> --in-process-pc [--require-index]

# 2/2 create  -> turns the configuration into the cache file
java -XX:AOTMode=create -XX:AOTConfiguration=<tmp> -XX:AOTCache=<out> \
  --enable-native-access=ALL-UNNAMED -XX:+UseCompactObjectHeaders \
  -jar <assembly.jar> --aot-train <workspace> --in-process-pc [--require-index]
```

- **Strict vs lenient is chosen by `.bsp` presence.** If `<workspace>/.bsp` exists,
  the script adds `--require-index` and prints `aot-train: .bsp present -> strict
  real-BSP training`; the training run then drives a real compile + reindex and
  **fails (non-zero) unless the SemanticDB index is populated and queryable**
  (see the strict assertions in §5.3). With no `.bsp`, it degrades to a lenient
  best-effort warm-up that always exits 0.
- Training forces `--in-process-pc` so the presentation compiler's classes are
  recorded into the cache. Production defaults to a **forked** PC in a child JVM,
  which the cache cannot cover — so the cache warms the main server JVM, not the PC
  worker.
- Success prints: `aot-train: AOT cache created: <out> (<N> bytes)`.

### 3.2 Launch with the cache

**Wrapped binary** — set `LS_AOT_CACHE`; the wrapper adds `-XX:AOTCache` for you:

```bash
LS_AOT_CACHE=.scala3-bsp-semantic-ls/aot-cache.bin ./result/bin/scala3-bsp-semantic-ls
```

**Raw jar** — pass the flag yourself, matching the training flags:

```bash
java --enable-native-access=ALL-UNNAMED -XX:+UseCompactObjectHeaders \
  -XX:AOTCache=.scala3-bsp-semantic-ls/aot-cache.bin \
  -jar out/core/assembly.dest/out.jar
```

### 3.3 Verify it took effect

The doctor Runtime section reports the cache from **this JVM's own** `-XX:AOTCache`
flag plus file existence:

```text
AOT cache: loaded (<path>)                # flag present, file exists
AOT cache: missing (no -XX:AOTCache flag) # not enabled
AOT cache: missing (<path> does not exist)# flag present, wrong/absent path
```

```bash
LS_AOT_CACHE=.scala3-bsp-semantic-ls/aot-cache.bin \
  ./result/bin/scala3-bsp-semantic-ls --doctor /abs/path/to/workspace | grep 'AOT cache:'
```

> **Cache coupling.** A cache is tied to JDK 25 **and** the exact runtime flags it
> was trained with. Rebuild the cache after a JDK bump, a flag change, or a server
> rebuild. A wrong/stale path silently degrades to `missing`, never an error.

---

## 4. BSP + language-server integration

### 4.1 The SemanticDB prerequisite (mandatory)

Workspace-wide answers come **only** from scalac-generated SemanticDB. Every Scala 3
build target the server indexes **must** be compiled with SemanticDB enabled:

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

Mill configuration — set it in a shared `ScalaModule` trait (as `it/sample-workspace`
does):

```scala
// scalacOptions for every indexable module:
def scalacOptions = Seq("-Xsemanticdb", "-sourceroot", mill.api.BuildCtx.workspaceRoot.toString)
```

> This is Scala 3 only. `-Xsemanticdb` is built into the Scala 3 compiler (no
> separate plugin). For sbt the equivalent is enabling SemanticDB and adding
> `-sourceroot` to `scalacOptions`; only Scala 3 (`3` / `3.x`) targets are indexed —
> Scala 2 targets are ignored entirely. There is no in-repo sbt example.

**SemanticDB is a hard requirement, not graceful.** A live Scala 3 target with no
SemanticDB is an error surfaced two ways:

- the doctor prints
  `SemanticDB coverage: ERROR - N target(s) without SemanticDB (recompile with -Xsemanticdb): <ids>`, and
- every document/position request on such a source is rejected with
  `<uri> has no SemanticDB output; every source must be compiled with -Xsemanticdb`
  — there is **no** presentation-compiler fallback.

It does **not** fail boot: the workspace still reaches `Ready` and indexable targets
still index.

> **Expect a permanent `mill-build` entry in the coverage ERROR.** Mill always
> exposes its own build definition as a Scala 3 target (`.../mill-build`) compiled
> without `-Xsemanticdb`, so a clean Mill workspace *always* shows at least
> `mill-build` under the coverage ERROR. This is normal — only worry about *your*
> modules appearing there.

### 4.2 Install a BSP connection

The server does not run a build itself; it speaks BSP to one. For Mill, install the
connection once per workspace (writes `.bsp/mill-bsp.json`):

```bash
cd /path/to/your/workspace
mill mill.bsp.BSP/install     # -> .bsp/mill-bsp.json
mill __.compile               # optional pre-compile to emit SemanticDB up front
```

### 4.3 How discovery & launch work

On the LSP `initialized` notification the server bootstraps asynchronously:

1. **Discover** — scan `<workspaceRoot>/.bsp/*.json`, parse each (Gson), drop files
   missing `name`/`argv`, sort by BSP server name (ties by file name), pick the
   first. No usable file → the server still boots BSP-less (see §4.7), it does not
   crash.
2. **Launch** — run the picked file's `argv` verbatim as a child process
   (cwd = workspace root; for Mill this is `mill --bsp`), and talk BSP over its stdio.
3. **Handshake** — `build/initialize` (languageIds `["scala"]`) → `build/initialized`.
4. **Load model** — `workspace/buildTargets` (filtered to Scala 3) →
   `buildTarget/sources` + `buildTarget/scalacOptions` (+ best-effort, capability-gated
   `dependencySources`/`outputPaths`). Compiles go through `buildTarget/compile`.
5. **Ingest** — parse SemanticDB for indexable targets into the SQLite + postings index.

A server-initiated `buildTarget/didChange` reloads the project model (re-runs load +
ingest without re-initializing).

> **BSP request timeout is 30 s in the interactive server.** The LSP-server connect path
> uses the default `BspSessionConfig` (`requestTimeout = 30.seconds`). Non-default
> timeouts elsewhere are the headless `--aot-train` driver's 600 s and the test fixtures'
> 300 s — so a cold first compile won't time out under `--aot-train` the way it can in an
> editor session. A very large *first* compile over BSP can exceed the interactive 30 s
> and surface as a `RequestTimeout`; pre-compiling with `mill __.compile` before starting
> an editor session avoids the cold-compile spike.

### 4.4 Wire the server into an editor

The server is a **generic stdio LSP server** — no editor-specific launcher ships in
this repo, so integrate it as you would any stdio LSP:

- **Command:** the wrapped binary `scala3-bsp-semantic-ls` (or `java … -jar
  out/core/assembly.dest/out.jar`). No CLI args are needed for normal use; append
  `--forked-pc` (default) or `--in-process-pc` to choose the PC backend, and set
  `LS_AOT_CACHE` in the environment to enable the cache.
- **Transport:** stdin = requests, **stdout = framed JSON-RPC only**, stderr = logs.
  The server redirects its own `System.out` to stderr at startup so stray output can
  never corrupt the protocol — a client must **not** write to the child's stdout, and
  must read logs from stderr.
- **Language / filetype:** `scala`.
- **Root:** send `initialize` with `rootUri` pointing at the workspace root (where
  `.bsp/` lives). If neither `rootUri` nor a `workspaceFolder` is sent, the server
  roots at its own process cwd — usually the wrong project.
- **Warm-up:** heavy work runs asynchronously after `initialized`. Until the workspace
  is `Ready`, requests may return empty results or typed "workspace is …" errors —
  never crashes. Clients must tolerate this brief warm-up.

Illustrative Neovim (`nvim-lspconfig` custom config — adjust paths):

```lua
-- Illustrative only; no official client config ships in-repo.
local configs = require("lspconfig.configs")
if not configs.scala3_bsp_semantic_ls then
  configs.scala3_bsp_semantic_ls = {
    default_config = {
      cmd = { "scala3-bsp-semantic-ls" },        -- or: java --enable-native-access=ALL-UNNAMED ... -jar out.jar
      filetypes = { "scala" },
      root_dir = require("lspconfig.util").root_pattern(".bsp", "build.mill"),
    },
  }
end
require("lspconfig").scala3_bsp_semantic_ls.setup({})
```

VS Code / Emacs `eglot` / any LSP client work the same way: register a stdio server
for `scala` whose command is the binary and whose root is the workspace. Register the
four `scala3SemanticLs.*` commands (§4.6) if your client exposes `workspace/executeCommand`.

### 4.5 Capabilities advertised

| Advertised                                                                              | Notes                                        |
|-----------------------------------------------------------------------------------------|----------------------------------------------|
| Text sync = **Full**                                                                    | whole-document; last change's full text used |
| Completion (`resolveProvider`, trigger `.`)                                             | served by the presentation compiler          |
| Hover; SignatureHelp (triggers `(` `,`)                                                 |                                              |
| Definition; TypeDefinition                                                              |                                              |
| References                                                                              | whole-repo, from the SemanticDB index        |
| Rename (with `prepareProvider`)                                                         | cross-file                                   |
| DocumentHighlight                                                                       |                                              |
| workspace/symbol                                                                        | FTS + fuzzy over the index                   |
| executeCommand (4 command IDs — §4.6)                                                   |                                              |
| **Diagnostics: push-only**                                                              | via `textDocument/publishDiagnostics`; **no** pull `diagnosticProvider` |

**Not advertised** (do not enable client-side): semanticTokens, inlayHint, codeAction,
formatting/rangeFormatting, folding, and pull diagnostics. Diagnostics appear only
after bootstrap connects to a BSP build and a compile runs.

### 4.6 Server commands (`workspace/executeCommand`)

| Command ID                       | Effect                                                                          |
|----------------------------------|---------------------------------------------------------------------------------|
| `scala3SemanticLs.compile`       | BSP compile of indexable targets → `compile ok (N targets)` / `compile failed: <code>` |
| `scala3SemanticLs.reindex`       | re-ingest SemanticDB for workspace targets → an `ingest: …` summary             |
| `scala3SemanticLs.doctor`        | the **live** doctor report (§5.2) — begins with `state: …`                       |
| `scala3SemanticLs.pcPluginStatus`| render presentation-compiler plugin status                                      |

An unknown command id is an `InvalidParams` error.

### 4.7 Lifecycle & behaviors worth knowing

- **Async bootstrap** on `initialized` (daemon thread); `initialize` returns
  capabilities synchronously.
- **Re-index triggers:** a debounced (~500 ms) compile + reindex over the saved
  target's reverse-dependency closure on `textDocument/didSave`, and a model reload
  on BSP `buildTarget/didChange`. `didChangeConfiguration` and
  `didChangeWatchedFiles` are **no-ops** — the server does not consume client file
  watchers.
- **No-BSP warm restart:** with no `.bsp` connection the server still reaches `Ready`
  and answers from the recovered persisted index, but **PC is disabled** and only
  already-indexed sources answer. A compile that the references/rename path needs is a
  hard failure (the `scala3SemanticLs.compile` command instead returns `compile skipped:
  no indexable targets`), so fresh references/rename that need a compile cannot run.
- **Freshness:** SemanticDB is trusted only when its stored MD5 matches the current
  source bytes. After editing a file you must recompile (save-driven or via the
  `compile`/`reindex` commands) before index features reflect the change.
- **Shutdown:** follow the standard LSP `shutdown` then `exit` sequence; `exit`
  returns process code 0 only if `shutdown` was received first, else 1.

### 4.8 Server CLI reference

The server binary (`ls.core.Main`) is primarily the stdio LSP server; it also has a
few headless modes. Flag dispatch is ordered and short-circuiting
(`--version` → `--doctor` → `--aot-train` → default LSP server).

| Flag                    | Meaning                                                                                  |
|-------------------------|------------------------------------------------------------------------------------------|
| *(none)*                | start the stdio LSP server (default; PC backend defaults to **forked**)                  |
| `--version`             | print `scala3-bsp-semantic-ls 0.1.0` and exit                                             |
| `--doctor [<dir>]`      | print the **offline** doctor report for `<dir>` (default `.`) and exit                    |
| `--aot-train <dir>`     | run the headless AOT-training workload against `<dir>` and exit                           |
| `--require-index`       | with `--aot-train`: strict real-BSP mode (fail non-zero on an empty/absent index)         |
| `--skip-pc`             | with `--aot-train --require-index`: skip the version-locked PC completion check           |
| `--forked-pc`           | run the PC in an isolated child JVM (**production default**; wins if both PC flags given) |
| `--in-process-pc`       | run the PC in the same JVM (used by AOT training)                                         |

> **Value flags take the immediately-following token.** Write `--aot-train <dir>
> --require-index`, *not* `--aot-train --require-index <dir>` — the latter makes the
> workspace dir the literal string `--require-index`. `--plugin-config` and
> `--jfr-preset` are **not** server flags (they belong to the forked PC worker and the
> benchmark harness respectively).

---

## 5. Manual testing / verification

### 5.1 Quick sanity

```bash
./result/bin/scala3-bsp-semantic-ls --version               # scala3-bsp-semantic-ls 0.1.0
./result/bin/scala3-bsp-semantic-ls --doctor /abs/workspace # offline doctor (Runtime + Nix only)
```

`--doctor` is **offline-only**: it renders real data for the Runtime and Nix sections
and prints `unavailable: not connected` for every live subsystem, with **no** `state:`
header. To see live sections you must run the `scala3SemanticLs.doctor` command over a
connected LSP session (§5.2), or drive the headless smoke (§5.3).

### 5.2 Doctor report reference

The **live** doctor (`scala3SemanticLs.doctor`) begins with `state: ready | not
ready: … | bootstrap failed: …` and renders eight sections in fixed order, then a
trailing `Bootstrap:` notes block:

| Section        | Key lines / meaning                                                                                   |
|----------------|-------------------------------------------------------------------------------------------------------|
| **Runtime**    | `Java:` ver · `Native access:` · `Compact Object Headers:` · `AOT cache: loaded/missing`              |
| **Nix**        | `flake detected:` · `mill-ivy-fetcher input:` · `ivy lock:` · `lock status: fresh/stale`              |
| **BSP**        | `server:` · `targets:` · `Scala 3 targets:` · `SemanticDB coverage: all … / ERROR - …`                |
| **SemanticDB** | `semanticdb roots:` · per-target root (exists/missing, file count) · fresh/stale/missing docs · `generated source status:` · `stale targets:` |
| **SQLite**     | `database:` · `WAL:` (`journal_mode=`) · `FTS:` · `manifest generation:` · `documents:` · `symbols:` · `wal size:` |
| **Postings**   | `active segments:` · per-segment active/superseded · `snapshot id/docs/occurrences:` · `compaction pending:` · `snapshot file: consistent/divergent/missing` |
| **PC**         | `worker status: forked worker alive / forked worker not running / in-process (no forked worker)` · active/registered targets |
| **PC Plugins** | `compiler plugins loaded:` · `service plugins loaded:` · `self-test results:` · `disabled plugins:`   |

Two lines are especially useful for deployment triage: **PC `worker status:`** tells
you whether the forked PC worker is alive vs in-process, and **`snapshot file:`**
cross-checks the published snapshot against the SQLite manifest (`divergent` = they
disagree).

### 5.3 Headless end-to-end smoke (`--aot-train --require-index`)

There is no CLI verb that fires the executeCommands directly, but the strict
AOT-training driver exercises the **same production code path** end-to-end (compile →
reindex → workspace/symbol → references → completion) and exits non-zero on any empty
result — this is the code-supported headless smoke test. (One difference from an editor
session: the headless driver uses a 600 s BSP timeout, so a cold first compile won't time
out here.)

```bash
nix develop -c bash -c '
  mill --no-daemon core.assembly &&
  java --enable-native-access=ALL-UNNAMED -XX:+UseCompactObjectHeaders \
    -jar out/core/assembly.dest/out.jar \
    --aot-train "$PWD/it/sample-workspace" --require-index'
```

Strict mode asserts, in order: **compile** succeeds (`compile ok …`), **reindex**
indexes > 0 docs, a **workspace/symbol** query returns the probed top-level type,
**references** on it returns locations, and (unless `--skip-pc`) **PC completion**
returns items. Any failure logs `FAIL: <reason>` and exits 1. Use `--skip-pc` for a
real repo whose compiler version differs from the bundled 3.8.x PC.

### 5.4 Full manual smoke sequence

```bash
# 0. toolchain + unit sanity (gated real-BSP/AOT suites auto-skip here)
nix develop
mill __.compile && mill __.test

# 1. package + sanity-check the wrapper
nix build .#default
./result/bin/scala3-bsp-semantic-ls --version

# 2. prepare a workspace with SemanticDB + a BSP connection
cd it/sample-workspace          # or your own -Xsemanticdb -sourceroot project
mill --no-daemon mill.bsp.BSP/install    # writes .bsp/mill-bsp.json
mill --no-daemon __.compile              # emit SemanticDB (a/b index; c is a deliberate ERROR)
cd ../..

# 3a. headless drive of the production path (fast, non-interactive)
java --enable-native-access=ALL-UNNAMED -XX:+UseCompactObjectHeaders \
  -jar out/core/assembly.dest/out.jar --aot-train "$PWD/it/sample-workspace" --require-index

# 3b. OR interactive: wire ./result/bin/scala3-bsp-semantic-ls into an editor (§4.4),
#     open a file, then run the commands and check the doctor:
#       scala3SemanticLs.compile   -> "compile ok (N targets)"
#       scala3SemanticLs.reindex   -> "ingest: … docs …"
#       scala3SemanticLs.doctor    -> "state: ready", coverage line, PC worker status
#     then try hover / go-to-definition / find-references / rename on a symbol.
```

> On the sample workspace, expect `c` **and** `mill-build` in the `SemanticDB
> coverage: ERROR` line — that is the intended demonstration of the mandatory-SemanticDB
> policy, not a failure.

### 5.5 Gated integration scripts

These exercise real Mill BSP / AOT and are **skipped** by an ordinary `mill __.test`
(the suites `assume(enabled)` on their gate env var). Run them via `nix develop -c`:

| Script                          | Gate / env                                                         | What it proves · green output                                             |
|---------------------------------|--------------------------------------------------------------------|---------------------------------------------------------------------------|
| `./scripts/it-real-bsp.sh`      | sets `LS_REAL_BSP_IT=1`, `LS_REPO_ROOT`                             | 4 real-BSP suites vs a live `mill --bsp` fixture · mill exits 0, 0 failed  |
| `./scripts/it-aot.sh`           | builds assembly; sets `LS_AOT_IT=1`, `LS_REPO_ROOT`, `LS_AOT_ASSEMBLY_JAR` | AOT training + cached-boot suites · mill exits 0, both pass         |
| `./scripts/it-zaozi.sh`         | needs `ZAOZI_SRC` + `LS_SQLITE_LIB` (from the dev shell); optional `ZAOZI_PROBE_SYMBOL` | heavy real-repo validation · ends `it-zaozi: OK — the server indexed the full original zaozi` |
| `./scripts/aot-train.sh`        | (see §3.1)                                                          | builds a real cache · `aot-train: AOT cache created: <out> (<N> bytes)`    |

```bash
nix develop -c ./scripts/it-real-bsp.sh
nix develop -c ./scripts/it-aot.sh
nix develop -c ./scripts/it-zaozi.sh
```

### 5.6 Guards

```bash
./scripts/check-docs.sh                             # pure bash; docs/traceability + stale-claim checker
nix develop -c ./scripts/check-offline-compile.sh   # build resolves entirely from the locked ivy cache
nix develop -c ./scripts/check-offline-compile.sh --self-test   # proves the guard rejects an unlocked dep
```

Full CI command set is in [nix-build.md §4](nix-build.md).

---

## 6. Troubleshooting

| Symptom                                                                    | Cause                                                                 | Fix                                                                                     |
|----------------------------------------------------------------------------|----------------------------------------------------------------------|----------------------------------------------------------------------------------------|
| `<uri> has no SemanticDB output; every source must be compiled with -Xsemanticdb` | the file's target is not compiled with `-Xsemanticdb`         | add `-Xsemanticdb -sourceroot <workspaceRoot>` to that module's `scalacOptions`, recompile |
| A module (or `mill-build`) always in `SemanticDB coverage: ERROR`          | `mill-build` (and any flag-less module) emits no SemanticDB          | expected for `mill-build` — ignore it; for your own modules, add the flags (row above)  |
| `RequestTimeout` on the first/large compile                                | shipped BSP request timeout is 30 s                                  | pre-compile with `mill __.compile` before the session so the first BSP compile is warm  |
| SQLite `UnsatisfiedLinkError` / FFM failure at startup                     | `LS_SQLITE_LIB` unset or wrong; missing native-access flag           | use the wrapped binary, or set `LS_SQLITE_LIB=<libsqlite3>` + `--enable-native-access=ALL-UNNAMED` |
| Garbled / failed JSON-RPC in the client                                    | something wrote to the server's stdout, or the client reads logs from stdout | stdout is protocol-only; logs are on **stderr** — do not write to the child's stdout    |
| Empty results / "workspace is …" right after opening                       | bootstrap still running (async on `initialized`)                    | wait for `Ready` (doctor `state: ready`); clients must tolerate warm-up                 |
| `AOT cache: missing` despite training                                      | `LS_AOT_CACHE` unset, or `-XX:AOTCache` path wrong                  | set `LS_AOT_CACHE=<path>` (wrapper) / pass `-XX:AOTCache=<path>` (raw jar); verify in doctor |
| Server roots at the wrong project                                          | client sent no `rootUri`/`workspaceFolder`                          | send `initialize` with `rootUri` = workspace root (where `.bsp/` lives)                  |
| `error: mill not found on PATH`                                            | ran `./mill` (or a script) outside the dev shell                    | run inside `nix develop`, or prefix commands with `nix develop -c`                       |
| Offline `package` build / `check-ivy-lock.sh` fails after a dep change     | `nix/ivy-lock.nix` not regenerated                                  | `nix develop -c ./scripts/regen-ivy-lock.sh` and commit the lock                         |
