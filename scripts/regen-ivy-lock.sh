#!/usr/bin/env bash
# Regenerate nix/ivy-lock.nix from build.mill. Run inside `nix develop`.
# Must be re-run (and the result committed) after every dependency change.
#
# Determinism notes:
# - Mill's launcher resolves its runner artifacts (mill-runner-daemon etc.)
#   through the default coursier cache derived from java's user.home, not
#   from $HOME/$COURSIER_CACHE. A warm host cache would satisfy it silently
#   and those artifacts would then be missing from the lock, breaking the
#   offline Nix build. We force user.home to a cold directory and afterwards
#   merge anything the launcher downloaded into the cache mif hashes.
# - The mill daemon does not start reliably inside the project copy mif
#   works on, so a PATH shim forces --no-daemon.
# - mif copies the project directory as-is; a stale out/mill-launcher
#   resolved-classpath file would let the launcher skip resolution entirely,
#   again dropping its artifacts from the lock. We therefore hand mif a
#   clean copy of the sources without out/.
set -euo pipefail

cd "$(dirname "$0")/.."
out_lock="$PWD/nix/ivy-lock.nix"

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

mkdir -p "$tmp/bin" "$tmp/home" "$tmp/cache/cache" "$tmp/src"
cp -r build.mill modules "$tmp/src/"

real_mill="$(command -v mill)"
printf '#!/usr/bin/env bash\nexec %q --no-daemon "$@"\n' "$real_mill" > "$tmp/bin/mill"
chmod +x "$tmp/bin/mill"

export PATH="$tmp/bin:$PATH"
export HOME="$tmp/home"
export XDG_CACHE_HOME="$tmp/home/.cache"
export COURSIER_CACHE="$tmp/cache/cache"
export JAVA_TOOL_OPTIONS="-Duser.home=$tmp/home ${JAVA_TOOL_OPTIONS:-}"

mif fetch -p "$tmp/src" -c "$tmp/cache"

# Merge launcher-side downloads (default cache layout <home>/.cache/coursier/v1)
# into the cache directory mif hashes.
if [[ -d "$tmp/home/.cache/coursier/v1" ]]; then
  cp -rn "$tmp/home/.cache/coursier/v1/." "$tmp/cache/cache/" || true
fi

mif codegen --cache "$tmp/cache" -o "$out_lock"
echo "wrote nix/ivy-lock.nix"
