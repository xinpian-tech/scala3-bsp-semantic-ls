#!/usr/bin/env bash
# CI gate: nix/ivy-lock.nix must match what `mif run` generates for the
# current build.mill. Run inside `nix develop` (needs mill + mill-ivy-fetcher).
set -euo pipefail

cd "$(dirname "$0")/.."

if [[ ! -f nix/ivy-lock.nix ]]; then
  echo "error: nix/ivy-lock.nix is missing. Generate it with:" >&2
  echo "  nix develop -c mif run -p . -o nix/ivy-lock.nix" >&2
  exit 1
fi

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mif run -p . -o "$tmp/ivy-lock.nix"

if ! diff -u nix/ivy-lock.nix "$tmp/ivy-lock.nix"; then
  echo "error: nix/ivy-lock.nix is stale relative to build.mill." >&2
  echo "Regenerate it with: nix develop -c mif run -p . -o nix/ivy-lock.nix" >&2
  exit 1
fi

echo "ivy-lock.nix is up to date."
