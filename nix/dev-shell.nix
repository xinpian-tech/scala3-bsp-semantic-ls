{ pkgs, jdk, mill, zaozi-src ? null }:

pkgs.mkShell {
  packages = with pkgs; [
    jdk
    mill
    mill-ivy-fetcher
    sqlite
    sqlite.dev
    pkg-config
    git
    jq

    # Rust toolchain for the v2 core rewrite (crates/). Nixpkgs-native stable
    # toolchain, pinned transitively through the flake lock — no extra inputs.
    rustc
    cargo
    clippy
    rustfmt
    rust-analyzer
    # protoc for the SemanticDB prost codegen; cbindgen for the C-ABI header.
    protobuf
    rust-cbindgen
  ];

  JAVA_HOME = "${jdk}";
  LS_JAVA_VERSION = "25";
  # Rust standard-library source so rust-analyzer resolves std in the dev shell.
  RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
  # The SQLite shared library consumed by the ls-sqlite-ffm FFM binding.
  # System SQLite is never used; only the Nix-provided library is a valid
  # runtime dependency.
  LS_SQLITE_LIB = "${pkgs.sqlite.out}/lib/libsqlite3${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}";

  # Pinned + patched zaozi source (real-repo real-BSP workspace for
  # scripts/it-zaozi.sh). Null when the flake is used without the zaozi input.
  ZAOZI_SRC = if zaozi-src == null then "" else "${zaozi-src}";
}
