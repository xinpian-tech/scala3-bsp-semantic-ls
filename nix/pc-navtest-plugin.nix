# The generic PC-navigation test-fixture plugin jar (mill `pcNavTestPlugin.jar`).
#
# A scalac compiler plugin (`plugin.properties` -> `ls.pc.navtest.
# NavTestPcPlugin`) that exists solely to prove the `pc-plugins.json`
# `compilerPlugins` product mechanism: the `pc-plugin-load` flake check loads
# it into the live embedded island via a workspace `pc-plugins.json` and
# observes its steering of the fixture Dynamic shape over the vtable
# (`crates/ls-jvm/tests/live_pcplugin.rs`). Built offline from the locked ivy
# cache, exactly like the spike agent — a CHECK input, never shipped in the
# package.
{ lib
, stdenvNoCC
, mill
, jdk
, ivy-gather
, configure-mill-env-hook
}:

let
  ivyCache = ivy-gather ./ivy-lock.nix;

  src = with lib.fileset; toSource {
    root = ../.;
    fileset = unions [
      ../build.mill
      ../modules
    ];
  };
in
stdenvNoCC.mkDerivation {
  pname = "ls-pc-navtest-plugin";
  version = "0.1.0";

  inherit src;

  nativeBuildInputs = [
    mill
    jdk
    configure-mill-env-hook
  ];

  buildInputs = [
    ivyCache
  ];

  buildPhase = ''
    runHook preBuild
    # --no-daemon: the daemon path resolves mill-runner-daemon from the network,
    # which the sandbox forbids and the ivy lock does not carry.
    mill --no-daemon pcNavTestPlugin.jar
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp out/pcNavTestPlugin/jar.dest/out.jar $out/pc-navtest-plugin.jar
    runHook postInstall
  '';

  dontShrink = true;
  dontPatchELF = true;

  meta = {
    description = "Generic PC-navigation test-fixture compiler plugin (scalac -Xplugin jar; check input, not shipped).";
    platforms = lib.platforms.linux;
  };
}
