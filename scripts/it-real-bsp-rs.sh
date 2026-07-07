#!/usr/bin/env bash
# Real-BSP end-to-end suite (Rust): drive the whole ls-server over the framed LSP
# wire against a REAL Mill BSP server built from the deterministic
# `it/sample-workspace` (production discovery -> mill launch -> model load ->
# compile -> diagnostics -> rename-through-compile -> teardown), plus the real
# embedded presentation-compiler rows and the dispatch-generation recovery.
#
# Mirrors scripts/it-real-bsp.sh (the Scala RealBsp* suites). Gated on
# LS_REAL_BSP_IT=1 and skipped in ordinary test runs, because mill needs a
# JVM/toolchain the hermetic Nix check sandbox forbids.
#
# The presentation-compiler rows need the embedded JVM boot inputs
# (LS_LIBJVM + PC_HOST_AGENT_JAR + LS_PC_TARGET_CLASSPATH). The dev shell exports them (see
# nix/dev-shell.nix); run this under `nix develop` so they are present. If they
# are missing this script FAILS LOUDLY rather than silently skipping the PC rows,
# unless LS_REAL_BSP_SKIP_PC=1 is set for an index-only local smoke run.
#
# Usage:  nix develop -c ./scripts/it-real-bsp-rs.sh
set -euo pipefail
cd "$(dirname "$0")/.."

export LS_REAL_BSP_IT=1
export LS_REPO_ROOT="$PWD"

# The index/BSP rows (own binary; no JVM).
cargo test -p ls-server --test real_bsp_e2e -- --test-threads=1 --nocapture

if [[ "${LS_REAL_BSP_SKIP_PC:-}" == "1" ]]; then
  echo "it-real-bsp-rs: LS_REAL_BSP_SKIP_PC=1 — skipping the presentation-compiler rows."
  exit 0
fi

# All THREE vars gate the PC rows (see pc_enabled() in real_bsp_common): a
# missing one makes the PC tests skip-pass, so check every one or the runner
# would report success without actually running the presentation-compiler rows.
if [[ -z "${LS_LIBJVM:-}" || -z "${PC_HOST_AGENT_JAR:-}" || -z "${LS_PC_TARGET_CLASSPATH:-}" ]]; then
  echo "it-real-bsp-rs: ERROR — the presentation-compiler rows need LS_LIBJVM," >&2
  echo "  PC_HOST_AGENT_JAR, and LS_PC_TARGET_CLASSPATH. Run under 'nix develop'" >&2
  echo "  (nix/dev-shell.nix exports them), or set LS_REAL_BSP_SKIP_PC=1 for an" >&2
  echo "  index-only smoke run." >&2
  exit 1
fi

# The presentation-compiler rows: each is its own binary (one embedded JVM/island
# per process). The recovery row additionally arms the test fault hook.
cargo test -p ls-server --test real_bsp_pc -- --nocapture
LS_PC_TEST_FAULT=busyCompletion cargo test -p ls-server --test real_bsp_pc_recovery -- --nocapture
