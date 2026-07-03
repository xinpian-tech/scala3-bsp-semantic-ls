#!/usr/bin/env bash
# Real-repo validation against the FULL, UNMODIFIED zaozi
# (https://github.com/xinpian-tech/zaozi) built with its OWN Nix toolchain.
#
# Unlike scripts/it-real-bsp.sh (deterministic toy workspace), this drives the
# language server against a genuine 260+-file Scala 3 hardware framework whose
# core modules bind native CIRCT/MLIR through the Panama FFM API — provisioned
# entirely by zaozi's flake (`nix develop`), which supplies CIRCT, MLIR and
# jextract from the binary cache.
#
# It is a HEAVY, network-dependent, manual validation (clones zaozi, builds the
# native toolchain, compiles 260+ files); it is NOT part of the ordinary CI gate.
#
# Usage:
#   nix develop -c ./scripts/it-zaozi.sh                 # clone + build + validate
#   ZAOZI_DIR=/path/to/zaozi nix develop -c ./scripts/it-zaozi.sh   # reuse a checkout
#
# The ONLY change made to zaozi is enabling SemanticDB emission (a scalac flag,
# added ALONGSIDE the upstream options; no source file is touched) — our server
# is SemanticDB-first, so the workspace must emit `.semanticdb`.
set -euo pipefail
cd "$(dirname "$0")/.."
REPO="$PWD"

ZAOZI_URL="https://github.com/xinpian-tech/zaozi.git"
WORK="${ZAOZI_DIR:-$(mktemp -d)/zaozi}"
PROBE_SYMBOL="${ZAOZI_PROBE_SYMBOL:-ConversionCreateApi}"

if [ ! -d "$WORK/.git" ]; then
  echo "it-zaozi: cloning original zaozi -> $WORK"
  git clone --depth 1 "$ZAOZI_URL" "$WORK"
fi

# Build OUR server assembly + capture the Nix-provided SQLite the FFM layer needs.
mill --no-daemon core.assembly
JAR="$REPO/out/core/assembly.dest/out.jar"
[ -f "$JAR" ] || { echo "it-zaozi: assembly jar not found: $JAR" >&2; exit 1; }
SQLITE="${LS_SQLITE_LIB:?LS_SQLITE_LIB unset — run inside 'nix develop'}"

# Enable SemanticDB on the ORIGINAL zaozi: add -Xsemanticdb -sourceroot alongside
# the upstream scalacOptions (idempotent; no source file changed).
if ! grep -q "Xsemanticdb" "$WORK/build.mill"; then
  echo "it-zaozi: enabling SemanticDB emission in zaozi/build.mill"
  python3 - "$WORK" <<'PY'
import sys
root=sys.argv[1]; p=root+"/build.mill"; s=open(p).read()
needle='super.scalacOptions() ++ Seq("-java-output-version", "25")'
repl='super.scalacOptions() ++ Seq("-java-output-version", "25", "-Xsemanticdb", "-sourceroot", mill.api.BuildCtx.workspaceRoot.toString)'
assert needle in s, "upstream scalacOptions anchor not found — zaozi build.mill changed"
open(p,"w").write(s.replace(needle,repl))
PY
fi

# Build the full zaozi (native CIRCT/MLIR modules) inside ITS OWN nix dev shell,
# producing SemanticDB, and install the real Mill BSP connection.
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
