# scala3-bsp-semantic-ls

A special-purpose language server for **Scala 3 + BSP** projects. It does not
aim for Metals' generality; it aims for exact, high-performance answers to
three workspace-wide questions:

1. `workspace/symbol`
2. whole-repo `textDocument/references`
3. cross-file `textDocument/rename`

Workspace-wide semantic truth comes **only** from scalac-generated SemanticDB;
immutable mmap postings segments (committed through an atomic-rename
`manifest.json` paired with generational workspace-state files) are
materialized indexes over that truth — never approximations. The host process
is a single **Rust binary**; the Scala 3 presentation compiler serves
interactive editing (completion, hover, signature help, definition,
dirty-buffer overlay) from an **embedded, lazily booted in-process JVM island**
reached over a flat C ABI via the Java FFM API — the only JNI artifact is the
single `JNI_CreateJavaVM` boot symbol, and an index-only session runs with
zero JVM in the process. The presentation compiler is never a source of
persistent index data.

See [plan-rust.md](plan-rust.md) for the v2 decision record and
[docs/architecture.md](docs/architecture.md),
[docs/index-format.md](docs/index-format.md),
[docs/plugin-spi.md](docs/plugin-spi.md),
[docs/nix-build.md](docs/nix-build.md) for the normative contracts.
[docs/deployment.md](docs/deployment.md) is the operator guide: packaging,
BSP/editor integration, and manual verification.
[docs/traceability.md](docs/traceability.md) and
[docs/coverage-audit.md](docs/coverage-audit.md) map the mandates and the
ported test inventory.

## Toolchain contract

