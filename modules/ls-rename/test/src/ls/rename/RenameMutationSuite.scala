package ls.rename

import java.nio.charset.StandardCharsets
import java.nio.file.Files
import scala.concurrent.duration.Duration

import ls.index.*

/** Rename rejections that require mutating a cloned fixture: stale md5 and
  * shared-source disagreement.
  */
class RenameMutationSuite extends munit.FunSuite:

  override def munitTimeout: Duration = Duration(600, "s")

  test("stale md5: source edited after compile is rejected before emitting edits"):
    val fx = FixtureWorkspace.cloneFixture()
    val stack = FixtureWorkspace.newStack()
    try
      stack.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      // mutate a file that will receive edits, without recompiling
      val beta = fx.sourcePath("a/src/pkga/Beta.scala")
      Files.write(
        beta,
        (fx.sourceText("a/src/pkga/Beta.scala") + "\n// edited after compile\n")
          .getBytes(StandardCharsets.UTF_8)
      )
      val engine = RenameEngine(stack.orchestrator, StubCompiler())
      val (line, ch) = fx.cursor("a/src/pkga/Alpha.scala", "Alpha", 0)
      val err = intercept[LsException](
        engine.rename("a/src/pkga/Alpha.scala", line, ch, "Omega")
      )
      assert(err.error.isInstanceOf[LsError.StaleIndex], err.error.toString)
      err.error match
        case LsError.StaleIndex(uri) => assertEquals(uri, "a/src/pkga/Beta.scala")
        case _ => ()
    finally stack.close()

  test("shared-source disagreement between targets is rejected"):
    val fx = FixtureWorkspace.cloneFixture()
    val stack = FixtureWorkspace.newStack()
    try
      stack.orchestrator.ingest(FixtureWorkspace.workspaceFor(fx))
      // target B loses its view of the shared source: the two targets can no
      // longer be proven to agree on the rename group at the edit spans.
      val sharedSdbB = fx.outB
        .resolve("META-INF")
        .resolve("semanticdb")
        .resolve("shared/src/shared/Shared.scala.semanticdb")
      assert(Files.isRegularFile(sharedSdbB), sharedSdbB.toString)
      Files.delete(sharedSdbB)
      val engine = RenameEngine(stack.orchestrator, StubCompiler())
      val (line, ch) = fx.cursor("shared/src/shared/Shared.scala", "tag", 0)
      val err = intercept[LsException](
        engine.rename("shared/src/shared/Shared.scala", line, ch, "label")
      )
      err.error match
        case LsError.RenameRejected(reasons) =>
          assert(reasons.exists(_.contains("disagree")), reasons.toString)
        case other => fail(s"expected RenameRejected, got $other")
    finally stack.close()
