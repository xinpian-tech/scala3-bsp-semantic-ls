{ pkgs, jdk, mill, self }:

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

  # Lock hygiene: nix/ivy-lock.nix must exist and parse as a Nix expression.
  ivy-lock-present = pkgs.runCommand "check-ivy-lock-present" { } ''
    test -f ${self}/nix/ivy-lock.nix
    ${pkgs.nix}/bin/nix-instantiate --parse ${self}/nix/ivy-lock.nix > /dev/null
    touch $out
  '';

  # The flake must keep the mill-ivy-fetcher input pinned.
  mill-ivy-fetcher-input = pkgs.runCommand "check-mill-ivy-fetcher-input" { } ''
    grep -q 'mill-ivy-fetcher.url = "github:Avimitin/mill-ivy-fetcher"' ${self}/flake.nix
    touch $out
  '';
}
