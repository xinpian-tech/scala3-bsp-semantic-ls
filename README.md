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
BSP/editor integration, logging, and manual verification.
[docs/traceability.md](docs/traceability.md) and
[docs/coverage-audit.md](docs/coverage-audit.md) map the mandates and the
ported test inventory.

## The supported toolchain

This project is opinionated about the stack on **both** sides of the wire.
The recipe below is the supported, CI-proven path; other combinations may
work, but they are not what the test suites prove.

| Layer | Tool | Why it is mandated |
|---|---|---|
| Environment & packaging | **Nix flakes** (Nix ≥ 2.28) | the flake output is the only supported artifact: a self-contained wrapper that bakes in JDK 25, the presentation-compiler island jar, and the scalafmt CLI — reproducible on any Linux machine, no toolchain drift on the consumer side |
| Build & BSP server | **Mill** | the first-class BSP path: one `def scalacOptions` line turns on SemanticDB, one command writes the connection file, and the project-level e2e suites run against a real Mill monorepo |
| Editor | **Zed** (Neovim config also provided) | the server is a generic stdio LSP; Zed's `lsp.<server>.binary` override runs it with zero extension code, and Zed's language-server log panel shows the server's diagnostic narrative directly |
| OS | **Linux** (`x86_64-linux`, `aarch64-linux`) | the embedded-libjvm boundary is supported on Linux exclusively; macOS is not |

An index-only session (references / rename / workspace-symbol /
documentSymbol) boots **zero JVM**; the baked JDK matters only once a
presentation-compiler feature (completion, hover, definition, …) is first
used. Your project's own JDK/Scala versions are unaffected — the server pins
its own runtime.

## Deploy in your project — the recipe

Follow the five steps in order; each is copy-paste ready. Everything deeper
(capabilities, server commands, the workspace state directory, scalafmt
rules) is in [docs/deployment.md §4](docs/deployment.md).

