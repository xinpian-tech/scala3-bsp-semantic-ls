{ pkgs, jdk, mill, self }:

let
  # Importing the lock at eval time both proves it parses as Nix and lets us
  # assert it is non-trivial.
  ivyLock = import (self + "/nix/ivy-lock.nix") { inherit (pkgs) fetchurl; };
  ivyLockCount = builtins.length (builtins.attrNames ivyLock);
in
{
  # Toolchain contract: the dev toolchain must be Java 25 and Mill must be
  # provided by the flake environment.
  java25-toolchain = pkgs.runCommand "check-java25-toolchain"
    {
      nativeBuildInputs = [ jdk mill ];
    } ''
    java -version 2>&1 | tee java-version.txt
    grep -q 'version "25' java-version.txt
    touch $out
  '';

  # Lock hygiene: nix/ivy-lock.nix must exist, parse, and lock a non-trivial
  # dependency set. Freshness vs build.mill is CI's scripts/check-ivy-lock.sh.
  ivy-lock-present =
    assert ivyLockCount > 0;
    pkgs.runCommand "check-ivy-lock-present" { } ''
      echo "ivy-lock.nix locks ${toString ivyLockCount} artifacts"
      touch $out
    '';

  # The flake must keep the mill-ivy-fetcher input pinned.
  mill-ivy-fetcher-input = pkgs.runCommand "check-mill-ivy-fetcher-input" { } ''
    grep -q 'mill-ivy-fetcher.url = "github:Avimitin/mill-ivy-fetcher"' ${self}/flake.nix
    touch $out
  '';

  # The full offline package build (assembly from the locked ivy cache).
  package = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
}
