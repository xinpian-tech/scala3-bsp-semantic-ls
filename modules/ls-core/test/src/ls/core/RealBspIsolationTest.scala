package ls.core

import java.nio.file.{Files, Path}
import java.util.concurrent.TimeUnit

import scala.concurrent.duration.{Duration, DurationInt}
import scala.jdk.CollectionConverters.*

import org.eclipse.lsp4j.*

/** Real-BSP Isolation batch: the process-isolation rows of the real-`mill --bsp`
  * end-to-end suite.
  *
  *   - E7 (gated `LS_REAL_BSP_IT=1`): the forked presentation compiler over a real
  *     Mill BSP session survives a worker-process kill — the next completion
  *     respawns the worker and replays the buffer, and the language server stays
  *     up. Boots its OWN forked-PC [[RealBspServer]] over a private workspace.
  *   - E9 (gated `LS_AOT_IT=1`, driven by `scripts/it-aot.sh` which builds the
  *     assembly and passes `LS_AOT_ASSEMBLY_JAR`): a boot WITH a trained AOT cache
  *     loads it (`AOT cache: loaded`) and stays queryable — the cached JVM drives
  *     the strict real-BSP index workload (compile + reindex + workspace/symbol +
  *     references) to exit 0.
  */
class RealBspIsolationTest extends munit.FunSuite:

  import RealBspFixture.{consumerUri, enabled, javaBin, runFlags, runProcess}

  override def munitTimeout: Duration = 900.seconds

  // A forked-PC server over its own real-BSP workspace; lazy so the ungated run
  // never boots mill-bsp or spawns a worker.
  private lazy val e7: RealBspServer =
    new RealBspServer(RealBspFixture.freshWorkspace(), pcBackendMode = PcBackendMode.Forked)

  private def guard(): Unit =
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")

  // -------------------------------------------------------------------- E7

  test("E7 forked PC over real BSP survives a worker kill"):
    guard()
    val _ = e7.readyIndex
    // dirty buffer with a member-select completion the forked worker must answer
    val dirty = e7.ws.sourceText(consumerUri) + "  val probe = greeting.mess\n"
    e7.docsService.didOpen(
      new DidOpenTextDocumentParams(
        new TextDocumentItem(e7.ws.fileUri(consumerUri), "scala", 1, dirty)
      )
    )
    try
      val line = dirty.linesIterator.length - 1
      val character = "  val probe = greeting.mess".length

      def completeLabels(): Vector[String] =
        val params = new CompletionParams(e7.textDoc(consumerUri), new Position(line, character))
        val r = e7.docsService.completion(params).get(180, TimeUnit.SECONDS)
        (if r.isRight then r.getRight.getItems.asScala.toVector else r.getLeft.asScala.toVector)
          .map(_.getLabel)

      // first completion spawns the worker and returns `message`
      assert(completeLabels().exists(_.startsWith("message")), "initial forked completion")

      // the doctor reports the forked worker as alive
      assert(
        e7.executeCommand(ScalaLs.Commands.Doctor).contains("forked worker alive"),
        "doctor should report a live forked worker"
      )

      // fault injection: kill the worker's OS process; the LS stays up and the
      // next completion respawns + replays the buffer.
      val pid = e7.server.currentState.ready.get.pc.workerPid.getOrElse(fail("no forked worker pid"))
      val handle = ProcessHandle.of(pid)
      assert(handle.isPresent, s"no OS process for pid $pid")
      handle.get.destroyForcibly()
      handle.get.onExit().get(30, TimeUnit.SECONDS)

      assert(completeLabels().exists(_.startsWith("message")), "completion after respawn")
      assert(
        e7.server.currentState.ready.get.pc.workerAlive.contains(true),
        "forked worker should be alive again after respawn"
      )
    finally
      e7.docsService.didClose(new DidCloseTextDocumentParams(e7.textDoc(consumerUri)))

  // -------------------------------------------------------------------- E9

  test("E9 an AOT-trained boot loads the cache and stays queryable"):
    assume(sys.env.get("LS_AOT_IT").contains("1"), "set LS_AOT_IT=1 (run scripts/it-aot.sh)")
    val jar = sys.env
      .get("LS_AOT_ASSEMBLY_JAR")
      .map(Path.of(_))
      .getOrElse(fail("LS_AOT_ASSEMBLY_JAR is unset; run this test via scripts/it-aot.sh"))
    assert(Files.isRegularFile(jar), s"assembly jar not found at $jar (run scripts/it-aot.sh)")

    // A real Mill BSP workspace (`.bsp` installed) so the training + cached boot
    // drive the strict real-BSP index workload.
    val ws = RealBspFixture.freshWorkspace().root
    val repo = RealBspFixture.repoRoot
    val cache = Files.createTempDirectory("ls-e9-aot-out-").resolve("aot-cache.bin")

    // train the cache via the production script (strict, because .bsp is present).
    val (trainRc, trainLog) = runProcess(
      Vector("bash", repo.resolve("scripts").resolve("aot-train.sh").toString,
        "--workspace", ws.toString, "--out", cache.toString),
      repo,
      15
    )
    assert(trainRc == 0, s"aot-train.sh (strict real-BSP) failed:\n$trainLog")
    assert(Files.isRegularFile(cache) && Files.size(cache) > 0L, s"cache not produced:\n$trainLog")

    // boot WITH the cache and drive the strict queryable workload -> exit 0
    // (compile + reindex + non-empty workspace/symbol + references + completion).
    // --in-process-pc matches the cache (trained in-process by aot-train.sh); the
    // production default is now forked.
    val (qRc, qLog) = runProcess(
      Vector(javaBin) ++ runFlags ++
        Vector(s"-XX:AOTCache=$cache", "-jar", jar.toString,
          "--aot-train", ws.toString, "--require-index", "--in-process-pc"),
      repo,
      15
    )
    assert(qRc == 0, s"cached queryable boot (--aot-train --require-index) failed:\n$qLog")

    // the doctor, in a JVM that loaded the cache, reports it loaded
    val (dRc, dLog) = runProcess(
      Vector(javaBin) ++ runFlags ++
        Vector(s"-XX:AOTCache=$cache", "-jar", jar.toString, "--doctor", ws.toString),
      repo,
      5
    )
    assert(dRc == 0, s"--doctor with -XX:AOTCache failed:\n$dLog")
    assert(dLog.contains("AOT cache: loaded"), s"doctor did not report a loaded cache:\n$dLog")
