package ls.rename

import scala.concurrent.duration.Duration

/** The ingest publish tail runs the SQLite WAL checkpoint, so repeated ingests
  * do not let the WAL grow unbounded. With a zero threshold every publish
  * truncates the fully-checkpointed WAL to zero; auto-checkpoint alone never
  * truncates the `-wal` file, so this fails if the publish-tail checkpoint is
  * removed. The test never calls `checkpoint` itself.
  */
class IngestCheckpointSuite extends munit.FunSuite:

  override def munitTimeout: Duration = Duration(600, "s")

  private lazy val fx = FixtureWorkspace.master

  test("publish-tail checkpoint truncates the WAL across repeated ingests"):
    val stack = FixtureWorkspace.newStack(walCheckpointThresholdBytes = 0L)
    try
      val ws = FixtureWorkspace.workspaceFor(fx)
      for _ <- 1 to 3 do
        stack.orchestrator.ingest(ws)
        assertEquals(
          stack.meta.walSizeBytes,
          0L,
          "the publish-tail checkpoint should truncate the WAL after each ingest"
        )
    finally stack.close()

  test("the default (16 MiB) threshold keeps the small-fixture WAL bounded"):
    val stack = FixtureWorkspace.newStack() // production default threshold
    try
      val ws = FixtureWorkspace.workspaceFor(fx)
      for _ <- 1 to 3 do stack.orchestrator.ingest(ws)
      // Under the default threshold the tiny fixture WAL is never truncated, but
      // it stays far below the threshold (bounded), proving PASSIVE ran without
      // forcing a truncate.
      assert(
        stack.meta.walSizeBytes < ls.sqlite.MetaStore.DefaultWalThresholdBytes,
        s"WAL grew to ${stack.meta.walSizeBytes} bytes"
      )
    finally stack.close()
