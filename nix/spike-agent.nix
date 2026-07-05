# The M0 spike island agent jar (mill `pcHostSpike.assembly`).
#
# A self-contained `-javaagent` assembly (Scala premain + jextract FFM bindings
# + scala-library) whose `premain` fires inside `JNI_CreateJavaVM`. Built offline
# from the locked ivy cache, exactly like the main package.
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
  pname = "ls-pc-host-spike-agent";
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
    mill --no-daemon pcHostSpike.assembly
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp out/pcHostSpike/assembly.dest/out.jar $out/spike-agent.jar
    runHook postInstall
  '';

  dontShrink = true;
  dontPatchELF = true;

  meta = {
    description = "M0 embedded-JVM boundary spike island (-javaagent premain, FFM/jextract).";
    platforms = lib.platforms.linux;
  };
}
