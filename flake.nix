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

        # The production island host agent jar plus a check that it builds
        # offline and is a valid -javaagent (its manifest declares the premain).
        pcHostAgentJar = pkgs.callPackage ./nix/pc-host-agent.nix { inherit mill jdk; };

        # The Scala standard-library jars the live-boundary check hands a
        # registered target as its classpath, so the embedded compiler can
        # resolve `List`/`String` (etc.) for real queries. Both are what the
        # retained PC test harness uses; pinned by hash, versions matched to the
        # compiler bundled in the PC-host assembly (build.mill `Deps.scalaVer`).
        scalaLibraryJar = pkgs.fetchurl {
          url = "https://repo1.maven.org/maven2/org/scala-lang/scala-library/3.8.4/scala-library-3.8.4.jar";
          hash = "sha256-G4Mw3ld0wVh0Fz8Wi2dCVcqgDF7zZIqwtst2O7f9Lec=";
        };
        scala3LibraryJar = pkgs.fetchurl {
          url = "https://repo1.maven.org/maven2/org/scala-lang/scala3-library_3/3.8.4/scala3-library_3-3.8.4.jar";
          hash = "sha256-j4LhyJdKho877rR1+pwyI50uCC6fgD6/w+65EhG4h9E=";
        };

        # End-to-end check that boots the PRODUCTION island (crane-built
        # `ls-jvm`) with the real PC-host assembly against a real JVM and drives
        # register/open/completion + hover through the 15-slot vtable to a live
        # compiler. Reuses the shared crane artifacts; the boot inputs are handed
        # in as env so the test runs for real here (and skips in `rust-test`,
        # which sets none of them).
        pc-boundary-check = craneLib.cargoTest (rust.commonArgs // {
          inherit (rust) cargoArtifacts;
          cargoTestExtraArgs = "-p ls-jvm --test live_boundary";
          nativeBuildInputs = [ jdk ];
          LS_LIBJVM = "${jdk.home}/lib/server/libjvm.so";
          PC_HOST_AGENT_JAR = "${pcHostAgentJar}/pc-host-agent.jar";
          LS_PC_TARGET_CLASSPATH = "${scalaLibraryJar}:${scala3LibraryJar}";
        });

        # The live dispatch-generation recovery check: same real JVM + assembly +
        # classpath, but the test arms the Java fault hook (via IslandConfig) so a
        # real completion wedges and the watchdog must recover through the real
        # spawn_dispatch slot, then hit the generation cap → fatal.
        pc-recovery-check = craneLib.cargoTest (rust.commonArgs // {
          inherit (rust) cargoArtifacts;
          cargoTestExtraArgs = "-p ls-jvm --test live_recovery";
          nativeBuildInputs = [ jdk ];
          LS_LIBJVM = "${jdk.home}/lib/server/libjvm.so";
          PC_HOST_AGENT_JAR = "${pcHostAgentJar}/pc-host-agent.jar";
          LS_PC_TARGET_CLASSPATH = "${scalaLibraryJar}:${scala3LibraryJar}";
        });
        pc-host-agent-check = pkgs.runCommand "check-pc-host-agent"
          { nativeBuildInputs = [ jdk ]; } ''
          jar="${pcHostAgentJar}/pc-host-agent.jar"
          echo "=== pc-host agent manifest ==="
          jar xf "$jar" META-INF/MANIFEST.MF
          cat META-INF/MANIFEST.MF
          grep -q "Premain-Class: ls.pc.host.PcHostAgent" META-INF/MANIFEST.MF \
            || { echo "pc-host agent jar is missing its Premain-Class"; exit 1; }
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
          pc-host-agent = pc-host-agent-check;
          pc-boundary = pc-boundary-check;
          pc-recovery = pc-recovery-check;
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
          # The production presentation-compiler island host agent jar.
          pc-host-agent-jar = pcHostAgentJar;
        };
      }) // { inherit inputs; };
}
