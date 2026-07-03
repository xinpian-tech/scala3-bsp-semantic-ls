#!/usr/bin/env bash
# Real-BSP end-to-end suite: drive the LSP server against a REAL Mill BSP server
# built from the deterministic `it/sample-workspace`. Runs every real-BSP suite in
# ONE JVM invocation so they share the single lazily-booted mill-bsp fixture
# (RealBspFixture): RealBspIntegrationTest (happy path), RealBspCoreTest
# (E1/E4/E5), and RealBspLifecycleTest (E2/E3/E6/E8).
#
# For a full real-repo validation against the unmodified zaozi built with its own
# Nix toolchain, see scripts/it-zaozi.sh (heavy, manual).
#
# Usage:  nix develop -c ./scripts/it-real-bsp.sh
#
# The suites are gated on LS_REAL_BSP_IT=1 and skipped in ordinary test runs.
set -euo pipefail
cd "$(dirname "$0")/.."

export LS_REAL_BSP_IT=1
export LS_REPO_ROOT="$PWD"

exec mill --no-daemon core.test.testOnly \
  ls.core.RealBspIntegrationTest \
  ls.core.RealBspCoreTest \
  ls.core.RealBspLifecycleTest
