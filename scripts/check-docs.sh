#!/usr/bin/env bash
# Mechanical documentation checker (v2: Rust core + JVM PC island).
#
# Fails if:
#   - docs/traceability.md is missing;
#   - a repo path named in docs/traceability.md does not exist;
#   - a Rust test file or Scala test class named in docs/traceability.md does
#     not exist;
#   - a `path` :: "case substring" entry does not resolve to a real test;
#   - a known stale/false claim (superseded Scala-era behavior) still appears
#     in docs/.
#
# Usage:  ./scripts/check-docs.sh   (no Nix/cargo/mill needed)
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
err() { printf 'check-docs: FAIL: %s\n' "$*" >&2; fail=1; }

trace="docs/traceability.md"
[ -f "$trace" ] || { printf 'check-docs: FAIL: %s is missing (add the traceability map)\n' "$trace" >&2; exit 1; }

# (1) Every repo path named in traceability.md must exist.
while IFS= read -r p; do
  [ -n "$p" ] || continue
  [ -e "$p" ] || err "traceability names path '${p}' but it does not exist"
done < <(grep -oE '(crates|modules|scripts|docs|nix|it|\.github)/[A-Za-z0-9_./-]+' "$trace" | sed 's/[.,;)]*$//' | sort -u)

# (2) Every Scala test class named in traceability.md must exist as a real
#     suite in the retained island tree.
while IFS= read -r cls; do
  [ -n "$cls" ] || continue
  if ! grep -rqE "(class|object) ${cls}\b" modules/*/test/src --include='*.scala'; then
    err "traceability names test class '${cls}' but no such suite exists"
  fi
done < <(grep -oE '\b[A-Z][A-Za-z0-9]*(Suite|Test)\b' "$trace" | sort -u)

# (3) Case map: every `path` :: "case substring" entry must resolve to a test
#     file that actually contains the substring — so a renamed/removed test
#     breaks the gate, not just a wrong file name.
mapped=0
while IFS= read -r line; do
  f=$(printf '%s\n' "$line" | sed -nE 's/.*`([A-Za-z0-9_./-]+)` :: ".*/\1/p')
  cse=$(printf '%s\n' "$line" | sed -nE 's/.*:: "(.+)".*/\1/p')
  [ -n "$f" ] && [ -n "$cse" ] || continue
  mapped=$((mapped + 1))
  if [ ! -f "$f" ]; then
    err "case map: file '${f}' not found"
  elif ! grep -Fq -- "$cse" "$f"; then
    err "case map: case \"${cse}\" not found in ${f}"
  fi
done < <(grep -E '`[A-Za-z0-9_./-]+` :: "' "$trace")
[ "$mapped" -ge 10 ] || err "case map has only ${mapped} entries; expected the recovery / matrix / boundary / e2e anchors"

# (4) Stale/false claims from the deleted Scala implementation. Grep everything
#     under docs/ EXCEPT traceability.md and coverage-audit.md (both document
#     the evolution old->new by design).
stale() { # <extended-regex> <why>
  local hit
  if hit=$(grep -rniE -- "$1" docs/ --include='*.md' \
      | grep -v '^docs/traceability.md:' \
      | grep -v '^docs/coverage-audit.md:' | head -1); then
    [ -n "$hit" ] && err "stale claim ($2): ${hit}"
  fi
  return 0
}
stale 'meta\.sqlite' 'the SQLite metadata store was removed (immutable segments + manifest.json + workspace-state)'
stale 'LS_SQLITE_LIB' 'the SQLite FFM binding was removed with the Scala core'
stale 'ForkedPcWorker|PcWorkerMain|forked worker' 'the forked PC worker was deleted; the island is embedded in-process'
stale '\-\-in-process-pc|\-\-forked-pc' 'the PC backend selection flags were removed with the Scala CLI'
stale 'core\.assembly' 'the Scala server assembly no longer exists; the binary is the crane-built ls-server'
stale 'AotTrain|aot-train' 'AOT training was deleted with the Scala core'
stale 'workspace_symbols_fts|fts5' 'FTS5 search was replaced by the deterministic segment-resident search section'
stale 'semanticdb watcher' 'no watcher; the lifecycle is compile -> reindex full rescan'
stale 'postings/segment-N/' 'storage layout is postings/segments/segment-NNNNNN/'

# (5) Positive assertions: docs must state the current accepted behavior.
grep -qiE 'no[ -]?semanticdb|semanticdb coverage: error|hard error' docs/architecture.md \
  || err "architecture.md must document the hard NoSemanticdb error for sources without SemanticDB"
grep -q 'manifest.json' docs/architecture.md \
  || err "architecture.md must document the manifest.json single commit point"
grep -q 'workspace-state' docs/architecture.md \
  || err "architecture.md must document the generational workspace-state pairing"
grep -qE 'JNI_CreateJavaVM' docs/architecture.md \
  || err "architecture.md must document the single-boot-symbol embedded JVM island"
grep -qE '\.bsp' docs/nix-build.md \
  || err "nix-build.md must document the BSP discovery expectation"
grep -q 'javaHome' docs/deployment.md \
  || err "deployment.md must document the config > env > nix-baked Java home resolution"
grep -q 'dump' docs/deployment.md \
  || err "deployment.md must document the dump store-inspection subcommand"

if [ "$fail" -eq 0 ]; then
  printf 'check-docs: OK (traceability entries resolve; no stale claims)\n'
else
  exit 1
fi
