package ls.rename

import java.nio.charset.StandardCharsets
import java.nio.file.Files
import java.util.concurrent.Executors
import scala.concurrent.duration.Duration

import ls.index.*

/** RawSemanticDBPath behavior and the synchronous per-doc write-through: after
  * editing a source and regenerating its SemanticDB WITHOUT re-ingesting,
  * symbol-at-cursor answers from the raw `.semanticdb` and flags needsReindex;
  * with write-through on (the default) it also persists the refreshed document
  * so the next query is snapshot-fresh.
  */
class RawPathSuite extends munit.FunSuite:

  override def munitTimeout: Duration = Duration(600, "s")

  private def editAndRegenerate(fx: FixtureWorkspace.Fixture, uri: String): Unit =
    // insert a comment line at the top (shifts every occurrence down one
    // line), regenerate SemanticDB, do NOT re-ingest
    val edited = "// leading comment\n" + fx.sourceText(uri)
    Files.write(fx.sourcePath(uri), edited.getBytes(StandardCharsets.UTF_8))
    FixtureWorkspace.compileTree(
      fx.root,
      FixtureWorkspace.targetASources ++ FixtureWorkspace.sharedSources,
      fx.outA,
      Vector.empty
    )

  test("write-through off: raw semanticdb path answers stale docs and flags needsReindex; manual reingest heals"):
    val fx = FixtureWorkspace.cloneFixture()
    val stack = FixtureWorkspace.newStack(syncWriteThrough = false)
    try
      val report1 = stack.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      val uri = "a/src/pkga/Impl.scala"

      // fresh doc resolves via the snapshot
      val (line0, ch0) = fx.cursor(uri, "shout", 0)
      val before = stack.orchestrator.symbolAtCursor(uri, line0, ch0)
      assertEquals(before.source, ResolutionSource.Snapshot)
      assert(!before.needsReindex)

      editAndRegenerate(fx, uri)

      // write-through disabled: the raw path serves but does NOT heal
      val cursor = stack.orchestrator.symbolAtCursor(uri, line0 + 1, ch0)
      assertEquals(cursor.source, ResolutionSource.RawSemanticdb)
      assert(cursor.needsReindex, "raw path must flag the doc for re-indexing")
      assertEquals(cursor.span, fx.tokenSpan(uri, "shout", 0))
      assert(cursor.semanticSymbol.contains("shout"), cursor.semanticSymbol)
      assertEquals(stack.orchestrator.lastWriteThroughThreadName, None)

      // references through the raw cursor still resolve and carry needsReindex
      val engine = ReferencesEngine(stack.orchestrator)
      val refs = engine.references(uri, line0 + 1, ch0, includeDeclaration = false)
      assert(refs.needsReindex)
      assert(refs.locations.exists(_.uri == "b/src/pkgb/UseB.scala"), refs.locations.toString)

      // a manual re-ingest heals: epoch bumps, snapshot swaps, cursor fresh
      val report2 = stack.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      assert(report2.segmentId > report1.segmentId)
      assertEquals(report2.docsStale, 0)
      assertEquals(stack.meta.documentsByUri(uri).head.epoch, 2L)
      val after = stack.orchestrator.symbolAtCursor(uri, line0 + 1, ch0)
      assertEquals(after.source, ResolutionSource.Snapshot)
      assert(!after.needsReindex)
    finally stack.close()

  test("write-through updates the index: a raw-path query heals so the next query is snapshot-fresh"):
    val fx = FixtureWorkspace.cloneFixture()
    val stack = FixtureWorkspace.newStack() // write-through on (production default)
    try
      stack.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      val uri = "a/src/pkga/Impl.scala"
      val (line0, ch0) = fx.cursor(uri, "shout", 0)
      editAndRegenerate(fx, uri)

      // a raw-path REFERENCE query serves from raw and synchronously writes the
      // refreshed document through to the index; a SUCCESSFUL write-through then
      // clears needsReindex so no redundant reingest is scheduled
      val engine = ReferencesEngine(stack.orchestrator)
      val refs = engine.references(uri, line0 + 1, ch0, includeDeclaration = false)
      assert(!refs.needsReindex, "a successful write-through clears needsReindex (no redundant reingest)")
      assert(refs.locations.exists(_.uri == "b/src/pkgb/UseB.scala"), refs.locations.toString)

      // no manual re-ingest ran; the write-through already healed the index
      assertEquals(stack.meta.documentsByUri(uri).head.epoch, 2L, "write-through must bump the epoch")
      val after = stack.orchestrator.symbolAtCursor(uri, line0 + 1, ch0)
      assertEquals(after.source, ResolutionSource.Snapshot)
      assert(!after.needsReindex, "after write-through the next query must be snapshot-fresh")
    finally stack.close()

  test("write-through does not run when the regenerated semanticdb still mismatches the source"):
    val fx = FixtureWorkspace.cloneFixture()
    val stack = FixtureWorkspace.newStack() // write-through on
    try
      stack.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      val uri = "a/src/pkga/Impl.scala"
      val snapshotBefore = stack.manager.withCurrent(_.snapshotId)
      // edit the source but do NOT regenerate semanticdb: both the snapshot and
      // the raw document are stale -> StaleIndex, never a guess, never a write
      val edited = "// leading comment\n" + fx.sourceText(uri)
      Files.write(fx.sourcePath(uri), edited.getBytes(StandardCharsets.UTF_8))
      val (line0, ch0) = fx.cursor(uri, "shout", 0)
      val err = intercept[LsException](stack.orchestrator.symbolAtCursor(uri, line0 + 1, ch0))
      assert(err.error.isInstanceOf[LsError.StaleIndex], err.error.toString)
      // the index was not written through and is not corrupted: same snapshot
      assertEquals(stack.manager.withCurrent(_.snapshotId), snapshotBefore)
      assertEquals(stack.orchestrator.lastWriteThroughThreadName, None)
    finally stack.close()

  test("write-through runs inline on the calling (single index-executor) thread"):
    val fx = FixtureWorkspace.cloneFixture()
    val stack = FixtureWorkspace.newStack() // write-through on
    val exec = Executors.newSingleThreadExecutor { r =>
      val t = new Thread(r, "ac18-index-writer")
      t.setDaemon(true)
      t
    }
    try
      stack.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      val uri = "a/src/pkga/Impl.scala"
      val (line0, ch0) = fx.cursor(uri, "shout", 0)
      editAndRegenerate(fx, uri)
      // run the raw-path query on the single-thread executor; the write-through
      // must run inline on that same thread (never offloaded elsewhere)
      exec.submit[Unit](() => { stack.orchestrator.symbolAtCursor(uri, line0 + 1, ch0); () }).get()
      assertEquals(stack.orchestrator.lastWriteThroughThreadName, Some("ac18-index-writer"))
    finally
      exec.shutdownNow()
      stack.close()
