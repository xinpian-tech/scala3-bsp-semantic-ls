package ls.rename

import java.nio.charset.StandardCharsets
import java.nio.file.Files
import scala.concurrent.duration.Duration

import ls.index.*

/** RawSemanticDBPath: after editing a source and regenerating its SemanticDB
  * WITHOUT re-ingesting, symbol-at-cursor still answers from the raw
  * `.semanticdb` and flags needsReindex; a re-ingest then supersedes with an
  * epoch bump.
  */
class RawPathSuite extends munit.FunSuite:

  override def munitTimeout: Duration = Duration(600, "s")

  test("raw semanticdb path answers stale docs and flags needsReindex; reingest heals"):
    val fx = FixtureWorkspace.cloneFixture()
    val stack = FixtureWorkspace.newStack()
    try
      val report1 = stack.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      val uri = "a/src/pkga/Impl.scala"

      // fresh doc resolves via the snapshot
      val (line0, ch0) = fx.cursor(uri, "shout", 0)
      val before = stack.orchestrator.symbolAtCursor(uri, line0, ch0)
      assertEquals(before.source, ResolutionSource.Snapshot)
      assert(!before.needsReindex)

      // edit: insert a comment line at the top (shifts every occurrence
      // down one line), regenerate SemanticDB, do NOT re-ingest
      val edited = "// leading comment\n" + fx.sourceText(uri)
      Files.write(fx.sourcePath(uri), edited.getBytes(StandardCharsets.UTF_8))
      FixtureWorkspace.compileTree(
        fx.root,
        FixtureWorkspace.targetASources ++ FixtureWorkspace.sharedSources,
        fx.outA,
        Vector.empty
      )

      val cursor = stack.orchestrator.symbolAtCursor(uri, line0 + 1, ch0)
      assertEquals(cursor.source, ResolutionSource.RawSemanticdb)
      assert(cursor.needsReindex, "raw path must flag the doc for re-indexing")
      assertEquals(cursor.span, fx.tokenSpan(uri, "shout", 0))
      assert(cursor.semanticSymbol.contains("shout"), cursor.semanticSymbol)

      // references through the raw cursor still resolve via the snapshot
      // group and carry the needsReindex flag
      val engine = ReferencesEngine(stack.orchestrator)
      val refs = engine.references(uri, line0 + 1, ch0, includeDeclaration = false)
      assert(refs.needsReindex)
      assert(refs.locations.exists(_.uri == "b/src/pkgb/UseB.scala"), refs.locations.toString)

      // re-ingest: epoch bumps, snapshot swaps, cursor is index-fresh again
      val report2 = stack.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      assert(report2.segmentId > report1.segmentId)
      assertEquals(report2.docsStale, 0)
      val row = stack.meta.documentsByUri(uri).head
      assertEquals(row.epoch, 2L, "md5 change must bump the document epoch")
      val after = stack.orchestrator.symbolAtCursor(uri, line0 + 1, ch0)
      assertEquals(after.source, ResolutionSource.Snapshot)
      assert(!after.needsReindex)
    finally stack.close()

  test("raw path rejects when the regenerated semanticdb still mismatches the source"):
    val fx = FixtureWorkspace.cloneFixture()
    val stack = FixtureWorkspace.newStack()
    try
      stack.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      val uri = "a/src/pkga/Impl.scala"
      // edit the source but do NOT regenerate semanticdb: both the snapshot
      // and the raw document are stale -> StaleIndex, never a guess
      val edited = "// leading comment\n" + fx.sourceText(uri)
      Files.write(fx.sourcePath(uri), edited.getBytes(StandardCharsets.UTF_8))
      val (line0, ch0) = fx.cursor(uri, "shout", 0)
      val err = intercept[LsException](stack.orchestrator.symbolAtCursor(uri, line0 + 1, ch0))
      assert(err.error.isInstanceOf[LsError.StaleIndex], err.error.toString)
    finally stack.close()
