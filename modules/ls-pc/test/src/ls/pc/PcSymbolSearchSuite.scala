package ls.pc

import java.nio.file.Files
import java.util.concurrent.atomic.AtomicReference

import scala.concurrent.duration.*

import org.eclipse.lsp4j.{Location, Position, Range}

/** The [[PcDefinitionResolver]] seam: a definition on a CROSS-FILE symbol (one
  * resolved from the classpath, not the open buffer) reaches the resolver as a
  * SemanticDB symbol string, and the resolver's locations come back as the PC
  * definition result. Also pins the default: the Empty resolver answers
  * nothing, keeping the old `EmptySymbolSearch` behavior.
  */
class PcSymbolSearchSuite extends munit.FunSuite:

  override def munitTimeout: Duration = 5.minutes

  test("cross-file definition consults the resolver with the SemanticDB symbol and returns its locations"):
    val recorded = new AtomicReference[(String, String)]()
    val canned = new Location(
      "file:///resolved-by-index/Elsewhere.scala",
      new Range(new Position(3, 2), new Position(3, 7))
    )
    val resolver = new PcDefinitionResolver:
      def definition(semanticdbSymbol: String, fromFileUri: String): Vector[Location] =
        recorded.set((semanticdbSymbol, fromFileUri))
        Vector(canned)

    val genRoot = Files.createTempDirectory("ls-pc-search-gen")
    val pm = new PcPluginManager(PcPluginInitContext(None, genRoot))
    val facade = new PcFacade(pm, PcSettings(None, genRoot, 4, 90000L), resolver)
    try
      val targetId = "searchTarget"
      facade.registerTarget(PcTargetConfig(targetId, SharedPc.libraryClasspath, Vector.empty))
      val uri = "file:///ls-pc-test/SearchBuffer.scala"
      val text = "object SearchBuffer:\n  val xs = List(1, 2)\n"
      facade.didOpen(targetId, uri, text)

      // cursor on `List` — defined in the scala library, NOT in this buffer
      val line = 1
      val character = "  val xs = Li".length
      val result = facade.definition(uri, line, character)

      assert(
        result.locations.map(_.location).contains(canned),
        s"resolver location missing from PC definition result: ${result.locations}"
      )
      val rec = recorded.get()
      assert(rec != null, "the PC never consulted the resolver for a cross-file symbol")
      val (symbol, fromUri) = rec
      // exact-format contract: the PC passes the SemanticDB symbol string our
      // index stores verbatim (global symbols) — a `scala/` global for List
      assert(symbol.startsWith("scala/"), s"unexpected symbol format: '$symbol'")
      assert(symbol.contains("List"), s"unexpected symbol: '$symbol'")
      assertEquals(fromUri, uri)
    finally facade.shutdown()

  test("the Empty resolver keeps cross-file definition empty (EmptySymbolSearch behavior)"):
    val search = new IndexBackedSymbolSearch(PcDefinitionResolver.Empty)
    assert(
      search.definition("scala/package.List.", java.net.URI.create("file:///x.scala")).isEmpty,
      "Empty resolver must answer no locations"
    )
    assert(search.definitionSourceToplevels("scala/package.List.", java.net.URI.create("file:///x.scala")).isEmpty)

  test("a throwing resolver never fails the definition request"):
    val resolver = new PcDefinitionResolver:
      def definition(semanticdbSymbol: String, fromFileUri: String): Vector[Location] =
        throw new RuntimeException("boom: intentional resolver crash")
    val search = new IndexBackedSymbolSearch(resolver)
    assertEquals(
      search.definition("scala/package.List.", java.net.URI.create("file:///x.scala")).size(),
      0
    )
