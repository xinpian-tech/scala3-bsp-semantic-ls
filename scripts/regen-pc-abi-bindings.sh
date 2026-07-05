#!/usr/bin/env bash
# Regenerate the presentation-compiler boundary header from the Rust ABI:
#   Rust ABI (crates/ls-pc-abi)  --cbindgen-->  boundary.h
#
# Run inside `nix develop` (provides cbindgen). The generated boundary.h is
# committed so the mill build and `nix flake check` do not need cbindgen at
# build time; rerun this whenever the Rust boundary ABI changes. The header is
# the single contract the JVM-island layout mirror is generated from (the
# jextract step lands with the host module).
set -euo pipefail

crate="crates/ls-pc-abi"

echo "==> cbindgen: $crate/boundary.h"
cbindgen --config "$crate/cbindgen.toml" --crate ls-pc-abi --output "$crate/boundary.h"

echo "==> generated:"
ls -l "$crate/boundary.h"
