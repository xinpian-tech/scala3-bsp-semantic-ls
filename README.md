# scala3-bsp-semantic-ls

A special-purpose language server for **Scala 3 + BSP** projects. It does not
aim for Metals' generality; it aims for exact, high-performance answers to
three workspace-wide questions:

1. `workspace/symbol`
2. whole-repo `textDocument/references`
3. cross-file `textDocument/rename`

Workspace-wide semantic truth comes **only** from scalac-generated SemanticDB.
SQLite (via the Java 25 FFM API) and immutable mmap postings segments are
materialized indexes over that truth — never approximations. The Scala 3
presentation compiler serves interactive editing (completion, hover, signature
help, definition, dirty-buffer overlay) and is never a source of persistent
index data.

See [plan.md](plan.md) for the full rationale and
[docs/architecture.md](docs/architecture.md),
[docs/index-format.md](docs/index-format.md),
[docs/plugin-spi.md](docs/plugin-spi.md),
[docs/nix-build.md](docs/nix-build.md) for the normative contracts.
[docs/deployment.md](docs/deployment.md) is the operator guide: packaging, AOT,
BSP/editor integration, and manual verification.

## Toolchain contract

- Java 25 only (FFM SQLite binding, MemorySegment mmap, AOT cache, Compact
  Object Headers)
- Scala 3 only, exactly pinned
- Nix flake + Mill + [mill-ivy-fetcher](https://github.com/Avimitin/mill-ivy-fetcher)
  are the only supported build entry points (Nix >= 2.28)

## Quickstart

```bash
nix develop                      # Java 25 + Mill 1.1.2 + mif + SQLite
mill __.compile
mill __.test
```

Build the package:

```bash
nix build .#default
```

Refresh the dependency lock after changing `build.mill`:

```bash
nix develop -c mif run -p . -o nix/ivy-lock.nix
./scripts/check-ivy-lock.sh      # CI gate
```

## Module map

| Mill module  | Directory               | Role                                        |
|--------------|-------------------------|---------------------------------------------|
| `indexModel` | `modules/ls-index-model`| shared ids, spans, snapshot contract        |
| `semanticdb` | `modules/ls-semanticdb` | SemanticDB locator/parser/normalizer/groups |
| `sqliteFfm`  | `modules/ls-sqlite-ffm` | SQLite metadata store over Java 25 FFM      |
| `postings`   | `modules/ls-postings`   | immutable mmap postings segments + snapshots|
| `bsp`        | `modules/ls-bsp`        | BSP client, project model, target graph     |
| `pc`         | `modules/ls-pc`         | presentation compiler worker + plugin SPI   |
| `rename`     | `modules/ls-rename`     | ingest pipeline, references + rename engines|
| `doctor`     | `modules/ls-doctor`     | doctor report                               |
| `core`       | `modules/ls-core`       | LSP server wiring (`ls.core.Main`)          |
| `bench`      | `modules/ls-bench`      | benchmark + JFR harness (`bench.smoke`)     |
