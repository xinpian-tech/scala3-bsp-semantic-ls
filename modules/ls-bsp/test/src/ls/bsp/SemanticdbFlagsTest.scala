package ls.bsp

import java.nio.file.Path

class SemanticdbFlagsTest extends munit.FunSuite:

  private val ws = Path.of("/workspace")
  private val classDir = Path.of("/workspace/out/classes")

  test("no semanticdb flags: disabled, sourceroot defaults to workspace root") {
    val cfg = SemanticdbFlags.extract(Vector("-deprecation", "-feature"), classDir, ws)
    assertEquals(cfg.enabled, false)
    assertEquals(cfg.semanticdbRoot, None)
    assertEquals(cfg.sourceroot, ws)
  }

  test("-Xsemanticdb alone: targetroot is the class directory") {
    val cfg = SemanticdbFlags.extract(Vector("-Xsemanticdb"), classDir, ws)
    assertEquals(cfg.enabled, true)
    assertEquals(cfg.semanticdbRoot, Some(classDir))
    assertEquals(cfg.sourceroot, ws)
  }

  test("-Ysemanticdb alone also enables SemanticDB") {
    val cfg = SemanticdbFlags.extract(Vector("-Ysemanticdb"), classDir, ws)
    assertEquals(cfg.semanticdbRoot, Some(classDir))
  }

  test("colon form -semanticdb-target: overrides the targetroot") {
    val cfg = SemanticdbFlags.extract(
      Vector("-Xsemanticdb", "-semanticdb-target:/workspace/out/meta"),
      classDir,
      ws
    )
    assertEquals(cfg.semanticdbRoot, Some(Path.of("/workspace/out/meta")))
  }

  test("two-token form -semanticdb-target <path> overrides the targetroot") {
    val cfg = SemanticdbFlags.extract(
      Vector("-Xsemanticdb", "-semanticdb-target", "/workspace/out/meta2"),
      classDir,
      ws
    )
    assertEquals(cfg.semanticdbRoot, Some(Path.of("/workspace/out/meta2")))
  }

  test("-semanticdb-target without -Xsemanticdb does not enable SemanticDB") {
    val cfg = SemanticdbFlags.extract(Vector("-semanticdb-target:/x"), classDir, ws)
    assertEquals(cfg.enabled, false)
    assertEquals(cfg.semanticdbRoot, None)
  }

  test("colon and two-token -sourceroot forms are both parsed") {
    val colon =
      SemanticdbFlags.extract(Vector("-Xsemanticdb", "-sourceroot:/repo/src"), classDir, ws)
    assertEquals(colon.sourceroot, Path.of("/repo/src"))
    val twoToken =
      SemanticdbFlags.extract(Vector("-Xsemanticdb", "-sourceroot", "/repo/src2"), classDir, ws)
    assertEquals(twoToken.sourceroot, Path.of("/repo/src2"))
  }

  test("last occurrence wins across both spellings") {
    val cfg = SemanticdbFlags.extract(
      Vector(
        "-Xsemanticdb",
        "-semanticdb-target:/first",
        "-semanticdb-target",
        "/second",
        "-sourceroot",
        "/src-first",
        "-sourceroot:/src-second"
      ),
      classDir,
      ws
    )
    assertEquals(cfg.semanticdbRoot, Some(Path.of("/second")))
    assertEquals(cfg.sourceroot, Path.of("/src-second"))
  }

  test("relative paths resolve against the workspace root and normalize") {
    val cfg = SemanticdbFlags.extract(
      Vector("-Xsemanticdb", "-semanticdb-target:out/../meta", "-sourceroot:sub/dir"),
      classDir,
      ws
    )
    assertEquals(cfg.semanticdbRoot, Some(Path.of("/workspace/meta")))
    assertEquals(cfg.sourceroot, Path.of("/workspace/sub/dir"))
  }

  test("trailing two-token flag without a value is ignored") {
    val cfg = SemanticdbFlags.extract(Vector("-Xsemanticdb", "-semanticdb-target"), classDir, ws)
    assertEquals(cfg.semanticdbRoot, Some(classDir))
  }

  test("empty colon-form value is ignored") {
    val cfg = SemanticdbFlags.extract(Vector("-Xsemanticdb", "-semanticdb-target:"), classDir, ws)
    assertEquals(cfg.semanticdbRoot, Some(classDir))
  }
