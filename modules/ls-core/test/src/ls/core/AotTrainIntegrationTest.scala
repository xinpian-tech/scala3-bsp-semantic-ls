package ls.core

import java.nio.file.{Files, Path}

import scala.concurrent.duration.{Duration, DurationInt}

import RealBspFixture.{copyTree, javaBin, repoRoot, runFlags, runProcess}

/** AOT-training integration test, gated on `LS_AOT_IT=1` and script-driven like
  * [[RealBspIntegrationTest]]. Positive: install a REAL Mill BSP connection in a
  * sample-workspace copy, then `scripts/aot-train.sh` runs the strict real-BSP
  * training (compile + reindex + non-empty index queries) and produces a
  * non-empty cache; a follow-up `Main --version` with `-XX:AOTCache` exits 0 and
  * a headless `Main --doctor` with `-XX:AOTCache` reports `AOT cache: loaded`.
  * Negative: `Main --aot-train` over a workspace with no `.bsp` exits cleanly
  * without hanging.
  *
  * Run with:  nix develop -c ./scripts/it-aot.sh
  */
class AotTrainIntegrationTest extends munit.FunSuite:

  override def munitTimeout: Duration = 900.seconds

  private val enabled = sys.env.get("LS_AOT_IT").contains("1")

  private object env:
    /** The pre-built server assembly jar. scripts/it-aot.sh builds it and passes
      * its path in LS_AOT_ASSEMBLY_JAR; the test must never invoke `mill` itself
      * (a nested `mill` inside the outer `mill core.test` run deadlocks on the
      * build lock).
      */
    lazy val jar: Path =
      val j = sys.env
        .get("LS_AOT_ASSEMBLY_JAR")
        .map(Path.of(_))
        .getOrElse(fail("LS_AOT_ASSEMBLY_JAR is unset; run this test via scripts/it-aot.sh"))
      assert(Files.isRegularFile(j), s"assembly jar not found at $j (run scripts/it-aot.sh)")
      j

  test("aot-train produces a loadable cache from the real BSP workload"):
    assume(enabled, "set LS_AOT_IT=1 to run the AOT training integration test")
    val jar = env.jar
    val ws = Files.createTempDirectory("ls-aot-it-ws-").toRealPath()
    copyTree(repoRoot.resolve("it").resolve("sample-workspace"), ws)

    // install a REAL Mill BSP connection so the training run drives the strict
    // real-BSP workload (compile + reindex + SemanticDB-backed queries).
    val (installRc, installLog) = runProcess(Vector("mill", "--no-daemon", "mill.bsp.BSP/install"), ws, 10)
    assert(installRc == 0, s"mill.bsp.BSP/install failed:\n$installLog")
    assert(
      Files.isRegularFile(ws.resolve(".bsp").resolve("mill-bsp.json")),
      s"mill.bsp.BSP/install did not write .bsp/mill-bsp.json:\n$installLog"
    )
    val out = Files.createTempDirectory("ls-aot-it-out-").resolve("aot-cache.bin")

    // build the cache via the production script; .bsp presence makes it strict.
    val (trainRc, trainLog) = runProcess(
      Vector("bash", repoRoot.resolve("scripts").resolve("aot-train.sh").toString,
        "--workspace", ws.toString, "--out", out.toString),
      repoRoot,
      15
    )
    assert(trainRc == 0, s"aot-train.sh (strict real-BSP) failed:\n$trainLog")
    // the strict workload must have compiled, reindexed docs, and queried the index
    assert(trainLog.contains("strict real-BSP training"), s"training was not strict:\n$trainLog")
    assert(Files.isRegularFile(out), s"cache not produced:\n$trainLog")
    assert(Files.size(out) > 0L, s"cache is empty:\n$trainLog")

    // a follow-up boot with the cache loads it and exits cleanly
    val (versionRc, versionLog) = runProcess(
      Vector(javaBin) ++ runFlags ++ Vector(s"-XX:AOTCache=$out", "-jar", jar.toString, "--version"),
      repoRoot,
      5
    )
    assert(versionRc == 0, s"--version with -XX:AOTCache failed:\n$versionLog")

    // the doctor, run in a JVM that loaded the cache, reports it as loaded
    val (doctorRc, doctorLog) = runProcess(
      Vector(javaBin) ++ runFlags ++
        Vector(s"-XX:AOTCache=$out", "-jar", jar.toString, "--doctor", ws.toString),
      repoRoot,
      5
    )
    assert(doctorRc == 0, s"--doctor with -XX:AOTCache failed:\n$doctorLog")
    assert(doctorLog.contains("AOT cache: loaded"), s"doctor did not report a loaded cache:\n$doctorLog")

  test("aot-train exits cleanly with no .bsp present (no hang)"):
    assume(enabled, "set LS_AOT_IT=1 to run the AOT training integration test")
    val jar = env.jar
    val bare = Files.createTempDirectory("ls-aot-it-nobsp-")
    Files.writeString(bare.resolve("Empty.scala"), "package p\nobject Empty\n")
    val (rc, log) = runProcess(
      Vector(javaBin) ++ runFlags ++
        Vector("-jar", jar.toString, "--aot-train", bare.toString, "--in-process-pc"),
      repoRoot,
      5
    )
    assert(rc == 0, s"--aot-train over a no-.bsp workspace did not exit cleanly:\n$log")
