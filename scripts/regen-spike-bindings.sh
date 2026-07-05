#!/usr/bin/env bash
# Regenerate the island boundary bindings for the M0 spike:
#   Rust ABI  --cbindgen-->  boundary.h  --jextract-->  Scala/Java FFM bindings
#
# Run inside `nix develop` (provides cbindgen, jextract, and the JDK). The
# generated Java under modules/ls-pc-host-spike/src/spike/boundary/ is committed
# so the mill build and `nix flake check` do not need jextract at build time;
# rerun this whenever the Rust boundary ABI (crates/ls-jvm-spike) changes.
set -euo pipefail

crate="crates/ls-jvm-spike"
module="modules/ls-pc-host-spike"

echo "==> cbindgen: $crate/boundary.h"
cbindgen --config "$crate/cbindgen.toml" --crate ls-jvm-spike --output "$crate/boundary.h"

# jextract's bundled clang needs the system stdint.h; take it from the nix
# toolchain search path (override with GLIBC_INCLUDE for non-dev-shell runs).
glibc_inc="${GLIBC_INCLUDE:-$(printf '' | cc -xc -E -v - 2>&1 \
  | sed -n '/search starts/,/End of search/p' | grep glibc | tr -d ' ')}"
echo "==> jextract include dir: $glibc_inc"

rm -rf "$module/src/spike/boundary"
mkdir -p "$module/src"
jextract --output "$module/src" -I "$glibc_inc" -t spike.boundary \
  --include-constant ABI_VERSION \
  --include-struct RustVtable \
  --include-struct PcVtable \
  --include-typedef LogFn \
  --include-typedef RegisterPcVtableFn \
  --include-typedef PcDispatchLoopFn \
  --include-typedef EchoFn \
  "$crate/boundary.h"

echo "==> generated:"
find "$module/src/spike/boundary" -type f | sort
