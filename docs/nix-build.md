# Nix Build Contract

> Normative contract for toolchain, dependency locking, packaging, and CI.
> Derived from the real `flake.nix`, `nix/rust.nix`, `nix/package.nix`,
> `nix/checks.nix`, `nix/dev-shell.nix`, `nix/pc-host-agent.nix`,
> `nix/zaozi-pcplugin.nix`, `nix/spike-agent.nix`, `nix/ivy-lock.nix`, and the
> `scripts/` in this repository.

The build is two-tier, mirroring the product (see the v2 decision record,
plan-rust.md §0): the deployable server is the **crane-built Rust cargo
workspace** under `crates/`, and Mill builds only the **JVM island artifacts**
— the PC-host agent jar and the zaozi PC plugin jar — from a locked ivy cache.

## 1. Toolchain contract

```text
Rust (stable, nixpkgs)  crane-built cargo workspace (crates/); vendored offline from Cargo.lock
Java 25                 (pkgs.jdk25; island-only — the embedded PC island runtime and the mill toolchain)
Scala 3.8.4             (pinned in build.mill: Deps.scalaVer; the bundled PC version)
Mill 1.1.2              (pkgs.millVersions.mill_1_1_2 via mill-ivy-fetcher's mill-overlay)
Nix >= 2.28             (required by mill-ivy-fetcher)
mill-ivy-fetcher input  (mandatory flake input, pinned in flake.lock)
```

Hard rules:

```text
1. The project must provide flake.nix and flake.lock.
2. flake inputs must include: mill-ivy-fetcher.url = "github:Avimitin/mill-ivy-fetcher";
3. The development environment must be entered via nix develop.
4. Mill (island) dependencies must be locked into a Nix expression via mill-ivy-fetcher;
   Rust dependencies are locked by the committed Cargo.lock.
5. CI must run through nix flake check / nix develop.
6. Local Coursier caches and system JDKs are never formal build dependencies.
7. sbt, Maven, and Gradle are not build tools for this project.
```

`nix develop` is the only formal entry point for development, building,
locking, and CI. The `./mill` script at the repo root is a thin launcher only:
it `exec`s whatever `mill` is on `PATH` and errors out with instructions to
enter `nix develop` otherwise. It deliberately performs **no** network
bootstrap — outside the flake dev shell this project does not build.

## 2. The flake as it exists

Inputs: `nixpkgs` (nixos-unstable), `flake-utils`, `crane` (builds the Rust
workspace and supplies the fmt/clippy/test/build checks), `mill-ivy-fetcher`
(hard requirement; its two overlays provide `pkgs.mill-ivy-fetcher` (the `mif`
tool), `pkgs.millVersions.*`, and the packaging helpers `ivy-gather` +
`configure-mill-env-hook`), and `zaozi` (a pinned non-flake source input; the
real third-party Scala 3 project behind the zaozi plugin validation).

Systems are **Linux only by decision**: `flake-utils.lib.eachSystem
["x86_64-linux" "aarch64-linux"]`. The embedded-libjvm boundary (dlopen +
`JNI_CreateJavaVM` + `/proc/self/maps` assertions) is exercised and supported
on Linux exclusively; macOS is explicitly unsupported.

Per-system outputs:

| Output | Definition |
|---|---|
| `devShells.default` | `nix/dev-shell.nix` (§6) |
| `formatter` | `pkgs.nixpkgs-fmt` |
| `checks.*` | `nix/checks.nix` + the crane checks + the boundary/package checks (§5) |
| `packages.default` | `nix/package.nix` — the wrapped Rust server + island jars (§3.1) |
| `packages.rust-workspace` | the crane-built cargo workspace on its own |
| `packages.pc-host-agent-jar` | the PC island host agent assembly (`mill pcHost.assembly`) |
| `packages.zaozi-pcplugin-jar` | the zaozi PC compiler plugin jar (`mill zaoziPcplugin.jar`) |
| `packages.spike-agent-jar` | the boundary-spike island agent jar (`mill pcHostSpike.assembly`) |
| `packages.mill`, `packages.mill-ivy-fetcher` | the pinned Mill and `mif` (so `nix shell '.#mill' '.#mill-ivy-fetcher' -c mif run …` works) |
| `packages.zaozi-src` | the pinned zaozi source with `nix/patches/zaozi-semanticdb.patch` applied |

## 3. The Rust workspace build (`nix/rust.nix`)

Crane builds the cargo workspace from an exact fileset — `Cargo.toml`,
`Cargo.lock`, `rustfmt.toml`, and `crates/` — so a Scala/mill change never
invalidates the Rust derivations. Dependencies are **vendored from the
committed `Cargo.lock`** (offline/reproducible); `craneLib.buildDepsOnly`
builds the dependency closure once and every check shares those artifacts. The
buildable workspace is exposed as `nix build .#rust-workspace`, and the same
`commonArgs`/`cargoArtifacts` are reused by the live PC checks (§5), which add
the island boot env plus a scoped test filter on top.

