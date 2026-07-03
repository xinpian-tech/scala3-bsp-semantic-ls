#!/usr/bin/env bash
# AOT-training integration test: builds an AOT cache with scripts/aot-train.sh
# over a real Mill BSP workspace and asserts a cached boot loads it and the
# doctor reports it. Gated on LS_AOT_IT=1 and skipped in ordinary test runs.
#
# Usage:  nix develop -c ./scripts/it-aot.sh
#
# The assembly jar is built HERE, before the test JVM starts, and its path is
# passed through LS_AOT_ASSEMBLY_JAR. The test never invokes `mill` itself: a
# nested `mill` inside the outer `mill core.test` run would deadlock on the
# build lock.
set -euo pipefail
cd "$(dirname "$0")/.."

mill --no-daemon core.assembly

export LS_AOT_IT=1
export LS_REPO_ROOT="$PWD"
export LS_AOT_ASSEMBLY_JAR="$PWD/out/core/assembly.dest/out.jar"

exec mill --no-daemon core.test.testOnly ls.core.AotTrainIntegrationTest
