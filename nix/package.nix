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
{ lib
, stdenvNoCC
, jdk
, rustWorkspace
, pcHostAgentJar
, zaoziPcpluginJar
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
    cp ${zaoziPcpluginJar}/zaozi-pcplugin.jar \
      $out/share/scala3-bsp-semantic-ls/zaozi-pcplugin.jar
    cp ${../modules/ls-pc/resources/default-plugin-schema.json} \
      $out/share/scala3-bsp-semantic-ls/default-plugin-schema.json

    makeWrapper ${rustWorkspace}/bin/ls-server $out/bin/scala3-bsp-semantic-ls \
      --set-default JAVA_HOME "${jdk.home}" \
      --set-default PC_HOST_AGENT_JAR "$out/share/scala3-bsp-semantic-ls/pc-host-agent.jar"

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
