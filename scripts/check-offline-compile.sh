#!/usr/bin/env bash
# Offline-compile guard: prove the whole build compiles from the locked ivy
# cache with no network access, so an unlocked dependency can never slip in.
#
# Normal mode seeds a temp coursier cache from the flake ivyCache derivation,
# forces coursier offline, and runs `mill --no-daemon __.compile`:
#
#   scripts/check-offline-compile.sh
#
# --self-test copies the repo to a scratch dir, appends one deliberately
# unlocked dependency, and requires the offline compile to FAIL — proving the
# guard actually rejects dependencies the locked cache does not carry:
#
#   scripts/check-offline-compile.sh --self-test
#
# The compile step is overridable via $OFFLINE_COMPILE_CMD (used by the focused
# test to substitute a fast stub that fails iff an unlocked dependency is
# present); unset, it runs the real offline mill compile.
set -euo pipefail

cd "$(dirname "$0")/.."
repo="$PWD"

# Runs the offline compile in project dir $1; exit status is the compile result.
run_offline_compile() {
  local proj="$1"
  if [[ -n "${OFFLINE_COMPILE_CMD:-}" ]]; then
    ( cd "$proj" && OFFLINE_PROJECT="$proj" "$OFFLINE_COMPILE_CMD" )
    return $?
  fi
  local cache ivy
  cache="$(mktemp -d)"
  # seed the temp coursier cache from the flake ivyCache (coursier-cache shaped)
  ivy="$(nix build --no-link --print-out-paths "$repo#default.passthru.ivyCache")"
  cp -r "$ivy/." "$cache/" 2>/dev/null || true
  ( cd "$proj" \
    && COURSIER_CACHE="$cache" \
       COURSIER_MODE=offline \
       mill --no-daemon __.compile )
}

self_test() {
  local scratch
  scratch="$(mktemp -d)"
  cp -r "$repo/build.mill" "$repo/modules" "$scratch/"
  # append one deliberately unlocked dependency (smallest build-file mutation)
  cat >> "$scratch/build.mill" <<'EOF'

// offline-guard self-test: a dependency the locked cache cannot resolve.
object offlineGuardUnlockedProbe extends LsModule {
  def dirName = "ls-index-model"
  def mvnDeps = Seq(mvn"com.example:definitely-not-locked_3:9.9.9")
}
EOF
  if run_offline_compile "$scratch"; then
    echo "self-test FAILED: the offline compile succeeded with an unlocked dependency" >&2
    rm -rf "$scratch"
    return 1
  fi
  echo "self-test ok: the offline compile guard rejects an unlocked dependency"
  rm -rf "$scratch"
  return 0
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
else
  run_offline_compile "$repo"
  echo "offline compile ok: the build resolves entirely from the locked cache"
fi
