#!/usr/bin/env bash
# Project-level e2e over a REAL third-party repo with a REAL editor client:
# headless Neovim attaches the production ls-server to the pinned, patched
# zaozi checkout (flake input, exported as ZAOZI_SRC by the dev shell), and
# it/nvim/e2e.lua drives readiness, reindex, workspace/symbol, definition,
# references, and PC-backed hover.
#
# The compile targets the pure-JVM `decoder` module only, so the run needs no
# CIRCT/MLIR native toolchain (zaozi's own flake) — just mill + JDK from this
# repo's dev shell. Maven Central access is required for zaozi's dependencies.
#
# Usage:  nix develop -c ./scripts/it-nvim-zaozi.sh
#   LS_NVIM_PROJECT_DIR=/path/to/checkout  drive a different real project
#   LS_NVIM_FILE / LS_NVIM_TOKEN           override the query anchor
set -euo pipefail
cd "$(dirname "$0")/.."

: "${ZAOZI_SRC:?ZAOZI_SRC is unset — run inside 'nix develop' (the flake exports the pinned zaozi source)}"
command -v nvim >/dev/null || { echo "nvim not on PATH — re-enter 'nix develop'" >&2; exit 1; }
command -v mill >/dev/null || { echo "mill not on PATH — re-enter 'nix develop'" >&2; exit 1; }

cargo build -p ls-server
bin="$PWD/target/debug/ls-server"

ws="${LS_NVIM_PROJECT_DIR:-}"
if [ -z "$ws" ]; then
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  cp -r --no-preserve=mode,ownership "$ZAOZI_SRC" "$tmp/zaozi"
  ws="$tmp/zaozi"
  # Keep only the CIRCT-free modules: the BSP model load evaluates
  # `buildTarget/scalacOptions` for EVERY Scala target, and the Panama modules
  # (circtlib/mlirlib and their dependents) fail that evaluation without
  # CIRCT_INSTALL_PATH from zaozi's own native toolchain. The retained
  # decoder + rvdecoderdb modules are real, standalone project sources.
  (cd "$ws" && rm -rf circtlib mlirlib omlib smtlib stdlib testlib zaozi)
fi

file="${LS_NVIM_FILE:-decoder/src/TruthTable.scala}"
token="${LS_NVIM_TOKEN:-BitSet}"

echo "=== preparing real workspace: $ws ==="
(
  cd "$ws"
  # Only the connection file: the compile itself runs over the server's own
  # retained BSP session (the session's out-dir view is authoritative; a CLI
  # pre-compile would write to a different out root and be invisible to it).
  mill --no-daemon mill.bsp.BSP/install
)

echo "=== nvim headless e2e: $file @ '$token' ==="
# Wrap the server so its stderr survives nvim (which swallows LSP stderr);
# e2e.lua tails this log on failure.
wrapper="$ws/.ls-server-wrapper.sh"
printf '#!/usr/bin/env bash\nexec "%s" 2>>"%s/ls-server.stderr.log"\n' "$bin" "$ws" > "$wrapper"
chmod +x "$wrapper"
exec nvim --headless -l it/nvim/e2e.lua "$ws" "$wrapper" "$file" "$token"
