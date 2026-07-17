#!/usr/bin/env bash
# Live mill BSP smoke: drive the ls-bsp client against a REAL mill BSP server
# built from the deterministic `it/sample-workspace` (discovery -> launch ->
# initialize -> project model -> buildTargets/sources/scalacOptions/compile ->
# forced diagnostic).
#
# Like scripts/it-real-bsp-rs.sh, the smoke is gated (LS_BSP_MILL_SMOKE=1) and
# skipped in ordinary test runs, because mill needs a JVM/toolchain the hermetic
# Nix check sandbox forbids.
#
# Usage:  nix develop -c ./scripts/it-mill-smoke.sh
set -euo pipefail
cd "$(dirname "$0")/.."

export LS_BSP_MILL_SMOKE=1
export LS_REPO_ROOT="$PWD"

exec cargo test -p ls-bsp --test mill_smoke -- --nocapture
