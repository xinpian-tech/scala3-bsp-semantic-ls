# Vendored real-world workspace: zaozi (pure-Scala subset)

This directory is a **real third-party Scala 3 codebase**, used as a higher-fidelity
workspace for the real-Mill-BSP end-to-end tests (`RealBspZaoziTest`, gated by
`LS_REAL_BSP_IT=1`) — a genuine project rather than a toy fixture.

## Source

Vendored from **https://github.com/xinpian-tech/zaozi** (Apache-2.0, © Jiuyang Liu),
"a Scala-based hardware design framework leveraging MLIR and CIRCT". Only the two
**pure-Scala** modules are vendored — `rvdecoderdb` (a RISC-V instruction decoder
database) and `decoder` (an Espresso/PLA truth-table logic decoder) — because the
rest of zaozi (`circtlib`, `mlirlib`, …) binds native CIRCT/MLIR libraries via the
Panama FFM API and requires zaozi's own Nix toolchain (`circt-nix`, `jextract`),
which is not available in this repository's dev shell.

## Adaptations (`build.mill`)

`ZaoziScalaModule.scalacOptions` was changed from `-java-output-version 25` to
`-Xsemanticdb -sourceroot <workspaceRoot>`, so the modules emit SemanticDB the
language server can index (the upstream build targets a JDK-25 class version that
the stock coursier-resolved Scala 3.7.4 here does not accept). No source file was
modified.

## Note on the presentation compiler

zaozi targets Scala 3.7.4 while this server bundles the 3.8.4 presentation
compiler, so PC completion is version-skewed on this workspace. The
SemanticDB-backed global features (workspace/symbol, references, rename) are
version-independent and are what `RealBspZaoziTest` asserts.
