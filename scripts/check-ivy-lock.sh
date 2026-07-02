#!/usr/bin/env bash
# CI gate: nix/ivy-lock.nix must match what scripts/regen-ivy-lock.sh
# generates for the current build.mill. Run inside `nix develop`.
set -euo pipefail

cd "$(dirname "$0")/.."

if [[ ! -f nix/ivy-lock.nix ]]; then
  echo "error: nix/ivy-lock.nix is missing. Generate it with:" >&2
  echo "  nix develop -c ./scripts/regen-ivy-lock.sh" >&2
  exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

cp nix/ivy-lock.nix "$tmp/committed-ivy-lock.nix"
./scripts/regen-ivy-lock.sh
mv nix/ivy-lock.nix "$tmp/regenerated-ivy-lock.nix"
cp "$tmp/committed-ivy-lock.nix" nix/ivy-lock.nix

if ! diff -u "$tmp/committed-ivy-lock.nix" "$tmp/regenerated-ivy-lock.nix"; then
  echo "error: nix/ivy-lock.nix is stale relative to build.mill." >&2
  echo "Regenerate it with: nix develop -c ./scripts/regen-ivy-lock.sh" >&2
  exit 1
fi

echo "ivy-lock.nix is up to date."
