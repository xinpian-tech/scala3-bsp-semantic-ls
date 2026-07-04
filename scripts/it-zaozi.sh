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
# It provisions TWO independent zaozi workspaces so the plugin and no-plugin
# baseline probes never share a Mill-BSP connection: a second sequential BSP
# connection to the SAME workspace flakily fails to resolve zaozi's native env
# inputs (mlirlib.mlirInstallPath / libcIncludePath). Isolated workspaces sidestep
# that entirely, so the no-plugin baseline is a real run, not an inferred one.
#
# Usage:  nix develop -c ./scripts/it-zaozi.sh
set -euo pipefail
cd "$(dirname "$0")/.."
REPO="$PWD"

: "${ZAOZI_SRC:?ZAOZI_SRC unset — run inside 'nix develop' (flake exposes the pinned zaozi source)}"
SQLITE="${LS_SQLITE_LIB:?LS_SQLITE_LIB unset — run inside 'nix develop'}"
PROBE_SYMBOL="${ZAOZI_PROBE_SYMBOL:-ConversionCreateApi}"

# Build OUR server assembly and the zaozi PC plugin jar (a plain compiler-plugin
# jar loaded into our presentation compiler via pc-plugins.json). Separate `mill`
# invocations: a single call with two targets silently skips the trailing one.
mill --no-daemon core.assembly
mill --no-daemon zaoziPcplugin.jar
JAR="$REPO/out/core/assembly.dest/out.jar"
PLUGIN_JAR="$REPO/out/zaoziPcplugin/jar.dest/out.jar"
[ -f "$JAR" ] || { echo "it-zaozi: assembly jar not found: $JAR" >&2; exit 1; }
[ -f "$PLUGIN_JAR" ] || { echo "it-zaozi: zaozi-pcplugin jar not found: $PLUGIN_JAR" >&2; exit 1; }

# Provision one writable zaozi workspace: copy the pinned+patched Nix-store source
# into `$1`, build the full zaozi (native CIRCT/MLIR) inside ITS OWN nix dev shell
# emitting SemanticDB, install the Mill BSP connection, and wrap that connection so
# `mill --bsp` runs inside zaozi's nix env. The `path:` flake scheme is required
# because the workspace is a plain store copy (not a git checkout), so a bare path
# would make Nix search upward and mis-root the flake at /tmp.
provision_zaozi() {
  local dst="$1"
  mkdir -p "$dst"
  cp -r "$ZAOZI_SRC/." "$dst/"
  chmod -R u+w "$dst"
  grep -q "Xsemanticdb" "$dst/build.mill" || { echo "it-zaozi: patched source lacks -Xsemanticdb" >&2; exit 1; }
  ( cd "$dst"
    rm -rf .bsp .scala3-bsp-semantic-ls
    nix develop "path:$dst" -c bash -c 'mill --no-daemon __.compile && mill --no-daemon mill.bsp.BSP/install'
  )
  local sdb; sdb=$(find "$dst/out" -name '*.semanticdb' 2>/dev/null | wc -l)
  [ "$sdb" -gt 0 ] || { echo "it-zaozi: $dst produced no SemanticDB" >&2; exit 1; }
  python3 - "$dst" <<'PY'
import json,sys
z=sys.argv[1]; p=z+"/.bsp/mill-bsp.json"; d=json.load(open(p))
# `path:` scheme (see the compile step): the workspace is a plain store copy, not
# a git checkout, so a bare path would make Nix mis-root the flake at /tmp.
if d["argv"][:2] != ["nix","develop"]:
    d["argv"]=["nix","develop","path:"+z,"-c"]+d["argv"]
    json.dump(d,open(p,"w"),indent=2)
PY
  echo "it-zaozi: provisioned zaozi workspace at $dst ($sdb SemanticDB files)"
}

# Two isolated workspaces: one loads the plugin (positive probe), one does not
# (negative/baseline probe). They never share a BSP connection.
ROOT="$(mktemp -d)"
WORK="$ROOT/zaozi-plugin"
BASE_WORK="$ROOT/zaozi-baseline"
provision_zaozi "$WORK"
provision_zaozi "$BASE_WORK"

# Configure OUR presentation compiler to load the zaozi PC plugin in the PLUGIN
# workspace only, exactly as a user would: a workspace pc-plugins.json naming the
# plugin jar as a compiler plugin. Written AFTER the build (whose `rm -rf
# .scala3-bsp-semantic-ls` above would otherwise delete it). In-process PC loads
# this in the main JVM; a forked PC child loads it via --plugin-config — both from
# this same file. The BASE_WORK workspace gets NO pc-plugins.json, so its PC runs
# without the plugin.
PLUGIN_CFG="$WORK/.scala3-bsp-semantic-ls/pc-plugins.json"
mkdir -p "$(dirname "$PLUGIN_CFG")"
python3 - "$PLUGIN_CFG" "$PLUGIN_JAR" <<'PY'
import json,sys
cfg,jar=sys.argv[1],sys.argv[2]
json.dump({"compilerPlugins":[{"jars":[jar]}]}, open(cfg,"w"), indent=2)
PY
echo "it-zaozi: wrote PC plugin config $PLUGIN_CFG -> $PLUGIN_JAR"

# Drive the server against the full real zaozi: real BSP compile + reindex, then
# assert the SemanticDB-backed index is populated and queryable, generic PC
# completion returns items, AND the PC answers the zaozi-pcplugin go-to+hover on
# the `io.a` / `io.f.g` / `io.k` Dynamic bundle-field accesses in
# zaozi/tests/src/BundleSpec.scala. No `--skip-pc`: the generic completion probe
# now tries member-selects in deterministic order until one completes, so a
# macro-retained empty select no longer forces skipping the whole PC path.
echo "it-zaozi: [plugin] indexing zaozi over real Mill BSP + PC nav probe (probe: $PROBE_SYMBOL)"
LS_SQLITE_LIB="$SQLITE" java --enable-native-access=ALL-UNNAMED -jar "$JAR" \
  --aot-train "$WORK" --require-index --zaozi-nav-probe

# The no-plugin BASELINE, in its OWN workspace: go-to on `io.a` must resolve to
# the framework `selectDynamic` (NOT `val a`), proving the plugin is the cause.
echo "it-zaozi: [baseline] indexing zaozi over real Mill BSP + PC nav baseline (no plugin)"
LS_SQLITE_LIB="$SQLITE" java --enable-native-access=ALL-UNNAMED -jar "$JAR" \
  --aot-train "$BASE_WORK" --require-index --zaozi-nav-probe --zaozi-nav-baseline

echo "it-zaozi: OK — plugin run resolved io.a -> val a / io.f.g / io.k; baseline resolved io.a -> selectDynamic"
