package ls.pc

import java.nio.file.Paths
import java.util.concurrent.TimeUnit

import scala.concurrent.duration.*
import scala.jdk.CollectionConverters.*

import org.eclipse.lsp4j.{Location, Position, Range}

/** [[InProcessPcWorker]] over the shared facade, plus pure round-trips of the
  * JSON-friendly carrier types.
  */
class WorkerProtocolSuite extends munit.FunSuite:
  override def munitTimeout: Duration = 5.minutes

  private lazy val worker = new InProcessPcWorker(SharedPc.facade)

  private def get[A](f: java.util.concurrent.CompletableFuture[A]): A =
    f.get(120, TimeUnit.SECONDS)

  test("PcWorkerTargetParams round-trips a PcTargetConfig"):
    val config = PcTargetConfig(
      bspId = "bsp://target#1",
      classpath = Vector(Paths.get("/a.jar"), Paths.get("/b.jar")),
      scalacOptions = Vector("-deprecation"),
      sourceDirs = Vector(Paths.get("/src")),
      scalaVersion = "3.8.4"
    )
    assertEquals(PcWorkerTargetParams.toConfig(PcWorkerTargetParams.of(config)), config)

  test("PcWorkerDefinitionResult round-trips origins"):
    val range = new Range(new Position(1, 2), new Position(1, 5))
    val result = DefinitionResult(
      "a/b.Foo#",
      Vector(
        DefinitionLocation(new Location("file:///A.scala", range), DefinitionOrigin.Workspace),
        DefinitionLocation(new Location("file:///Gen.scala", range), DefinitionOrigin.Synthetic),
        DefinitionLocation(new Location("file:///P.scala", range), DefinitionOrigin.Plugin)
      )
    )
    assertEquals(PcWorkerDefinitionResult.toDefinitionResult(PcWorkerDefinitionResult.of(result)), result)
    assert(result.hasSyntheticHits)
    assert(DefinitionResult.empty.isEmpty)
    assert(!DefinitionResult.empty.hasSyntheticHits)

  test("in-process worker serves initializeTarget/didOpen/completion/didChange/didClose"):
    assertEquals(get(worker.initializeTarget(PcWorkerTargetParams.of(SharedPc.targetConfig))), "ok")

    val open = new PcWorkerDidOpenParams
    open.targetId = SharedPc.targetId
    open.uri = "file:///ls-pc-test/WorkerBuffer.scala"
    open.text = "object Worker:\n  val xs = List(1)\n  val ys = xs.\n"
    assertEquals(get(worker.didOpen(open)), "ok")

    val pos = new PcWorkerPositionParams
    pos.uri = open.uri
    pos.line = 2
    pos.character = "  val ys = xs.".length
    val list = get(worker.completion(pos))
    assert(SharedPc.labels(list).exists(_.startsWith("map")))

    val change = new PcWorkerChangeParams
    change.uri = open.uri
    change.text = "object Worker:\n  val renamedVal = 1\n  val ys = renam\n"
    assertEquals(get(worker.didChange(change)), "ok")
    val pos2 = new PcWorkerPositionParams
    pos2.uri = open.uri
    pos2.line = 2
    pos2.character = "  val ys = renam".length
    assert(SharedPc.labels(get(worker.completion(pos2))).exists(_.startsWith("renamedVal")))

    val close = new PcWorkerUriParams
    close.uri = open.uri
    assertEquals(get(worker.didClose(close)), "ok")

  test("in-process worker serves hover/signatureHelp/definition/typeDefinition/prepareRename"):
    val open = new PcWorkerDidOpenParams
    open.targetId = SharedPc.targetId
    open.uri = "file:///ls-pc-test/WorkerQueries.scala"
    open.text =
      "object WQ:\n  class Box\n  def foo(x: Int): Int = x\n  val y = foo(1)\n  val b = new Box\n  def use = b\n  def m: Int =\n    val localVal = 1\n    localVal + 1\n"
    get(worker.didOpen(open))

    def at(line: Int, character: Int): PcWorkerPositionParams =
      val p = new PcWorkerPositionParams
      p.uri = open.uri
      p.line = line
      p.character = character
      p

    val hover = get(worker.hover(at(3, "  val y = fo".length)))
    assert(hover != null)

    val sig = get(worker.signatureHelp(at(3, "  val y = foo(1".length)))
    assert(!sig.getSignatures.isEmpty)

    val defn = get(worker.definition(at(3, "  val y = fo".length)))
    assert(defn.locations.asScala.exists(_.getUri == open.uri))
    assertEquals(defn.locations.size(), defn.origins.size())
    assert(defn.origins.asScala.contains(DefinitionOrigin.Workspace.toString))

    val typeDefn = get(worker.typeDefinition(at(5, "  def use = ".length)))
    assert(
      PcWorkerDefinitionResult
        .toDefinitionResult(typeDefn)
        .locations
        .exists(dl => dl.origin == DefinitionOrigin.Workspace && dl.location.getRange.getStart.getLine == 1)
    )

    // prepareRename targets a local value: the PC only offers rename ranges
    // for symbols it can safely rename locally
    val ren = get(worker.prepareRename(at(8, "    loc".length)))
    assert(ren != null)
    assertEquals(ren.getStart.getLine, 8)
    assertEquals(ren.getStart.getCharacter, "    ".length)

  test("in-process worker reports plugin status"):
    val status = get(worker.pluginStatus())
    val plugins = status.servicePlugins.asScala.mkString("\n")
    assert(plugins.contains("marker-completion"), plugins)
    assert(status.disabled.asScala.exists(_.startsWith("throwing-plugin")), status.disabled.asScala.toString)
