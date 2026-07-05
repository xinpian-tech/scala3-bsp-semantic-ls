#!/usr/bin/env bash
# Regenerate the island boundary bindings for the presentation-compiler host:
#   Rust ABI (crates/ls-pc-abi)  --cbindgen-->  boundary.h  --jextract-->  FFM bindings
#
# Run inside `nix develop` (provides cbindgen, jextract, and the JDK). The
# generated Java under modules/ls-pc-host/src/ls/pc/host/boundary/ is committed
# so the mill build and `nix flake check` do not need jextract at build time;
# rerun this whenever the Rust boundary ABI (crates/ls-pc-abi) changes. This is
# the production counterpart to scripts/regen-spike-bindings.sh.
set -euo pipefail

crate="crates/ls-pc-abi"
module="modules/ls-pc-host"

echo "==> cbindgen: $crate/boundary.h"
cbindgen --config "$crate/cbindgen.toml" --crate ls-pc-abi --output "$crate/boundary.h"

# jextract's bundled clang needs the system stdint.h; take it from the nix
# toolchain search path (override with GLIBC_INCLUDE for non-dev-shell runs).
glibc_inc="${GLIBC_INCLUDE:-$(printf '' | cc -xc -E -v - 2>&1 \
  | sed -n '/search starts/,/End of search/p' | grep glibc | tr -d ' ')}"
echo "==> jextract include dir: $glibc_inc"

rm -rf "$module/src/ls/pc/host/boundary"
mkdir -p "$module/src"
jextract --output "$module/src" -I "$glibc_inc" -t ls.pc.host.boundary \
  --include-constant ABI_VERSION \
  --include-constant STATUS_OK \
  --include-constant STATUS_PANIC \
  --include-constant STATUS_BAD_ARG \
  --include-constant STATUS_ABI_MISMATCH \
  --include-constant STATUS_DECODE \
  --include-constant STATUS_ALLOC \
  --include-constant STATUS_INTERNAL \
  --include-constant MAGIC \
  --include-struct LsStr \
  --include-struct LsBytes \
  --include-struct LsBuf \
  --include-struct BlobStr \
  --include-struct Position \
  --include-struct AbiRange \
  --include-struct LocationRecord \
  --include-struct PcVtable \
  --include-struct RustVtable \
  --include-typedef AllocFn \
  --include-typedef FreeFn \
  --include-typedef LogFn \
  --include-typedef PcRequestFn \
  --include-typedef PcUriFn \
  --include-typedef PcQueryFn \
  --include-typedef PcResolveFn \
  --include-typedef PcStatusOutFn \
  --include-typedef PcVoidFn \
  --include-typedef PcSpawnDispatchFn \
  --include-typedef RegisterPcVtableFn \
  --include-typedef PcDispatchLoopFn \
  --include-typedef SymbolDefinitionFn \
  "$crate/boundary.h"

echo "==> generated:"
find "$module/src/ls/pc/host/boundary" -type f | sort