- Rust stable (flake-pinned) for the host process
- Java 25 only, for the presentation-compiler island only
- Scala 3 only, exactly pinned
- Linux only (the embedded-libjvm boundary is supported on Linux exclusively)
- Nix flake + cargo/crane + Mill +
  [mill-ivy-fetcher](https://github.com/Avimitin/mill-ivy-fetcher)
  are the only supported build entry points (Nix >= 2.28)

## Quickstart

```bash
nix develop                      # rust toolchain + Java 25 + Mill + mif
cargo test --workspace           # the Rust core
mill __.test                     # the retained JVM island
```

Build the package:

```bash
nix build .#default              # bin/scala3-bsp-semantic-ls + the island agent jar
```

Refresh the dependency lock after changing `build.mill`:

```bash
nix develop -c ./scripts/regen-ivy-lock.sh
./scripts/check-ivy-lock.sh      # CI gate
```

## Deploy in a flake project

The flake exposes the ready-to-run server as `packages.<system>.default`: a
**self-contained wrapper** that bakes in its own pinned JDK 25, the
presentation-compiler island jar, and the scalafmt CLI, so a consumer does
**not** need a matching `nixpkgs`, a JDK, or Mill on their side. The wrapped
binary is `scala3-bsp-semantic-ls` and its resolution defaults stay overridable
(config > env > baked — see [Configuration](#configuration-optional) below).

> **Linux only** (`x86_64-linux`, `aarch64-linux`). The embedded-libjvm boundary
> is supported on Linux exclusively; macOS is not.

### 1. Add the flake input

```nix
# flake.nix of the project you want to edit with this LSP
{
  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
  inputs.scala3-bsp-semantic-ls.url = "github:xinpian-tech/scala3-bsp-semantic-ls";

  outputs = { self, nixpkgs, scala3-bsp-semantic-ls }:
    let
      system = "x86_64-linux";
      pkgs = nixpkgs.legacyPackages.${system};
      # The wrapped server binary — puts `scala3-bsp-semantic-ls` on PATH.
      ls = scala3-bsp-semantic-ls.packages.${system}.default;
    in {
      devShells.${system}.default = pkgs.mkShell {
        packages = [
          ls
          # + your own Scala toolchain: mill / sbt / jdk, etc.
        ];
      };
    };
}
```

`nix develop` in that project now has `scala3-bsp-semantic-ls` on `PATH`; point
your editor at it (step 4). The server pins its own runtime, so it works
regardless of the JDK in your dev shell.

Other ways to get the binary (all equivalent — pick one):

```bash
nix run github:xinpian-tech/scala3-bsp-semantic-ls -- --version   # one-off, no install
nix profile install github:xinpian-tech/scala3-bsp-semantic-ls    # into your user profile
nix build github:xinpian-tech/scala3-bsp-semantic-ls#default      # ./result/bin/scala3-bsp-semantic-ls
```

> First build compiles the Rust core (crane) and the JVM island (Mill) from
> source — minutes, once. Add your own binary cache if you deploy this widely;
> the flake ships no public cache.

### 2. Make your build emit SemanticDB

Workspace-wide answers come **only** from scalac-generated SemanticDB, so every
**Scala 3** target you want indexed must compile with it (Scala 2 targets are
ignored). In a shared Mill `ScalaModule` trait:

```scala
def scalacOptions = Seq("-Xsemanticdb", "-sourceroot", mill.api.BuildCtx.workspaceRoot.toString)
```

sbt/bloop: add `-Xsemanticdb -sourceroot <workspaceRoot>` to `scalacOptions`.
A live Scala 3 target without SemanticDB is a typed hard error per request on
its sources (no PC fallback), and the doctor lists it under the coverage error —
booting still succeeds and other targets still index. (A permanent `mill-build`
entry in that error is normal.) Full rules: [docs/deployment.md §4.1](docs/deployment.md).

### 3. Provide a BSP connection file

The server discovers the build over `.bsp/<name>.json` at the workspace root and
speaks BSP to it. For Mill:

```bash
mill mill.bsp.BSP/install     # writes .bsp/mill-bsp.json
```

sbt (`sbt bspConfig`) or Bloop work the same way — any BSP 2.x server that
exposes `buildTarget/scalacOptions` (so the server can read each target's
SemanticDB flags) is fine.

### 4. Point your editor at the server

It is a **generic stdio LSP server** (stdout = framed JSON-RPC only, stderr =
logs), language id `scala`, root = the directory containing `.bsp/`. Heavy work
runs after `initialized`; until the workspace is `Ready` requests return typed
"workspace is …" errors rather than crashing — clients must tolerate the brief
warm-up.

Neovim (`nvim-lspconfig`):

```lua
local configs = require("lspconfig.configs")
if not configs.scala3_bsp_semantic_ls then
  configs.scala3_bsp_semantic_ls = {
    default_config = {
      cmd = { "scala3-bsp-semantic-ls" },        -- on PATH via the dev shell
      filetypes = { "scala" },
      root_dir = require("lspconfig.util").root_pattern(".bsp", "build.mill"),
    },
  }
end
require("lspconfig").scala3_bsp_semantic_ls.setup({})
```

VS Code / Emacs `eglot` / any LSP client: register a stdio server for `scala`
whose command is the binary and whose root is the workspace. Editor-integration
depth (capabilities, commands, the `scala3SemanticLs.doctor` health command)
is in [docs/deployment.md §4](docs/deployment.md).

### Configuration (optional)

Per-workspace overrides live in `<workspaceRoot>/.scala3-bsp-semantic-ls/config.json`;
each key wins over the environment, which wins over the nix-baked default:

```json
{
  "javaHome": "/abs/path/to/jdk",    // PC island JVM; else $LS_LIBJVM / $JAVA_HOME / baked JDK 25
  "scalafmt": "/abs/path/to/scalafmt" // textDocument/formatting binary; else $LS_SCALAFMT / baked
}
```

An index-only session (references / rename / workspace-symbol / documentSymbol)
boots **zero JVM** — `javaHome` matters only once a presentation-compiler feature
(completion, hover, definition, inlay hints, …) is first used. `textDocument/formatting`
needs a workspace-root `.scalafmt.conf` with a pinned `version`; the baked
scalafmt runs offline, so a version mismatch is a typed error, not a download.

Verify a deployment without an editor:

```bash
scala3-bsp-semantic-ls --version
scala3-bsp-semantic-ls --doctor /path/to/workspace   # offline health report, no JVM
```

## Layout

| Crate (cargo) | Role |
|---|---|
| `crates/ls-index-model` | ids, spans, flags, roles, errors |
| `crates/ls-semanticdb`  | SemanticDB locator/parser/normalizer/groups |
| `crates/ls-store`       | segments, manifest, workspace-state, snapshots, search |
| `crates/ls-bsp`         | BSP client, project model, target graph |
| `crates/ls-engine`      | ingest pipeline, references/rename engines, orchestrator |
| `crates/ls-pc-abi`      | the flat `#[repr(C)]` island boundary + cbindgen header |
| `crates/ls-jvm`         | libjvm dlopen boot, dispatch generations, watchdog |
| `crates/ls-server`      | LSP loop, bootstrap, diagnostics, doctor, CLI (`main`) |
| `crates/ls-bench`       | ingest+query benchmark harness (`--smoke` CI gate) |
| `crates/ls-jvm-spike`   | embedded-JVM boundary viability spike |

| Mill module (JVM island) | Directory | Role |
|---|---|---|
| `pc`            | `modules/ls-pc`            | presentation-compiler facade + plugin SPI |
| `pcHost`        | `modules/ls-pc-host`       | island host `-javaagent` (FFM premain)    |
| `pcHostSpike`   | `modules/ls-pc-host-spike` | boundary spike agent                      |
| `pcNavTestPlugin` | `modules/ls-pc-navtestplugin` | pc-plugins.json test-fixture plugin (`-Xplugin`; check input, not shipped) |
