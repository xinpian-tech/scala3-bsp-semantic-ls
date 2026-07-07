#!/usr/bin/env bash
# Mechanically verify that docs/coverage-audit.md accounts for EVERY retained
# Scala test file under modules/*/test/. Every path must appear verbatim in the
# audit (as a row or an explicit support-file classification), so a newly added
# or renamed suite cannot be silently dropped from the coverage gate. Paths are
# matched in full, so duplicate basenames (e.g. the two Jdk25GuardSuite.scala)
# cannot collapse into one row.
set -euo pipefail
cd "$(dirname "$0")/.."

AUDIT=docs/coverage-audit.md
if [[ ! -f $AUDIT ]]; then
  echo "check-audit-inventory: $AUDIT not found" >&2
  exit 1
fi

lister() {
  if command -v rg >/dev/null 2>&1; then
    rg --files modules | rg '/test/.*\.scala$'
  else
    find modules -path '*/test/*' -name '*.scala'
  fi
}

missing=0
total=0
while IFS= read -r path; do
  total=$((total + 1))
  if ! grep -qF "$path" "$AUDIT"; then
    echo "MISSING from $AUDIT: $path" >&2
    missing=$((missing + 1))
  fi
done < <(lister | sort)

if [[ $missing -eq 0 ]]; then
  echo "check-audit-inventory: all $total retained Scala test files are accounted for in $AUDIT."
else
  echo "check-audit-inventory: $missing of $total retained Scala test files are MISSING from $AUDIT." >&2
  exit 1
fi
