#!/usr/bin/env bash
# The black-box LSP suite: pytest-lsp spawns the REAL ls-server binary over
# stdio against the scriptable fake BSP server (it/lsp-blackbox/fake_bsp.py),
# which advertises the committed ls-engine SemanticDB fixture corpus — so the
# whole run is hermetic: no mill, no JVM, no network.
#
# Usage:  nix develop -c ./scripts/it-lsp-blackbox.sh [pytest args...]
set -euo pipefail
cd "$(dirname "$0")/.."

cargo build -p ls-server
export LS_SERVER_BIN="$PWD/target/debug/ls-server"
exec pytest it/lsp-blackbox "$@"
