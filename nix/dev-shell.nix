{ pkgs, jdk, mill, zaozi-src ? null
, pcHostAgentJar ? null, scalaLibraryJar ? null, scala3LibraryJar ? null
, zaoziPcpluginJar ? null, pythonEnv ? null }:

pkgs.mkShell {
  packages = with pkgs; [
    jdk
    mill
    mill-ivy-fetcher
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
    # jextract generates the Java/Scala FFM bindings from the cbindgen C-ABI
    # header (island boundary); used by the ls-pc-host island + the boundary spike.
    jextract
    # Headless editor client for the project-level e2e
    # (scripts/it-nvim-zaozi.sh + it/nvim/e2e.lua).
    neovim
    # The formatter the server shells out to for textDocument/formatting
    # (resolved via the PATH tier in the dev shell; the packaged wrapper bakes
    # its own store copy as the LS_SCALAFMT default — nix/package.nix).
    scalafmt
  ] ++ pkgs.lib.optional (pythonEnv != null) pythonEnv;

  JAVA_HOME = "${jdk}";
  LS_JAVA_VERSION = "25";
  # Rust standard-library source so rust-analyzer resolves std in the dev shell.
  RUST_SRC_PATH = "${pkgs.rustPlatform.rustLibSrc}";
  # protoc for the SemanticDB prost codegen (ls-semanticdb).
  PROTOC = "${pkgs.protobuf}/bin/protoc";
  # The embedded JVM's libjvm.so, dlopen'd by ls-jvm for the in-process island.
  # On nixpkgs jdk25 it lives under ${jdk.home} (= ${jdk}/lib/openjdk), NOT
  # $JAVA_HOME/lib/server — expose the exact path.
  LS_LIBJVM = "${jdk.home}/lib/server/libjvm.so";

  # The presentation-compiler boot inputs, so the real-BSP PC rows
  # (`scripts/it-real-bsp-rs.sh`) and the `ls-jvm`/`ls-server` live PC checks run
  # for real in the dev shell — the mill-built island host agent jar plus the
  # Scala standard-library classpath the embedded compiler resolves against.
  # Null when the flake is used without these inputs (then the PC rows skip).
  PC_HOST_AGENT_JAR =
    if pcHostAgentJar == null then "" else "${pcHostAgentJar}/pc-host-agent.jar";
  LS_PC_TARGET_CLASSPATH =
    if scalaLibraryJar == null || scala3LibraryJar == null then ""
    else "${scalaLibraryJar}:${scala3LibraryJar}";
  # The zaozi PC-navigation compiler plugin jar, so the live zaozi vtable-boundary
  # test (`cargo test -p ls-jvm --test live_zaozi`) runs in the dev shell instead
  # of skipping. Null when the flake is used without this input.
  ZAOZI_PCPLUGIN_JAR =
    if zaoziPcpluginJar == null then "" else "${zaoziPcpluginJar}/zaozi-pcplugin.jar";
  # Pinned + patched zaozi source (real-repo real-BSP workspace for
  # manual real-repo validation). Null when the flake is used without the zaozi input.
  ZAOZI_SRC = if zaozi-src == null then "" else "${zaozi-src}";
}
