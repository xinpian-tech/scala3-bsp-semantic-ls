{ pkgs, jdk, mill }:

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
  ];

  JAVA_HOME = "${jdk}";
  LS_JAVA_VERSION = "25";
  # The SQLite shared library consumed by the ls-sqlite-ffm FFM binding.
  # System SQLite is never used; only the Nix-provided library is a valid
  # runtime dependency.
  LS_SQLITE_LIB = "${pkgs.sqlite.out}/lib/libsqlite3${pkgs.stdenv.hostPlatform.extensions.sharedLibrary}";
}
