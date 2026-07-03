package ls.rename

import java.nio.file.{Files, Path}

import scala.concurrent.duration.Duration
import scala.jdk.CollectionConverters.*

/** The ingest publish tail reclaims drained superseded segment directories, so
  * a long-running server does not leak one segment directory per re-ingest.
  */
class IngestJanitorSuite extends munit.FunSuite:

  override def munitTimeout: Duration = Duration(600, "s")

  private lazy val fx = FixtureWorkspace.master

  private def segmentDirs(stack: FixtureWorkspace.Stack): Vector[Path] =
    val dir = stack.manager.segmentsDir
    if !Files.isDirectory(dir) then Vector.empty
    else
      val stream = Files.list(dir)
      try stream.iterator().asScala.filter(_.getFileName.toString.startsWith("segment-")).toVector
      finally stream.close()

  test("publish prunes drained superseded segments: exactly one segment dir after re-ingests"):
    val stack = FixtureWorkspace.newStack()
    try
      val ws = FixtureWorkspace.workspaceFor(fx)
      stack.orchestrator.ingest(ws)
      stack.orchestrator.ingest(ws)
      stack.orchestrator.ingest(ws)
      assertEquals(segmentDirs(stack).length, 1, segmentDirs(stack).map(_.getFileName.toString).toString)
    finally stack.close()

  test("held snapshots delay pruning until release"):
    val stack = FixtureWorkspace.newStack()
    try
      val ws = FixtureWorkspace.workspaceFor(fx)
      stack.orchestrator.ingest(ws)
      val held = stack.manager.current().get // retain the first snapshot across a re-ingest
      stack.orchestrator.ingest(ws)
      assertEquals(segmentDirs(stack).length, 2, "held snapshot's segment dir must survive the publish")
      held.release()
      stack.manager.deleteSuperseded()
      assertEquals(segmentDirs(stack).length, 1, "drained dir is reclaimed after release")
    finally stack.close()
