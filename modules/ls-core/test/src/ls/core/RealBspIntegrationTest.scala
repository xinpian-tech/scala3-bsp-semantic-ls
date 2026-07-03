package ls.core

import java.util.concurrent.TimeUnit

import scala.concurrent.duration.{Duration, DurationInt}
import scala.jdk.CollectionConverters.*

import org.eclipse.lsp4j.*

/** Plan 20 Phase 2 acceptance against a REAL BSP server: Mill itself.
  *
  * Gated by `LS_REAL_BSP_IT=1` (run `scripts/it-real-bsp.sh` inside
  * `nix develop`); skipped otherwise so the ordinary test run stays hermetic.
  *
  * The heavy boot (copy `it/sample-workspace`, real `mill mill.bsp.BSP/install`
  * + `mill __.compile`, launch `mill --bsp`, boot [[ScalaLs]] over the
  * production [[BspDiscovery]]/[[BspSession.launch]] path) lives in the shared
  * [[RealBspFixture]] so this suite and [[RealBspCoreTest]] share one server.
  *
  * Real mill-bsp behaviors this test encodes (found empirically, they differ
  * from the in-process fake server):
  *
  *   - Mill 1.1.2 BSP mode evaluates into a SEPARATE output directory
  *     `.bsp/out` (not `out/`), so the CLI pre-compile does NOT produce the
  *     SemanticDB the BSP-reported targetroots point at. The index only fills
  *     after a compile REQUESTED OVER BSP — the fixture drives
  *     `scala3SemanticLs.compile` + `scala3SemanticLs.reindex` first, exactly
  *     what a real editor session goes through.
  *   - Mill exposes its own build definition as a Scala 3 target
  *     (`.../mill-build`, Scala 3.8.1) without `-Xsemanticdb`, and the sample
  *     adds module `c` without `-Xsemanticdb` too: SemanticDB is mandatory, so
  *     BOTH are flagged as a hard SemanticDB-coverage ERROR (see
  *     [[RealBspCoreTest]] for the per-request hard errors on such sources).
  *   - Mill also advertises a `mill-synthetic-root-target` with no `scala`
  *     languageId; the project model must filter it (and does).
  */
