# The production package: the Rust `ls-server` binary (crane-built cargo
# workspace) wrapped with the presentation-compiler island artifacts.
#
# The wrapper bakes the nix-provided defaults for the embedded JVM boot —
# `JAVA_HOME` (the exact JDK the flake pins) and `PC_HOST_AGENT_JAR` (the
# mill-built island host agent assembly) — via `--set-default`, so the
# resolution precedence stays config > env > nix-baked: a workspace
# `.scala3-bsp-semantic-ls/config.json` `javaHome` wins, then the caller's
# environment, then these baked defaults. An index-only session never touches
# them: the JVM boots lazily on the first presentation-compiler query.
#
# The same discipline bakes `LS_SCALAFMT` (the nixpkgs scalafmt CLI) for
# `textDocument/formatting`: workspace config `scalafmt` > the caller's
# `LS_SCALAFMT` > this baked default. The baked scalafmt is ONE fixed version
# and the server spawns it with `COURSIER_MODE=offline`, so a workspace
# `.scalafmt.conf` pinning a different version fails typed instead of
# downloading jars (the offline stance, docs/deployment.md).
{ lib
, stdenvNoCC
, jdk
, rustWorkspace
, pcHostAgentJar
, scalafmt
, makeWrapper
}:

stdenvNoCC.mkDerivation {
  pname = "scala3-bsp-semantic-ls";
  version = "0.1.0";

  dontUnpack = true;

  nativeBuildInputs = [ makeWrapper ];

  installPhase = ''
    runHook preInstall

    mkdir -p $out/bin $out/share/scala3-bsp-semantic-ls
    cp ${pcHostAgentJar}/pc-host-agent.jar \
      $out/share/scala3-bsp-semantic-ls/pc-host-agent.jar
    cp ${../modules/ls-pc/resources/default-plugin-schema.json} \
      $out/share/scala3-bsp-semantic-ls/default-plugin-schema.json

    makeWrapper ${rustWorkspace}/bin/ls-server $out/bin/scala3-bsp-semantic-ls \
      --set-default JAVA_HOME "${jdk.home}" \
      --set-default PC_HOST_AGENT_JAR "$out/share/scala3-bsp-semantic-ls/pc-host-agent.jar" \
      --set-default LS_SCALAFMT "${scalafmt}/bin/scalafmt"

    runHook postInstall
  '';

  meta = {
    description = "Scala 3 + BSP + SemanticDB-first language server: Rust core with an embedded presentation-compiler JVM island";
    mainProgram = "scala3-bsp-semantic-ls";
    # Linux only by decision: the embedded-libjvm boundary is exercised and
    # supported on Linux exclusively; macOS is explicitly unsupported.
    platforms = lib.platforms.linux;
  };
}
