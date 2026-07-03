package ls.bench

import java.nio.file.{Files, Path}
import java.nio.file.attribute.PosixFilePermissions

/** The offline-compile guard's `--self-test` must inject an unlocked dependency
  * into a scratch copy and REQUIRE the offline compile to fail. This exercises
  * that orchestration with a stub compile command (fails iff an unlocked
  * dependency is present), so it runs fast and without a Nix build; real CI
  * runs the same script with the real offline mill compile.
  */
class OfflineCompileGuardSuite extends munit.FunSuite:

  private def repoRoot: Path =
    Iterator
      .iterate(Path.of("").toAbsolutePath.normalize)(p => p.getParent)
      .takeWhile(_ != null)
      .find(p => Files.isRegularFile(p.resolve("scripts").resolve("check-offline-compile.sh")))
      .getOrElse(fail("could not locate scripts/check-offline-compile.sh above the test cwd"))

  /** Writes an executable stub that mimics offline resolution: it fails iff the
    * project's build.mill names the unlocked probe dependency.
    */
  private def stubCompile(dir: Path, body: String): Path =
    val stub = dir.resolve("offline-compile-stub.sh")
    Files.writeString(stub, s"#!/usr/bin/env bash\n$body\n")
    Files.setPosixFilePermissions(stub, PosixFilePermissions.fromString("rwxr-xr-x"))
    stub

  private def runSelfTest(stub: Path): (Int, String) =
    val pb = new ProcessBuilder(
      "bash",
      repoRoot.resolve("scripts").resolve("check-offline-compile.sh").toString,
      "--self-test"
    )
    pb.environment().put("OFFLINE_COMPILE_CMD", stub.toString)
    // the guard isolates HOME; the stub compares against this original value
    pb.environment().put("ORIG_HOME", Option(pb.environment().get("HOME")).getOrElse("/nonexistent-orig-home"))
    pb.redirectErrorStream(true)
    val proc = pb.start()
    val output = new String(proc.getInputStream.readAllBytes())
    (proc.waitFor(), output)

  // Stub that keeps its teeth ONLY under an isolated cold cache: it fails when
  // the unlocked dep is present (cold cache cannot resolve it), but models a
  // warm/leaky cache silently resolving it (exit 0) when the guard failed to
  // isolate HOME/XDG_CACHE_HOME/COURSIER_*/user.home. So reverting the script's
  // env isolation flips this to a passing compile and the self-test fails.
  private val toothedUnderIsolation =
    """isolated() {
      |  [[ -n "${HOME:-}" && "$HOME" != "${ORIG_HOME:-}" \
      |     && "$XDG_CACHE_HOME" == "$HOME/.cache" \
      |     && -n "${COURSIER_CACHE:-}" \
      |     && "$COURSIER_MODE" == "offline" \
      |     && "$JAVA_TOOL_OPTIONS" == *"-Duser.home=$HOME"* ]]
      |}
      |if isolated; then
      |  if grep -q 'definitely-not-locked' build.mill; then exit 1; else exit 0; fi
      |else
      |  exit 0
      |fi""".stripMargin

  test("--self-test passes only when the guard isolates a cold cache and the unlocked dep fails"):
    val dir = Files.createTempDirectory("offline-guard-ok")
    val stub = stubCompile(dir, toothedUnderIsolation)
    val (code, out) = runSelfTest(stub)
    assertEquals(code, 0, s"self-test should pass (cold-cache guard rejected the unlocked dep):\n$out")

  test("--self-test fails when the offline compile does NOT reject the unlocked dependency"):
    val dir = Files.createTempDirectory("offline-guard-toothless")
    // a toothless guard: the compile always succeeds, so the self-test must flag it
    val stub = stubCompile(dir, "exit 0")
    val (code, out) = runSelfTest(stub)
    assertNotEquals(code, 0, s"self-test must fail when an unlocked dep is not rejected:\n$out")
