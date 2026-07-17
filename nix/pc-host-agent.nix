# The embedded-JVM presentation-compiler island host agent jar
# (mill `pcHost.assembly`).
#
# A self-contained `-javaagent` assembly (Scala premain + jextract FFM bindings
# + scala-library) whose `premain` fires inside `JNI_CreateJavaVM`. Built
# offline from the locked ivy cache, exactly like the main package and the
# boundary spike agent.
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
  pname = "ls-pc-host-agent";
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
    mill --no-daemon pcHost.assembly
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall
    mkdir -p $out
    cp out/pcHost/assembly.dest/out.jar $out/pc-host-agent.jar
    runHook postInstall
  '';

  dontShrink = true;
  dontPatchELF = true;

  # The locked coursier cache, exposed for the offline-compile guard
  # (scripts/check-offline-compile.sh seeds its cold cache from it).
  passthru = { inherit ivyCache; };

  meta = {
    description = "Embedded-JVM presentation-compiler island host (-javaagent premain, FFM/jextract).";
    platforms = lib.platforms.linux;
  };
}
