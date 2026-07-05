# Crane-backed Rust cargo workspace (crates/).
#
# Provides the shared dependency artifacts plus the fmt/clippy/test/build checks
# wired into `nix flake check`, and a buildable workspace derivation exposed as
# `nix build .#rust-workspace`. Offline/reproducible: dependencies are vendored
# from the committed Cargo.lock.
{ pkgs, craneLib }:

let
  lib = pkgs.lib;

  # Exactly the cargo workspace inputs — Rust sources, manifests, lock, and the
  # rustfmt config — so a Scala/mill change never invalidates the Rust checks.
  src = lib.fileset.toSource {
    root = ../.;
    fileset = lib.fileset.unions [
      ../Cargo.toml
      ../Cargo.lock
      ../rustfmt.toml
      ../crates
    ];
  };

  commonArgs = {
    inherit src;
    strictDeps = true;
    # The current crates are pure Rust; protoc/cbindgen become buildInputs when
    # ls-semanticdb / ls-pc-abi land.
  };

  # Build (and cache) the dependency closure once, shared by every check.
  cargoArtifacts = craneLib.buildDepsOnly commonArgs;

  workspace = craneLib.buildPackage (commonArgs // {
    inherit cargoArtifacts;
    doCheck = false;
  });
in
{
  # `nix build .#rust-workspace`
  package = workspace;

  # Exposed so the flake can build an extra cargo-test check (the live
  # embedded-JVM boundary test) that reuses the shared dependency artifacts and
  # source fileset but adds boot env + a scoped test filter.
  inherit commonArgs cargoArtifacts;

  # Merged into `nix flake check`.
  checks = {
    rust-build = workspace;
    rust-test = craneLib.cargoTest (commonArgs // {
      inherit cargoArtifacts;
      cargoTestExtraArgs = "--workspace";
    });
    rust-clippy = craneLib.cargoClippy (commonArgs // {
      inherit cargoArtifacts;
      cargoClippyExtraArgs = "--all-targets --workspace -- -D warnings";
    });
    rust-fmt = craneLib.cargoFmt { inherit src; };
  };
}
