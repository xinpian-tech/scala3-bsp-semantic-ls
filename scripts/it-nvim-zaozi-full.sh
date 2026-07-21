#!/usr/bin/env bash
# FULL-workspace zaozi editor e2e: the real-repo macro-navigation verification
# (docs/deployment.md §5.7). Headless Neovim attaches the PACKAGED server
# (`nix build .#default` — the wrapper's baked defaults, deploy realism) to an
# UNTRIMMED copy of the pinned zaozi checkout and drives it/nvim/e2e.lua in
# FULL mode (LS_NVIM_FULL=1): the full-model health gates (ready / compile /
# reindex / doctor over every CIRCT module) plus the dynamic-bundle-field
# probes — go-to-definition and hover on a real `io.<field>` access resolve
# the `val <field> = Aligned/Flipped(...)` declaration through the shipped
# zaozi PC plugin (wired via the workspace pc-plugins.json), and the
# references / workspace-symbol counts are recorded as E2E INFO lines.
#
# The zaozi build needs zaozi's OWN native toolchain (CIRCT/MLIR via Panama),
# so every mill / server step runs inside ZAOZI'S dev shell
# (`nix develop $ZAOZI_SRC`, first entry fetches CIRCT from the org ci-cache);
# nvim and the packaged server binary are resolved to absolute store paths
# from THIS repo's dev shell before entering it.
#
# Usage:  nix develop -c ./scripts/it-nvim-zaozi-full.sh
#   LS_NVIM_PROJECT_DIR=/path/to/full-copy   reuse a prepared FULL workspace
#                                            (skips the copy; never deleted)
#   LS_NVIM_FILE / LS_NVIM_TOKEN             override the main-buffer anchor
#   LS_NVIM_COMPILE_TIMEOUT_MS               session-compile budget (default 60min)
#   LS_NVIM_READY_TIMEOUT_S                  bootstrap-ready budget (default 30min)
#   LS_NVIM_REINDEX_TIMEOUT_MS               reindex budget (default 10min)
#   LS_NVIM_FULL_PRECOMPILE=0                skip the mill __.compile pre-warm
#   LS_NVIM_FULL_KEEP=1                      keep the tmp workspace on success
set -euo pipefail
cd "$(dirname "$0")/.."
repo="$PWD"

: "${ZAOZI_SRC:?ZAOZI_SRC is unset — run inside 'nix develop' (the flake exports the pinned zaozi source)}"
nvim_bin="$(command -v nvim)" || { echo "nvim not on PATH — re-enter 'nix develop'" >&2; exit 1; }

phase_start=$(date +%s)
phase() { # <label>
  local now
  now=$(date +%s)
  echo "=== timing: $1 (+$((now - phase_start))s) ==="
  phase_start=$now
}

# The packaged server: baked JAVA_HOME / PC_HOST_AGENT_JAR / LS_SCALAFMT
# defaults, and the shipped zaozi PC plugin jar under share/.
pkg_link="$repo/tmp/it-nvim-zaozi-full-package"
mkdir -p "$repo/tmp"
nix build "$repo#default" -o "$pkg_link"
server_bin="$pkg_link/bin/scala3-bsp-semantic-ls"
plugin_jar="$(readlink -f "$pkg_link")/share/scala3-bsp-semantic-ls/zaozi-pcplugin.jar"
[ -x "$server_bin" ] || { echo "packaged server missing at $server_bin" >&2; exit 1; }
[ -f "$plugin_jar" ] || { echo "packaged zaozi plugin jar missing at $plugin_jar" >&2; exit 1; }
phase "nix build .#default (packaged server)"

ws="${LS_NVIM_PROJECT_DIR:-}"
tmp=""
if [ -z "$ws" ]; then
  tmp=$(mktemp -d)
  cp -r --no-preserve=mode,ownership "$ZAOZI_SRC" "$tmp/zaozi"
  ws="$tmp/zaozi"
  phase "workspace copy (untrimmed)"
fi
cleanup() {
  status=$?
  if [ -n "$tmp" ]; then
    if [ "$status" -eq 0 ] && [ -z "${LS_NVIM_FULL_KEEP:-}" ]; then
      # The mill BSP subprocess the server spawned can outlive nvim by a
      # moment and repopulate .bsp/out mid-sweep — retry the removal briefly
      # and never let a teardown race fail a green run.
      for _ in 1 2 3 4 5; do
        rm -rf "$tmp" 2>/dev/null && break
        sleep 2
      done
      rm -rf "$tmp" 2>/dev/null || echo "=== cleanup: could not fully remove $tmp ===" >&2
    else
      echo "=== full workspace kept at $ws (exit $status) ===" >&2
    fi
  fi
  exit "$status"
}
trap cleanup EXIT

