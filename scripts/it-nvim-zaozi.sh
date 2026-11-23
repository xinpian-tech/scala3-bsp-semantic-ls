#!/usr/bin/env bash
# Drive the decoder editor checks against the pinned zaozi workspace.
set -euo pipefail
export LS_NVIM_FILE="${LS_NVIM_FILE:-decoder/src/TruthTable.scala}"
export LS_NVIM_TOKEN="${LS_NVIM_TOKEN:-BitSet}"
export LS_NVIM_FULL=0
exec "$(dirname "$0")/it-nvim-zaozi-full.sh"
