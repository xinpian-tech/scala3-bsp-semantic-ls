{
  description = "scala3-bsp-semantic-ls";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";

    # Hard requirement.
    mill-ivy-fetcher.url = "github:Avimitin/mill-ivy-fetcher";
  };

  outputs = { self, nixpkgs, flake-utils, mill-ivy-fetcher }@inputs:
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
      in
      {
        devShells.default = import ./nix/dev-shell.nix {
          inherit pkgs jdk mill;
        };

        formatter = pkgs.nixpkgs-fmt;

        checks = import ./nix/checks.nix {
          inherit pkgs jdk mill self;
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
        };
      }) // { inherit inputs; };
}
