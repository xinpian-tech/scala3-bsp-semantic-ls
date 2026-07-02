{ lib
, stdenvNoCC
, mill
, jdk
, sqlite
, ivy-gather
, configure-mill-env-hook
, makeWrapper
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
  pname = "scala3-bsp-semantic-ls";
  version = "0.1.0";

  inherit src;

  nativeBuildInputs = [
    mill
    jdk
    configure-mill-env-hook
    makeWrapper
  ];

  buildInputs = [
    ivyCache
  ];

  buildPhase = ''
    runHook preBuild
    mill -i core.assembly
    runHook postBuild
  '';

  installPhase = ''
    runHook preInstall

    mkdir -p $out/bin $out/lib/scala3-bsp-semantic-ls $out/share/scala3-bsp-semantic-ls
    cp out/core/assembly.dest/out.jar $out/lib/scala3-bsp-semantic-ls/scala3-bsp-semantic-ls.jar
    cp modules/ls-pc/resources/default-plugin-schema.json \
      $out/share/scala3-bsp-semantic-ls/default-plugin-schema.json

    # Java 25 only runtime. The assembly runs on the class path, so native
    # access for the ls-sqlite-ffm FFM binding is granted via ALL-UNNAMED
    # (the module-path spelling would be --enable-native-access=ls.sqlite.ffm).
    makeWrapper ${jdk}/bin/java $out/bin/scala3-bsp-semantic-ls \
      --set JAVA_HOME "${jdk}" \
      --set-default LS_SQLITE_LIB "${sqlite.out}/lib/libsqlite3${stdenvNoCC.hostPlatform.extensions.sharedLibrary}" \
      --add-flags "--enable-native-access=ALL-UNNAMED" \
      --add-flags "-XX:+UseCompactObjectHeaders" \
      --add-flags "\''${LS_AOT_CACHE:+-XX:AOTCache=\$LS_AOT_CACHE}" \
      --add-flags "-jar $out/lib/scala3-bsp-semantic-ls/scala3-bsp-semantic-ls.jar"

    runHook postInstall
  '';

  dontShrink = true;
  dontPatchELF = true;

  passthru = { inherit ivyCache; };

  meta = {
    description = "Scala 3 + BSP + SemanticDB-first language server with SQLite/mmap exact indexing";
    mainProgram = "scala3-bsp-semantic-ls";
    platforms = lib.platforms.linux ++ lib.platforms.darwin;
  };
}
