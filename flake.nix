{
  description = "scala3-bsp-semantic-ls";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    # Crane builds the Rust cargo workspace (crates/) reproducibly and supplies
    # the fmt/clippy/test/build checks wired into `nix flake check`.
    crane.url = "github:ipetkov/crane";

    # Hard requirement.
    mill-ivy-fetcher.url = "github:Avimitin/mill-ivy-fetcher";

    # A real third-party Scala 3 project used as a real-repo workspace for the
    # manual real-BSP validation (scripts/it-zaozi.sh). Pinned source only: it is
    # built with its OWN flake (native CIRCT/MLIR toolchain). Local modifications
    # are maintained as patches under nix/patches/ (see zaozi-semanticdb.patch).
    zaozi = {
      url = "github:xinpian-tech/zaozi";
      flake = false;
    };
  };

  outputs = { self, nixpkgs, flake-utils, crane, mill-ivy-fetcher, zaozi }@inputs:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs {
          inherit system;
          overlays = [
            mill-ivy-fetcher.overlays.default
            mill-ivy-fetcher.overlays.mill-overlay
          ];
        };

        jdk = pkgs.jdk25;
        mill = pkgs.millVersions.mill_1_1_2 or pkgs.mill;

        # Crane library + the Rust cargo workspace (crates/) build and checks.
        craneLib = crane.mkLib pkgs;
        rust = import ./nix/rust.nix { inherit pkgs craneLib; };

        # Embedded-JVM boundary spike: the mill-built island agent jar plus an
        # end-to-end check that boots the JVM through the crane-built spike binary
        # and drives every boundary scenario (echo / containment / timeout /
        # layout-canary refusal).
        spikeAgentJar = pkgs.callPackage ./nix/spike-agent.nix { inherit mill jdk; };
        spike-boundary-check = pkgs.runCommand "check-spike-boundary"
          { nativeBuildInputs = [ jdk ]; } ''
          export LS_LIBJVM="${jdk.home}/lib/server/libjvm.so"
          export SPIKE_AGENT_JAR="${spikeAgentJar}/spike-agent.jar"
          bin="${rust.package}/bin/ls-jvm-spike"
          for s in echo java-throw rust-panic timeout bad-canary; do
            echo "=== boundary scenario: $s ==="
            result=$("$bin" "$s")
            echo "$result"
            echo "$result" | grep -q "SPIKE_OK" || { echo "scenario $s did not report SPIKE_OK"; exit 1; }
          done
          touch $out
        '';

        # The pinned zaozi source with our patches applied (SemanticDB emission,
        # which our SemanticDB-first server requires). Exposed to the dev shell
        # as ZAOZI_SRC so scripts/it-zaozi.sh builds a pinned, reproducible tree
        # instead of an ad-hoc `git clone`.
        zaozi-src = pkgs.applyPatches {
          name = "zaozi-patched-src";
          src = zaozi;
          patches = [ ./nix/patches/zaozi-semanticdb.patch ];
        };
      in
      {
        devShells.default = import ./nix/dev-shell.nix {
          inherit pkgs jdk mill zaozi-src;
        };

        formatter = pkgs.nixpkgs-fmt;

        checks = (import ./nix/checks.nix {
          inherit pkgs jdk mill self;
        }) // rust.checks // {
          spike-boundary = spike-boundary-check;
        };

        packages = {
          default = pkgs.callPackage ./nix/package.nix {
            inherit mill jdk;
            inherit (pkgs) sqlite;
          };
          # Exposed so `nix shell '.#mill' '.#mill-ivy-fetcher' -c mif run ...`
          # (plan section 15.3) works as documented.
          inherit mill;
          inherit (pkgs) mill-ivy-fetcher;
          # The patched, pinned zaozi source (real-repo real-BSP workspace).
          inherit zaozi-src;
          # The crane-built Rust cargo workspace (crates/).
          rust-workspace = rust.package;
          # The embedded-JVM boundary spike island agent jar (-javaagent premain).
          spike-agent-jar = spikeAgentJar;
        };
      }) // { inherit inputs; };
}