class RealBspIntegrationTest extends munit.FunSuite:

  import RealBspFixture.{
    consumerUri,
    docsService,
    enabled,
    executeCommand,
    greetingUri,
    indexedUris,
    locationOf,
    position,
    textDoc,
    ws,
    wsService
  }

  override def munitTimeout: Duration = 900.seconds

  // ------------------------------------------------------------------ tests

  test("doctor reports the real mill BSP server and its Scala 3 targets"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")
    val _ = RealBspFixture.initResult
    val report = executeCommand(ScalaLs.Commands.Doctor)
    assert(report.contains("state: ready"), report)
    // (a) the real server identity from build/initialize
    assert(report.contains("server: mill-bsp"), report)
    // (a) >= 2 Scala 3 targets (a, b, c; mill also exposes its own mill-build
    // meta-target as Scala 3)
    val scala3Count =
      "Scala 3 targets: (\\d+)".r.findFirstMatchIn(report).map(_.group(1).toInt)
    assert(scala3Count.exists(_ >= 2), s"expected >=2 Scala 3 targets in:\n$report")
    val rootUri = Uris.toUri(ws.root).stripSuffix("/")
    for name <- Vector("a", "b") do
      assert(report.contains(s"$rootUri/$name"), s"target $name missing in:\n$report")
    // (a) SemanticDB is mandatory. mill-bsp REALLY advertises the build
    // definition itself (mill-build, Scala 3.8.1, no -Xsemanticdb), and the
    // sample's module `c` is built without SemanticDB, so BOTH are flagged as a
    // hard SemanticDB-coverage ERROR (not a tolerated steady state). The sample's
    // indexable modules a and b must not appear.
    val coverage =
      "SemanticDB coverage: ([^\\n]*)".r.findFirstMatchIn(report).map(_.group(1).trim)
    assert(coverage.exists(_.startsWith("ERROR")), s"expected a SemanticDB error in:\n$report")
    assert(
      coverage.exists(c => c.contains(s"$rootUri/c") && c.contains(s"$rootUri/mill-build")),
      s"expected both module c and mill-build flagged without SemanticDB in:\n$report"
    )
    assert(!coverage.exists(_.contains(s"$rootUri/a")), s"module a must be indexable:\n$report")

  test("compile over the real BSP session fills the index (separate .bsp/out)"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")
    val summary = RealBspFixture.readyIndex
    assert(summary.startsWith("ingest: segment"), summary)

  test("workspace/symbol finds the class defined in module a"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")
    val _ = RealBspFixture.readyIndex
    val result = wsService.symbol(new WorkspaceSymbolParams("Greeting")).get(60, TimeUnit.SECONDS)
    assert(result.isRight, "expected WorkspaceSymbol list")
    val symbols = result.getRight.asScala.toVector
    val greeting = symbols.filter(_.getName == "Greeting")
    assert(greeting.nonEmpty, symbols.map(_.getName).toString)
    assert(
      greeting.exists(s => s.getLocation.getLeft.getUri == ws.fileUri(greetingUri)),
      greeting.map(_.getLocation).toString
    )

  test("references on a usage in b returns the exact cross-module, cross-file set"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")
    val _ = RealBspFixture.readyIndex
    val params = new ReferenceParams(
      textDoc(consumerUri),
      position(consumerUri, "message"),
      new ReferenceContext(true)
    )
    val locations = docsService.references(params).get(120, TimeUnit.SECONDS).asScala.toVector
    val expected = indexedUris.map(uri => locationOf(uri, ws.tokenSpan(uri, "message", 0)))
    assertEquals(
      locations.toSet,
      expected.toSet,
      s"expected exactly the 4 message occurrences, got $locations"
    )
    assertEquals(locations.length, 4, locations.toString)

  test("rename compiles through the real BSP server and edits both modules"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")
    val _ = RealBspFixture.readyIndex
    val params = new RenameParams(
      textDoc(consumerUri),
      position(consumerUri, "message"),
      "note"
    )
    val edit = docsService.rename(params).get(600, TimeUnit.SECONDS)
    val changes = edit.getChanges
    assertEquals(changes.keySet.asScala.toSet, indexedUris.map(ws.fileUri).toSet)
    for uri <- indexedUris do
      val edits = changes.get(ws.fileUri(uri)).asScala.toVector
      assertEquals(edits.length, 1, s"$uri: $edits")
      assertEquals(edits.head.getNewText, "note", s"$uri: $edits")
      assertEquals(
        LspConvert.span(edits.head.getRange),
        ws.tokenSpan(uri, "message", 0),
        s"$uri: $edits"
      )

  test("completion works on a dirty buffer against the real classpath"):
    assume(enabled, "set LS_REAL_BSP_IT=1 to run the real-BSP integration test")
    val _ = RealBspFixture.readyIndex
    val dirtyText = ws.sourceText(consumerUri) + "  val q = greeting.mess\n"
    docsService.didOpen(
      new DidOpenTextDocumentParams(
        new TextDocumentItem(ws.fileUri(consumerUri), "scala", 1, dirtyText)
      )
    )
    val line = dirtyText.linesIterator.length - 1
    val character = "  val q = greeting.mess".length
    val params = new CompletionParams(textDoc(consumerUri), new Position(line, character))
    val result = docsService.completion(params).get(180, TimeUnit.SECONDS)
    val items =
      if result.isRight then result.getRight.getItems.asScala.toVector
      else result.getLeft.asScala.toVector
    assert(items.exists(_.getLabel.startsWith("message")), items.map(_.getLabel).toString)
    docsService.didClose(new DidCloseTextDocumentParams(textDoc(consumerUri)))
