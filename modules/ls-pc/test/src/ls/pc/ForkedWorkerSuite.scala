package ls.pc

import java.nio.file.Files
import java.util.concurrent.TimeUnit

import scala.concurrent.duration.*
import scala.jdk.CollectionConverters.*

/** Spawns a real PC worker JVM (`java -cp <inherited classpath> ls.pc.PcWorkerMain`)
  * and drives it over stdin/stdout JSON-RPC.
  *
  * Set LS_PC_SKIP_FORK_TEST=1 to skip in environments where forking a JVM from
  * the test JVM is not possible.
  */
class ForkedWorkerSuite extends munit.FunSuite:
  override def munitTimeout: Duration = 5.minutes

  private def assumeForkAllowed(): Unit =
    assume(!sys.env.contains("LS_PC_SKIP_FORK_TEST"), "LS_PC_SKIP_FORK_TEST set: skipping fork test")

  private def get[A](f: java.util.concurrent.CompletableFuture[A]): A =
    f.get(120, TimeUnit.SECONDS)

  test("forked worker: spawn, complete() round-trip, plugin status, supervision restart, clean shutdown"):
    assumeForkAllowed()
    val genDir = Files.createTempDirectory("ls-pc-forked-gen")
    val worker = new ForkedPcWorker(
      workerArgs = Vector("--generated-sources", genDir.toString, "--timeout-ms", "90000"),
      requestTimeoutMillis = 120000
    )
    try
      // init + open
      assertEquals(get(worker.initializeTarget(PcWorkerTargetParams.of(SharedPc.targetConfig))), "ok")
      val open = new PcWorkerDidOpenParams
      open.targetId = SharedPc.targetId
      open.uri = "file:///ls-pc-test/Forked.scala"
      open.text = "object Forked:\n  val xs = List(1)\n  val ys = xs.\n"
      assertEquals(get(worker.didOpen(open)), "ok")
      assert(worker.isAlive)

      // completion round-trip across the process boundary
      val pos = new PcWorkerPositionParams
      pos.uri = open.uri
      pos.line = 2
      pos.character = "  val ys = xs.".length
      val list = get(worker.completion(pos))
      val labels = list.getItems.asScala.map(_.getLabel)
      assert(labels.exists(_.startsWith("map")), s"missing map in: ${labels.take(20)}")

      // survives a plugin status call
      val status = get(worker.pluginStatus())
      assert(status.servicePlugins.isEmpty) // no plugins configured in the child
      assert(status.disabled.isEmpty)

      // supervision: kill the child; next request respawns it and replays
      // initializeTarget + didOpen, so the same completion still answers
      worker.restart()
      assert(!worker.isAlive)
      val relist = get(worker.completion(pos))
      val relabels = relist.getItems.asScala.map(_.getLabel)
      assert(relabels.exists(_.startsWith("map")), s"after respawn: ${relabels.take(20)}")
      assert(worker.isAlive)

      // clean shutdown: the child process exits
      assert(get(worker.shutdown()).startsWith("ok"))
      assert(!worker.isAlive)
    finally worker.close()