### 1. Nix — add the flake input

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
          pkgs.mill        # the BSP build server this recipe assumes
          # + your project's own JDK / Scala toolchain
        ];
      };
    };
}
```

`nix develop` in that project now has `scala3-bsp-semantic-ls` and `mill` on
`PATH`. Other ways to get the binary (all equivalent — pick one):

```bash
nix run github:xinpian-tech/scala3-bsp-semantic-ls -- --version   # one-off, no install
nix profile install github:xinpian-tech/scala3-bsp-semantic-ls    # into your user profile
nix build github:xinpian-tech/scala3-bsp-semantic-ls#default      # ./result/bin/scala3-bsp-semantic-ls
```

> First build compiles the Rust core (crane) and the JVM island (Mill) from
> source — minutes, once. Add your own binary cache if you deploy this widely;
> the flake ships no public cache.

### 2. Mill — make the build emit SemanticDB

Workspace-wide answers come **only** from scalac-generated SemanticDB, so
every **Scala 3** target you want indexed must compile with it (Scala 2
targets are ignored). In a shared Mill `ScalaModule` trait:

```scala
def scalacOptions = Seq("-Xsemanticdb", "-sourceroot", mill.api.BuildCtx.workspaceRoot.toString)
```

sbt/bloop: add `-Xsemanticdb -sourceroot <workspaceRoot>` to `scalacOptions`.
A live Scala 3 target without SemanticDB is a typed hard error per request on
its sources (no PC fallback), and the doctor lists it under the coverage error —
booting still succeeds and other targets still index. (A permanent `mill-build`
entry in that error is normal.) Full rules: [docs/deployment.md §4.1](docs/deployment.md).

### 3. Mill — install the BSP connection file

The server discovers the build over `.bsp/<name>.json` at the workspace root
and speaks BSP to it:

```bash
mill mill.bsp.BSP/install     # writes .bsp/mill-bsp.json
```

sbt (`sbt bspConfig`) or Bloop work the same way — any BSP 2.x server that
exposes `buildTarget/scalacOptions` (so the server can read each target's
SemanticDB flags) is fine.

### 4. Zed — point the Scala language server at this binary

Install the **Scala** extension (`zed: extensions`); it registers language
server id `metals` for `scala` buffers. Override that server's binary and Zed
launches this server instead of downloading Metals — in your user
`~/.config/zed/settings.json`, or checked into the project as
`.zed/settings.json`:

```json
{
  "lsp": {
    "metals": {
      "binary": {
        "path": "/home/you/.nix-profile/bin/scala3-bsp-semantic-ls",
        "arguments": []
      }
    }
  }
}
```

- The path must be **absolute** — Zed does not consult `$PATH` for overridden
  binaries. `nix profile install` (step 1) gives the stable
  `~/.nix-profile/bin/…` path above; for a checked-in `.zed/settings.json`,
  `nix build github:xinpian-tech/scala3-bsp-semantic-ls -o .zed/ls` pins a
  per-clone `<clone>/.zed/ls/bin/scala3-bsp-semantic-ls` symlink instead.
- Open the project at the directory containing `.bsp/` (the workspace root).
- Metals-specific `initializationOptions` the extension sends are ignored;
  the server reads only what it needs from `initialize`.
- Server logs (the diagnostic narrative of [step 5](#5-verify--doctor--the-log-narrative))
  are in the command palette under `dev: open language server logs`.

Neovim (`nvim-lspconfig`) — the provided alternative:

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
(stdout = framed JSON-RPC only, stderr = logs) whose command is the binary and
whose root is the workspace. Heavy work runs after `initialized`; until the
workspace is `Ready` requests return typed "workspace is …" errors rather than
crashing — clients must tolerate the brief warm-up.

### 5. Verify — doctor + the log narrative

Without an editor:

```bash
scala3-bsp-semantic-ls --version
scala3-bsp-semantic-ls --doctor /path/to/workspace   # offline health report, no JVM
```

With an editor: open a `.scala` file and read the server's stderr (in Zed:
`dev: open language server logs`). The server narrates its whole lifecycle in
`[+elapsed LEVEL area]` lines — `.bsp` discovery, build-server launch,
handshake (with `still waiting for …` heartbeats while Mill starts up or
another Mill holds the workspace lock), target model, index ingest, and
finally `READY in <seconds>`. If it ever looks stuck, the "last line you see →
where it is stuck → what to check" table in
[docs/deployment.md §6](docs/deployment.md) resolves it; `LS_LOG=debug` and
`LS_LOG_FILE=/tmp/ls.log` (for editors that swallow stderr) are the two knobs.

### Configuration (optional)

Per-workspace overrides live in `<workspaceRoot>/.scala3-bsp-semantic-ls/config.json`;
each key wins over the environment, which wins over the nix-baked default:

```json
{
  "javaHome": "/abs/path/to/jdk",    // PC island JVM; else $LS_LIBJVM / $JAVA_HOME / baked JDK 25
  "scalafmt": "/abs/path/to/scalafmt" // textDocument/formatting binary; else $LS_SCALAFMT / baked
}
```

`textDocument/formatting` needs a workspace-root `.scalafmt.conf` with a
pinned `version`; the baked scalafmt runs offline, so a version mismatch is a
typed error, not a download.

## Hacking on the server itself

Contributor toolchain (all flake-pinned): Rust stable for the host process,
Java 25 only for the presentation-compiler island, Scala 3 exactly pinned,
Mill + [mill-ivy-fetcher](https://github.com/Avimitin/mill-ivy-fetcher) for
the island build. Nix flake + cargo/crane + Mill are the only supported build
entry points.

```bash
nix develop                      # rust toolchain + Java 25 + Mill + mif
cargo test --workspace           # the Rust core
mill __.test                     # the retained JVM island
nix build .#default              # bin/scala3-bsp-semantic-ls + the island agent jar
```

Refresh the dependency lock after changing `build.mill`:

```bash
nix develop -c ./scripts/regen-ivy-lock.sh
./scripts/check-ivy-lock.sh      # CI gate
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
