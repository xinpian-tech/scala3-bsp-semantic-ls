#!/usr/bin/env bash
# Real-repo validation against the FULL, UNMODIFIED zaozi
# (https://github.com/xinpian-tech/zaozi) built with its OWN Nix toolchain.
#
# zaozi is a pinned flake input of THIS repo (see flake.nix `inputs.zaozi`); the
# only local change — enabling SemanticDB emission, which our SemanticDB-first
# server requires — is maintained as nix/patches/zaozi-semanticdb.patch and
# applied by the flake. The dev shell exposes the patched, pinned source as
# ZAOZI_SRC, so this script never does an ad-hoc `git clone` or in-place edit.
#
# Unlike scripts/it-real-bsp.sh (deterministic toy workspace), this drives the
# server against a genuine 260+-file Scala 3 hardware framework whose core
# modules bind native CIRCT/MLIR through the Panama FFM API — provisioned by
# zaozi's own flake (`nix develop`), which supplies CIRCT/MLIR/jextract from the
# binary cache. It is a HEAVY manual validation, NOT part of the ordinary CI gate.
#
# Usage:  nix develop -c ./scripts/it-zaozi.sh
set -euo pipefail
cd "$(dirname "$0")/.."
REPO="$PWD"

: "${ZAOZI_SRC:?ZAOZI_SRC unset — run inside 'nix develop' (flake exposes the pinned zaozi source)}"
SQLITE="${LS_SQLITE_LIB:?LS_SQLITE_LIB unset — run inside 'nix develop'}"
PROBE_SYMBOL="${ZAOZI_PROBE_SYMBOL:-ConversionCreateApi}"

# Build OUR server assembly.
mill --no-daemon core.assembly
JAR="$REPO/out/core/assembly.dest/out.jar"
[ -f "$JAR" ] || { echo "it-zaozi: assembly jar not found: $JAR" >&2; exit 1; }

# Copy the pinned + patched zaozi source out of the read-only Nix store into a
# writable workspace (mill writes out/, .bsp/ there).
WORK="$(mktemp -d)/zaozi"
mkdir -p "$WORK"
cp -r "$ZAOZI_SRC/." "$WORK/"
chmod -R u+w "$WORK"
grep -q "Xsemanticdb" "$WORK/build.mill" || { echo "it-zaozi: patched source lacks -Xsemanticdb" >&2; exit 1; }
echo "it-zaozi: pinned+patched zaozi source at $WORK"

# Build the full zaozi (native CIRCT/MLIR) inside ITS OWN nix dev shell, emitting
# SemanticDB, and install the real Mill BSP connection.
( cd "$WORK"
  rm -rf .bsp .scala3-bsp-semantic-ls
  nix develop "$WORK" -c bash -c 'mill --no-daemon __.compile && mill --no-daemon mill.bsp.BSP/install'
)
sdb=$(find "$WORK/out" -name '*.semanticdb' 2>/dev/null | wc -l)
echo "it-zaozi: zaozi built with $sdb SemanticDB files"
[ "$sdb" -gt 0 ] || { echo "it-zaozi: zaozi produced no SemanticDB" >&2; exit 1; }

# Wrap the BSP launch so `mill --bsp` runs inside zaozi's nix env (CIRCT/MLIR),
# while OUR server runs in ITS OWN nix env (our SQLite). Mixing the two native
# closures in one process crashes; wrapping keeps each in its own dev shell.
python3 - "$WORK" <<'PY'
import json,sys
z=sys.argv[1]; p=z+"/.bsp/mill-bsp.json"; d=json.load(open(p))
if d["argv"][:2] != ["nix","develop"]:
    d["argv"]=["nix","develop",z,"-c"]+d["argv"]
    json.dump(d,open(p,"w"),indent=2)
PY

# Drive the server against the full real zaozi: real BSP compile + reindex, then
# assert the SemanticDB-backed index is populated and queryable (workspace/symbol
# + references). PC completion is version-skewed (zaozi is Scala 3.7.x, our PC is
# 3.8.x) so it is skipped; the index features are version-independent.
echo "it-zaozi: indexing the full zaozi over real Mill BSP (probe: $PROBE_SYMBOL)"
LS_SQLITE_LIB="$SQLITE" java --enable-native-access=ALL-UNNAMED -jar "$JAR" \
  --aot-train "$WORK" --require-index --skip-pc

echo "it-zaozi: OK — the server indexed the full original zaozi"
