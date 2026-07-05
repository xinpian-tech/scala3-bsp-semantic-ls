# The zaozi presentation-compiler plugin jar (mill `zaoziPcplugin.jar`).
#
# A scalac compiler plugin (`plugin.properties` -> `ls.zaozi.pcplugin.
# ZaoziPcDefinitionPlugin`) the PC loads via `-Xplugin:<jar>` (the
# `pc-plugins.json` `compilerPlugins` mechanism). Built offline from the locked
# ivy cache, exactly like the main package and the pc-host agent. This is the
# same jar `nix/package.nix` ships under `share/.../zaozi-pcplugin.jar`; exposing
# it standalone lets the live zaozi vtable-nav check load it.
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
  pname = "ls-zaozi-pcplugin";
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
    mill --no-daemon zaoziPcplugin.jar
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp out/zaoziPcplugin/jar.dest/out.jar $out/zaozi-pcplugin.jar
    runHook postInstall
  '';

  dontShrink = true;
  dontPatchELF = true;

  meta = {
    description = "Zaozi presentation-compiler go-to plugin (scalac -Xplugin jar).";
    platforms = lib.platforms.linux;
  };
}
