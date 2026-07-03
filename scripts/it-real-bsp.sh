#!/usr/bin/env bash
# Real-BSP integration tests: drive the LSP server against a REAL Mill BSP
# server built from `it/sample-workspace` (deterministic acceptance) and from
# `it/zaozi` (a vendored real third-party Scala 3 codebase — see it/zaozi/NOTICE.md).
#
# Usage:  nix develop -c ./scripts/it-real-bsp.sh
#
# The tests are gated on LS_REAL_BSP_IT=1 and skipped in ordinary test runs.
set -euo pipefail
cd "$(dirname "$0")/.."

export LS_REAL_BSP_IT=1
export LS_REPO_ROOT="$PWD"

exec mill --no-daemon core.test.testOnly ls.core.RealBspIntegrationTest ls.core.RealBspZaoziTest