### 3.1 Package assembly (`nix/package.nix`)

`nix build .#default` produces:

```text
bin/scala3-bsp-semantic-ls                              # makeWrapper launcher around the crane-built ls-server binary
share/scala3-bsp-semantic-ls/pc-host-agent.jar          # the mill-built island host agent (-javaagent premain assembly)
share/scala3-bsp-semantic-ls/zaozi-pcplugin.jar         # the mill-built zaozi PC plugin (scalac -Xplugin)
share/scala3-bsp-semantic-ls/default-plugin-schema.json # from modules/ls-pc/resources/
```

The wrapper bakes the embedded-JVM boot defaults via `--set-default` —
`JAVA_HOME` (the flake's JDK 25) and `PC_HOST_AGENT_JAR` (the shipped agent
jar) — so the runtime resolution precedence stays **config > env > nix-baked**:
a workspace `.scala3-bsp-semantic-ls/config.json` `javaHome` wins, then the
caller's environment (`LS_LIBJVM` exact-path override, else `JAVA_HOME`), then
these baked defaults. `libjvm` is located at `<javaHome>/lib/server/libjvm.so`.
An index-only session never touches any of it: the JVM boots lazily on the
first presentation-compiler query. `meta.platforms = lib.platforms.linux`.

## 4. The island jars & dependency locking

`nix/pc-host-agent.nix`, `nix/zaozi-pcplugin.nix`, and `nix/spike-agent.nix`
build the three Mill artifacts offline: each takes only `build.mill` +
`modules/` as source, consumes the fixed-output ivy cache built by
`ivy-gather ./ivy-lock.nix`, and runs `mill --no-daemon
pcHost.assembly` / `zaoziPcplugin.jar` / `pcHostSpike.assembly` (`--no-daemon`
because the daemon path resolves its runner from the network, which the
sandbox forbids).

`nix/ivy-lock.nix` is the generated Nix expression of the Maven/ivy dependency
closure — **now shrunk to the island modules only** (the Mill build's whole
surface after the rewrite). Supported regeneration command (run from the repo
root):

```bash
nix develop -c ./scripts/regen-ivy-lock.sh
```

The script wraps `mif fetch` + `mif codegen` with three determinism guards
that plain `mif run` lacks: (1) Mill's launcher resolves its own runtime
through the coursier cache derived from java's `user.home` — the script points
`user.home` at a cold directory and merges launcher downloads into the cache
`mif codegen` hashes, so a warm host cache cannot silently drop artifacts from
the lock; (2) a PATH shim forces `mill --no-daemon`; (3) mif is handed a clean
copy of `build.mill` + `modules/` only, so a stale `out/mill-launcher`
resolved-classpath file cannot let the launcher skip resolution. The flake
exports `.#mill` / `.#mill-ivy-fetcher` for the README-style `mif run`
variant, but prefer the script for the guards above.

The lock rules — all mandatory:

```text
1. After changing dependencies in build.mill, nix/ivy-lock.nix must be regenerated.
2. A PR that changes build.mill dependencies must also commit nix/ivy-lock.nix.
3. CI must verify ivy-lock.nix is consistent with build.mill.
4. CI must never resolve unlocked dependencies online.
```

Rule 3 is `scripts/check-ivy-lock.sh` (regenerates inside `nix develop` and
diffs against the committed lock, failing on drift or a missing file). Rule 4
is additionally enforced by the offline-compile guard,
`scripts/check-offline-compile.sh`: it seeds a temp coursier cache from the
flake's ivyCache, forces coursier offline under a **cold** cache boundary
(isolating `HOME`/`XDG_CACHE_HOME`/`user.home`, not just `COURSIER_CACHE`),
and runs `mill --no-daemon __.compile`; `--self-test` appends a deliberately
unlocked dependency and requires the offline compile to **fail**, proving the
guard actually rejects what the lock does not carry.

## 5. Checks (`nix flake check`)

| Check | What it enforces |
|---|---|
| `java25-toolchain` | `java -version` from the flake toolchain reports major version 25 |
| `ivy-lock-present` | `nix/ivy-lock.nix` exists, parses as Nix, and locks a non-trivial artifact set |
| `mill-ivy-fetcher-input` | `flake.nix` literally pins `mill-ivy-fetcher.url = "github:Avimitin/mill-ivy-fetcher"` |
| `package` | the full offline `packages.default` build succeeds |
| `rust-build` | the cargo workspace builds (the crane workspace derivation) |
| `rust-test` | `cargo test --workspace` (live-JVM tests skip here: no boot env is set) |
| `rust-clippy` | `cargo clippy --all-targets --workspace -- -D warnings` |
| `rust-fmt` | `cargo fmt` cleanliness against the committed `rustfmt.toml` |
| `spike-boundary` | boots the spike island through the crane-built `ls-jvm-spike` binary and drives every boundary scenario (echo / java-throw / rust-panic / timeout / bad-canary) |
| `pc-host-agent` | the agent jar builds offline and its manifest declares `Premain-Class: ls.pc.host.PcHostAgent` (a valid `-javaagent`) |
| `package-cli` | the packaged binary works offline: `--version` prints the identity, `--doctor` renders the Store section pre-bootstrap, `dump` reports an absent store gracefully, and both island jars are shipped under `share/` |
| `pc-boundary` | live `ls-jvm` test: boots the production island against a real JVM and drives register/open/completion/hover through the 15-slot vtable |
| `pc-recovery` | live dispatch-generation recovery: a real wedged completion is recovered by the watchdog; the generation cap turns into a fatal |
| `pc-definition` | live cross-file go-to: the full FFM round-trip through the symbol-resolver slot with a real snapshot-backed resolver |
| `pc-zaozi` | live zaozi navigation: the shipped plugin, loaded through a workspace `pc-plugins.json`, steers go-to on a zaozi dynamic field access |
| `pc-server-definition` | live `ls-server` test: `textDocument/definition` through the real `CoreHandlers` dispatch into the booted island |

