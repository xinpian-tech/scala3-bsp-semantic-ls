#!/usr/bin/env bash
# Mechanical documentation checker.
#
# Fails if:
#   - docs/traceability.md is missing;
#   - a test class named in docs/traceability.md does not exist in the tree;
#   - a repo path named in docs/traceability.md does not exist;
#   - a known stale/false claim still appears in docs/ (superseded behavior).
#
# It is RED on the pre-reconciliation docs and on a missing traceability file, and
# GREEN once the docs match the implementation.
#
# Usage:  ./scripts/check-docs.sh   (no Nix/Mill needed)
set -euo pipefail
cd "$(dirname "$0")/.."

fail=0
err() { printf 'check-docs: FAIL: %s\n' "$*" >&2; fail=1; }

trace="docs/traceability.md"
[ -f "$trace" ] || { printf 'check-docs: FAIL: %s is missing (add the traceability map)\n' "$trace" >&2; exit 1; }

# (1) Every test class named in traceability.md must exist as a real suite. Test
#     classes are referenced as bare `XxxSuite` / `XxxTest` identifiers.
while IFS= read -r cls; do
  [ -n "$cls" ] || continue
  if ! grep -rqE "(class|object) ${cls}\b" modules/*/test/src --include=*.scala; then
    err "traceability names test class '${cls}' but no such suite exists"
  fi
done < <(grep -oE '\b[A-Z][A-Za-z0-9]*(Suite|Test)\b' "$trace" | sort -u)

# (2) Every repo path named in traceability.md must exist.
while IFS= read -r p; do
  [ -n "$p" ] || continue
  [ -e "$p" ] || err "traceability names path '${p}' but it does not exist"
done < <(grep -oE '(modules|scripts|docs|nix|it|\.github)/[A-Za-z0-9_./-]+\.[A-Za-z0-9]+' "$trace" | sort -u)

# (2b) Case map: every `Class` :: "case substring" entry must resolve to a test
#      whose file actually contains that substring — so a renamed/removed/typo'd
#      test case breaks the gate (not just a wrong class name).
mapped=0
while IFS= read -r line; do
  cls=$(printf '%s\n' "$line" | sed -nE 's/.*`([A-Za-z0-9]+)` :: ".*/\1/p')
  cse=$(printf '%s\n' "$line" | sed -nE 's/.*:: "(.+)".*/\1/p')
  [ -n "$cls" ] && [ -n "$cse" ] || continue
  mapped=$((mapped + 1))
  f=$(grep -rlE "(class|object) ${cls}\b" modules/*/test/src --include='*.scala' | head -1)
  if [ -z "$f" ]; then
    err "case map: test class '${cls}' not found"
  elif ! grep -Fq -- "$cse" "$f"; then
    err "case map: case \"${cse}\" not found in ${cls} (${f})"
  fi
done < <(grep -E '`[A-Za-z0-9]+` :: "' "$trace")
[ "$mapped" -ge 30 ] || err "case map has only ${mapped} entries; expected the rename-rule / correctness-case / benchmark / real-BSP E-row map"

# (3) Stale/false claims that F1 must purge. Grep everything under docs/ EXCEPT
#     traceability.md itself (which documents the evolution old->new by design).
stale() { # <extended-regex> <why>
  local hit
  if hit=$(grep -rniE "$1" docs/ --include='*.md' | grep -v '^docs/traceability.md:' | head -1); then
    [ -n "$hit" ] && err "stale claim ($2): ${hit}"
  fi
  return 0
}
stale 'semanticdb watcher' 'no watcher; the lifecycle is compile -> reindex full rescan (v1)'
stale 'current gaps \(planned items\)' 'nix-build "Current gaps" lists files that now exist'
stale 'mark target .?indexunavailable' 'a source without SemanticDB is now a hard NoSemanticdb error, not a tolerated IndexUnavailable state'
stale 'currently export' 'the flake DOES export .#mill / .#mill-ivy-fetcher'
stale 'runs three checks' 'nix flake check runs FOUR checks (java25, ivy-lock, mif-input, package)'
stale 'mill -i core' 'the package build uses `mill --no-daemon core.assembly`, not `mill -i`'
stale 'postings/segment-N/' 'storage layout is postings/segments/segment-NNNNNN/'

# (4) Positive assertions: docs must state the current accepted behavior.
grep -qiE 'no[ -]?semanticdb|semanticdb coverage: error|hard error' docs/architecture.md \
  || err "architecture.md must document the hard NoSemanticdb error for sources without SemanticDB"
grep -qE '\.bsp' docs/nix-build.md \
  || err "nix-build.md AOT docs must state the strict-vs-lenient .bsp distinction"

if [ "$fail" -eq 0 ]; then
  printf 'check-docs: OK (traceability entries resolve; no stale claims)\n'
else
  exit 1
fi
