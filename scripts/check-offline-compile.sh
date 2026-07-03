#!/usr/bin/env bash
# Offline-compile guard: prove the whole build compiles from the locked ivy
# cache with no network access, so an unlocked dependency can never slip in.
#
# Normal mode seeds a temp coursier cache from the flake ivyCache derivation,
# forces coursier offline under a COLD cache boundary, and runs
# `mill --no-daemon __.compile`:
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

# One scratch root, cleaned on any exit (success or failure).
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
seq=0

# Runs the offline compile in project dir $1 under a COLD cache boundary; exit
# status is the compile result. Isolating HOME/XDG_CACHE_HOME/user.home (not
# just COURSIER_CACHE) matters because Mill's launcher resolves through the
# coursier cache derived from java's user.home — a warm host cache there would
# otherwise satisfy resolution silently and let the guard pass for the wrong
# reason (see scripts/regen-ivy-lock.sh and docs/nix-build.md).
run_offline_compile() {
  local proj="$1"
  seq=$((seq + 1))
  local root="$work/run-$seq"
  local home="$root/home"
  local cache="$root/cache"
  mkdir -p "$home/.cache" "$cache"

  # Seed the temp coursier cache from the flake ivyCache (coursier-cache shaped).
  # Skipped only when a stub compile is provided for the focused guard test.
  if [[ -z "${OFFLINE_COMPILE_CMD:-}" ]]; then
    local ivy
    if ! ivy="$(nix build --no-link --print-out-paths "$repo#default.passthru.ivyCache")" \
      || ! cp -r "$ivy/." "$cache/"; then
      echo "error: failed to seed the offline coursier cache from the flake ivyCache" >&2
      return 1
    fi
  fi

  local rc=0
  (
    cd "$proj"
    export HOME="$home"
    export XDG_CACHE_HOME="$home/.cache"
    export COURSIER_CACHE="$cache"
    export COURSIER_MODE=offline
    export JAVA_TOOL_OPTIONS="-Duser.home=$home ${JAVA_TOOL_OPTIONS:-}"
    if [[ -n "${OFFLINE_COMPILE_CMD:-}" ]]; then
      OFFLINE_PROJECT="$proj" "$OFFLINE_COMPILE_CMD"
    else
      mill --no-daemon __.compile
    fi
  ) || rc=$?
  return $rc
}

self_test() {
  local scratch="$work/scratch"
  mkdir -p "$scratch"
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
    return 1
  fi
  echo "self-test ok: the offline compile guard rejects an unlocked dependency"
  return 0
}

if [[ "${1:-}" == "--self-test" ]]; then
  self_test
else
  run_offline_compile "$repo"
  echo "offline compile ok: the build resolves entirely from the locked cache"
fi
