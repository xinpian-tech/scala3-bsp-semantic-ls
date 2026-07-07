#!/usr/bin/env bash
# Real-BSP end-to-end suite (Rust): drive the whole ls-server over the framed LSP
# wire against a REAL Mill BSP server built from the deterministic
# `it/sample-workspace` (production discovery -> mill launch -> model load ->
# compile -> diagnostics -> rename-through-compile -> teardown).
#
# Mirrors scripts/it-real-bsp.sh (which runs the Scala RealBsp* suites). The suite
# is gated on LS_REAL_BSP_IT=1 and skips in ordinary test runs, because mill needs
# a JVM/toolchain the hermetic Nix check sandbox forbids.
#
# The presentation-compiler scenarios additionally need a real embedded JVM and
# skip unless LS_LIBJVM + PC_HOST_AGENT_JAR + LS_PC_TARGET_CLASSPATH are set (see
# scripts/it-real-bsp.sh / the ls-jvm live checks for how those are provisioned).
#
# Usage:  nix develop -c ./scripts/it-real-bsp-rs.sh
set -euo pipefail
cd "$(dirname "$0")/.."

export LS_REAL_BSP_IT=1
export LS_REPO_ROOT="$PWD"

exec cargo test -p ls-server --test real_bsp_e2e -- --nocapture
