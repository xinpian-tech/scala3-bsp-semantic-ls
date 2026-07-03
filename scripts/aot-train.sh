#!/usr/bin/env bash
# AOT cache training (plan §16.3 / AC-14): build the server assembly jar, then
# run the JDK-25 two-step (record -> create) against a workspace to produce a
# non-empty AOT cache. The launcher consumes it via LS_AOT_CACHE (nix wrapper)
# or -XX:AOTCache=<path> directly.
#
# Usage:  nix develop -c ./scripts/aot-train.sh [--workspace <dir>] [--out <file>]
#
#   --workspace  workspace to train against (default: it/sample-workspace)
#   --out        output cache path (default: .scala3-bsp-semantic-ls/aot-cache.bin)
set -euo pipefail
cd "$(dirname "$0")/.."

workspace="it/sample-workspace"
out=".scala3-bsp-semantic-ls/aot-cache.bin"
while [ $# -gt 0 ]; do
  case "$1" in
    --workspace) workspace="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    -h|--help) sed -n '2,12p' "$0"; exit 0 ;;
    *) echo "aot-train: unknown argument: $1" >&2; exit 2 ;;
  esac
done

# Reuse a pre-built assembly jar when one is provided (LS_AOT_ASSEMBLY_JAR is
# set by scripts/it-aot.sh so the integration test never nests a `mill` call
# inside the outer `mill` test run); otherwise build it here.
jar="${LS_AOT_ASSEMBLY_JAR:-}"
if [ -z "$jar" ]; then
  mill --no-daemon core.assembly
  jar="out/core/assembly.dest/out.jar"
fi
[ -f "$jar" ] || { echo "aot-train: assembly jar not found: $jar" >&2; exit 1; }

mkdir -p "$(dirname "$out")"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
conf="$tmp/aot.conf"

# Runtime flags must match the production launcher so the resulting cache is
# usable under it (native access for the SQLite FFM layer, compact headers).
runflags=(--enable-native-access=ALL-UNNAMED -XX:+UseCompactObjectHeaders)

echo "aot-train: run 1/2 (record) over $workspace"
java -XX:AOTMode=record -XX:AOTConfiguration="$conf" \
  "${runflags[@]}" -jar "$jar" --aot-train "$workspace"

echo "aot-train: run 2/2 (create) -> $out"
java -XX:AOTMode=create -XX:AOTConfiguration="$conf" -XX:AOTCache="$out" \
  "${runflags[@]}" -jar "$jar" --aot-train "$workspace"

[ -s "$out" ] || { echo "aot-train: cache was not produced: $out" >&2; exit 1; }
echo "aot-train: AOT cache created: $out ($(wc -c < "$out") bytes)"
