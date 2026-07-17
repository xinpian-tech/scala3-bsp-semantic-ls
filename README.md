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
nix build .#default              # bin/scala3-bsp-semantic-ls + island jars
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
| `zaoziPcplugin` | `modules/ls-zaozi-pcplugin`| zaozi PC navigation plugin (`-Xplugin`)   |