The live PC checks reuse the shared crane artifacts and are handed the boot
inputs (`LS_LIBJVM`, `PC_HOST_AGENT_JAR`, `LS_PC_TARGET_CLASSPATH`, and for
`pc-zaozi` also `ZAOZI_PCPLUGIN_JAR`) as derivation env; the target classpath
is the pinned Scala 3.8.4 standard-library jars, version-matched to the
compiler bundled in the PC-host assembly.

The hermetic checks never touch a `.bsp/` directory — real-BSP coverage lives
in the gated scripts (§7): `scripts/it-real-bsp-rs.sh` and
`scripts/it-mill-smoke.sh` copy `it/sample-workspace` aside and run
`mill mill.bsp.BSP/install` there to write the real `.bsp/mill-bsp.json` the
server's production discovery then finds.

## 6. Dev shell (`nix/dev-shell.nix`)

Packages: `jdk` (JDK 25), `mill` (1.1.2), `mill-ivy-fetcher`, `git`, `jq`; the
Rust toolchain (`rustc`, `cargo`, `clippy`, `rustfmt`, `rust-analyzer`);
`protobuf` (protoc for the SemanticDB prost codegen), `rust-cbindgen` and
`jextract` (the boundary-binding generators behind
`scripts/regen-pc-abi-bindings.sh`, `scripts/regen-pc-host-bindings.sh`, and
`scripts/regen-spike-bindings.sh` — the generated header/bindings are
committed, so only regeneration needs them).

Exported environment — contract, code and scripts rely on these:

| Variable | Value | Meaning |
|---|---|---|
| `JAVA_HOME` | `${jdk}` | the Nix JDK 25; the only JDK the island build/boot may use |
| `LS_JAVA_VERSION` | `"25"` | the single supported island runtime major |
| `RUST_SRC_PATH` | rust stdlib source | rust-analyzer std resolution |
| `PROTOC` | `${protobuf}/bin/protoc` | SemanticDB prost codegen (`ls-semanticdb`) |
| `LS_LIBJVM` | `${jdk.home}/lib/server/libjvm.so` | the exact libjvm the embedded island dlopens (on nixpkgs jdk25 it lives under `jdk.home`, not `$JAVA_HOME/lib/server`) |
| `PC_HOST_AGENT_JAR` | the mill-built agent jar | island boot input for the live PC tests and dev-shell servers |
| `LS_PC_TARGET_CLASSPATH` | pinned scala-library + scala3-library jars | the classpath live-PC test targets register |
| `ZAOZI_PCPLUGIN_JAR` | the zaozi plugin jar | lets the live zaozi row run instead of skipping |
| `ZAOZI_SRC` | the patched zaozi tree | pinned real-repo workspace source for manual validation |

With these set, `cargo test` in the dev shell runs the live embedded-JVM tests
for real (the same tests skip in the hermetic `rust-test` check, which sets
none of them).

## 7. CI command set (`.github/workflows/ci.yml`)

```bash
nix flake check                                  # everything in §5
nix develop -c mill __.compile                   # the retained island modules (pc, pcHost, pcHostSpike, zaoziPcplugin)
nix develop -c mill __.test                      # the island test suites
nix develop -c cargo run -p ls-bench -- --smoke  # bench smoke over the Rust engine
nix develop -c ./scripts/check-ivy-lock.sh       # lock freshness (rule 3, §4)
./scripts/check-docs.sh                          # docs/traceability + stale-claim gate
./scripts/check-audit-inventory.sh               # coverage-audit accounts for every retained suite
nix develop -c ./scripts/it-real-bsp-rs.sh       # separate job: real-BSP e2e (real mill --bsp, .bsp discovery, embedded-PC rows, cold-start zero-JVM assertion)
```

Expectations these commands must keep true: `nix flake check` passes;
`nix develop` launches Java 25 and the Rust toolchain; the cargo workspace and
the island modules compile under `nix develop`; the ivy-lock refresh check
passes and no ivy dependency resolves outside the nix lock; the package wraps
the native `ls-server` binary and ships both island jars.
