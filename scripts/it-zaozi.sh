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

# Build OUR server assembly and the zaozi PC plugin jar (a plain compiler-plugin
# jar loaded into our presentation compiler via pc-plugins.json). Separate `mill`
# invocations: a single call with two targets silently skips the trailing one.
mill --no-daemon core.assembly
mill --no-daemon zaoziPcplugin.jar
JAR="$REPO/out/core/assembly.dest/out.jar"
PLUGIN_JAR="$REPO/out/zaoziPcplugin/jar.dest/out.jar"
[ -f "$JAR" ] || { echo "it-zaozi: assembly jar not found: $JAR" >&2; exit 1; }
[ -f "$PLUGIN_JAR" ] || { echo "it-zaozi: zaozi-pcplugin jar not found: $PLUGIN_JAR" >&2; exit 1; }

# Copy the pinned + patched zaozi source out of the read-only Nix store into a
# writable workspace (mill writes out/, .bsp/ there).
WORK="$(mktemp -d)/zaozi"
mkdir -p "$WORK"
cp -r "$ZAOZI_SRC/." "$WORK/"
chmod -R u+w "$WORK"
grep -q "Xsemanticdb" "$WORK/build.mill" || { echo "it-zaozi: patched source lacks -Xsemanticdb" >&2; exit 1; }
echo "it-zaozi: pinned+patched zaozi source at $WORK"

# Build the full zaozi (native CIRCT/MLIR) inside ITS OWN nix dev shell, emitting
# SemanticDB, and install the real Mill BSP connection. The workspace is a plain
# copy of the Nix-store source (not a git checkout), so the flake is addressed
# with the explicit `path:` scheme — a bare path would make Nix search upward for
# an enclosing git repo and mis-root the flake at /tmp.
( cd "$WORK"
  rm -rf .bsp .scala3-bsp-semantic-ls
  nix develop "path:$WORK" -c bash -c 'mill --no-daemon __.compile && mill --no-daemon mill.bsp.BSP/install'
)
sdb=$(find "$WORK/out" -name '*.semanticdb' 2>/dev/null | wc -l)
echo "it-zaozi: zaozi built with $sdb SemanticDB files"
[ "$sdb" -gt 0 ] || { echo "it-zaozi: zaozi produced no SemanticDB" >&2; exit 1; }

# Configure OUR presentation compiler to load the zaozi PC plugin, exactly as a
# user would: a workspace pc-plugins.json naming the plugin jar as a compiler
# plugin. Written AFTER the build (whose `rm -rf .scala3-bsp-semantic-ls` above
# would otherwise delete it). In-process PC loads this in the main JVM; a forked
# PC child would load it via --plugin-config — both from this same file.
PLUGIN_CFG="$WORK/.scala3-bsp-semantic-ls/pc-plugins.json"
mkdir -p "$(dirname "$PLUGIN_CFG")"
python3 - "$PLUGIN_CFG" "$PLUGIN_JAR" <<'PY'
import json,sys
cfg,jar=sys.argv[1],sys.argv[2]
json.dump({"compilerPlugins":[{"jars":[jar]}]}, open(cfg,"w"), indent=2)
PY
echo "it-zaozi: wrote PC plugin config $PLUGIN_CFG -> $PLUGIN_JAR"

# Wrap the BSP launch so `mill --bsp` runs inside zaozi's nix env (CIRCT/MLIR),
# while OUR server runs in ITS OWN nix env (our SQLite). Mixing the two native
# closures in one process crashes; wrapping keeps each in its own dev shell.
python3 - "$WORK" <<'PY'
import json,sys
z=sys.argv[1]; p=z+"/.bsp/mill-bsp.json"; d=json.load(open(p))
# `path:` scheme (see the compile step): the workspace is a plain store copy, not
# a git checkout, so a bare path would make Nix mis-root the flake at /tmp.
if d["argv"][:2] != ["nix","develop"]:
    d["argv"]=["nix","develop","path:"+z,"-c"]+d["argv"]
    json.dump(d,open(p,"w"),indent=2)
PY

# Drive the server against the full real zaozi: real BSP compile + reindex, then
# assert the SemanticDB-backed index is populated and queryable AND that the PC
# answers the zaozi-pcplugin go-to+hover on the `io.a` / `io.f.g` / `io.k` Dynamic
# bundle-field accesses in zaozi/tests/src/BundleSpec.scala.
#
# `--skip-pc` skips only the GENERIC PC-completion probe (an arbitrary member-select
# via findSelectProbe): general PC completion on zaozi's utest/@generator macro-heavy
# code lands inside a macro's retained tree copy and returns nothing — a pre-existing
# dotty-PC-on-macro limitation the plugin does not (and is not meant to) address. The
# dedicated `--zaozi-nav-probe` still runs and is the real go-to assertion: it drives
# textDocument/definition + hover on the Dynamic accesses (the plugin steers those
# through the same macro-retained copies via its retained-call rewrite).
echo "it-zaozi: [plugin] indexing zaozi over real Mill BSP + PC nav probe (probe: $PROBE_SYMBOL)"
LS_SQLITE_LIB="$SQLITE" java --enable-native-access=ALL-UNNAMED -jar "$JAR" \
  --aot-train "$WORK" --require-index --skip-pc --zaozi-nav-probe

# The no-plugin BASELINE (io.a does NOT resolve to the field without the plugin) is
# covered by the unit + forked suites (ZaoziPcNavSuite "without it, it does not";
# ZaoziPcForkedSuite baseline resolves to selectDynamic) — both exercise the real PC.
# A second real-zaozi aot-train run here (plugin disabled) is intentionally NOT done:
# a SECOND sequential Mill-BSP connection to the same workspace flakily fails to
# resolve zaozi's native env inputs (mlirlib.mlirInstallPath / libcIncludePath) in the
# re-launched BSP server — a Mill-BSP-in-nix flake unrelated to the plugin.

echo "it-zaozi: OK — the server indexed the full original zaozi and PC nav resolved io.a -> val a / io.f.g / io.k"