# Wire the shipped PC plugin into the workspace: the island reads
# .scala3-bsp-semantic-ls/pc-plugins.json at boot (docs/deployment.md §4.8).
mkdir -p "$ws/.scala3-bsp-semantic-ls"
cat > "$ws/.scala3-bsp-semantic-ls/pc-plugins.json" <<EOF
{"compilerPlugins":[{"jars":["$plugin_jar"],"options":[]}],"servicePluginJars":[]}
EOF

file="${LS_NVIM_FILE:-zaozi/tests/src/UIntSpec.scala}"
token="${LS_NVIM_TOKEN:-UIntSpecIO}"

echo "=== preparing FULL real workspace: $ws ==="
# Everything zaozi-flavored runs inside ZAOZI'S dev shell: mill, the JDK the
# build expects, and the CIRCT/MLIR install paths its Panama modules need.
# The shell is evaluated at the pristine store source ($ZAOZI_SRC carries
# flake.nix + flake.lock), so the mutable copy is never re-copied to the
# store. The shell MUST be entered with cwd = the workspace copy: zaozi's
# shellHook prepares the locked coursier/ivy cache under $PWD/out/.coursier
# (redirecting user.home/ivy.home there via JAVA_TOOL_OPTIONS) and writes the
# .bsp connection file relative to $PWD.
zaozi_dev() { # <cmd...>  (cwd = the workspace copy)
  (cd "$ws" && nix develop "path:$ZAOZI_SRC" -c bash -c "$*")
}

# jextract parses the 95K-line MLIR/CIRCT CAPI headers on its JVM main
# thread; zaozi's build hands the jextract subprocess a bare
# JAVA_TOOL_OPTIONS (dropping the dev shell's -Xss32m), so the default 1M
# main stack overflows nondeterministically (SIGSEGV in libc, observed on
# mlirlib.generatedSources). JDK_JAVA_OPTIONS reaches every `java` LAUNCHER
# in the tree (mill, scalac forks, jextract) and never the server's
# JNI-created island JVM — widen the stack there, harness-side only.
export JDK_JAVA_OPTIONS="${JDK_JAVA_OPTIONS:+$JDK_JAVA_OPTIONS }-Xss32m"

zaozi_dev mill --no-daemon mill.bsp.BSP/install
phase "mill.bsp.BSP/install (zaozi dev shell)"

if [ "${LS_NVIM_FULL_PRECOMPILE:-1}" != "0" ]; then
  # Pre-warm the full compile so the retained BSP session's own compile is an
  # incremental confirmation instead of a cold many-minute build (the doctor's
  # troubleshooting advice for large first compiles). Retried over the warm
  # caches: mill resumes at the failed task, so a flaky native-codegen crash
  # does not restart the build.
  attempts="${LS_NVIM_FULL_PRECOMPILE_ATTEMPTS:-3}"
  i=1
  while true; do
    if zaozi_dev mill --no-daemon __.compile; then
      break
    fi
    if [ "$i" -ge "$attempts" ]; then
      echo "mill __.compile failed after $attempts attempts" >&2
      exit 1
    fi
    echo "=== mill __.compile attempt $i failed; retrying over the warm caches ===" >&2
    i=$((i + 1))
  done
  phase "mill __.compile pre-warm (full workspace)"
fi

echo "=== nvim headless FULL e2e: $file @ '$token' ==="
# Wrap the server so its stderr survives nvim (which swallows LSP stderr);
# e2e.lua tails this log on failure.
wrapper="$ws/.ls-server-wrapper.sh"
printf '#!/usr/bin/env bash\nexec "%s" 2>>"%s/ls-server.stderr.log"\n' "$server_bin" "$ws" > "$wrapper"
chmod +x "$wrapper"

export LS_NVIM_FULL=1
export LS_NVIM_COMPILE_TIMEOUT_MS="${LS_NVIM_COMPILE_TIMEOUT_MS:-3600000}"
export LS_NVIM_READY_TIMEOUT_S="${LS_NVIM_READY_TIMEOUT_S:-1800}"
export LS_NVIM_REINDEX_TIMEOUT_MS="${LS_NVIM_REINDEX_TIMEOUT_MS:-600000}"
# nvim comes from OUR dev shell (absolute store path); the process tree —
# server wrapper, mill BSP subprocess, the embedded island JVM — inherits
# zaozi's shell env (JAVA_HOME, CIRCT_INSTALL_PATH, JAVA_TOOL_OPTIONS
# -Xss32m + the workspace-out user.home/coursier redirection, hence
# cwd = $ws here too).
(
  cd "$ws" \
    && nix develop "path:$ZAOZI_SRC" \
      -c "$nvim_bin" --headless -l "$repo/it/nvim/e2e.lua" "$ws" "$wrapper" "$file" "$token"
)
phase "nvim FULL e2e"
