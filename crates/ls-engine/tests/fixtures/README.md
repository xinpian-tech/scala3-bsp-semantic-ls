# `ls-engine` correctness-matrix fixtures

Pinned-scalac SemanticDB corpus driving the references / rename / document-highlight
correctness matrix. Three targets sharing one `sourceroot` (`sources/`):

- **fixture-a** (`out-a/`) — the definition tree (classes, traits, enums, case
  classes, vars, locals, overloads, extensions, givens, an override family, plus
  generated/readonly/dependency-marked docs) and a shared source.
- **fixture-b** (`out-b/`) — depends on A (compiled with A's output on the
  classpath); references A's symbols; also compiles the shared source.
- **fixture-c** (`out-c/`) — disconnected; deliberately reuses A's
  package/class names (`pkga.Core`) to prove target pruning.

`shared/src/shared/Shared.scala` is compiled by both A and B (primary owner = A,
first in workspace order).

## Provenance

The sources are transcribed verbatim from the Scala `FixtureWorkspace` fixture
(formerly `modules/ls-rename/test/src/ls/rename/FixtureWorkspace.scala`; the
Scala tree was deleted at the rewrite cutover). Each target tree
was compiled once with pinned **Scala 3.8.4** and `-Xsemanticdb`, sharing one
`-sourceroot`, mirroring `FixtureWorkspace.compileAll`:

```
scalac -Xsemanticdb -sourceroot sources -d out-a  -classpath <scala3-library>  <a/** + dep/** + shared/**>
scalac -Xsemanticdb -sourceroot sources -d out-b  -classpath <scala3-library>:out-a  <b/** + shared/**>
scalac -Xsemanticdb -sourceroot sources -d out-c  -classpath <scala3-library>  <c/**>
```

Only `META-INF/semanticdb/**.semanticdb` is retained (`.tasty`/`.class` output is
dropped). The committed sources are byte-identical to the ones compiled, so the
SemanticDB md5 validates and token spans align exactly.

To regenerate after editing sources, recompile each tree with the same pinned
compiler and the same `-sourceroot`, then keep only the `META-INF/semanticdb`
subtree.
