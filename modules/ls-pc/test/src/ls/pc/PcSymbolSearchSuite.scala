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

  // --- searchMethods / search seam (workspace + classpath symbol search) -----

  private final class RecordingVisitor extends scala.meta.pc.SymbolSearchVisitor:
    val workspaceSymbols =
      scala.collection.mutable.ArrayBuffer.empty[(java.nio.file.Path, String, org.eclipse.lsp4j.SymbolKind, Range)]
    val classfiles = scala.collection.mutable.ArrayBuffer.empty[(String, String)]
    def shouldVisitPackage(pkg: String): Boolean = true
    def visitClassfile(pkg: String, filename: String): Int =
      classfiles += ((pkg, filename)); 1
    def visitWorkspaceSymbol(
        path: java.nio.file.Path,
        symbol: String,
        kind: org.eclipse.lsp4j.SymbolKind,
        range: Range
    ): Int =
      workspaceSymbols += ((path, symbol, kind, range)); 1
    def isCancelled: Boolean = false

  test("searchMethods forwards resolver hits to the visitor and reports COMPLETE"):
    val range = new Range(new Position(1, 6), new Position(1, 10))
    val hit = WorkspaceMethodHit(
      "file:///ls-pc-test/Ext.scala",
      "example/enrichments.incr().",
      org.eclipse.lsp4j.SymbolKind.Method.getValue,
      range
    )
    val seen = new AtomicReference[(String, String)]()
    val resolver = new PcDefinitionResolver:
      def definition(semanticdbSymbol: String, fromFileUri: String): Vector[Location] =
        Vector.empty
      override def searchMethods(query: String, buildTargetIdentifier: String): Vector[WorkspaceMethodHit] =
        seen.set((query, buildTargetIdentifier))
        Vector(hit)
    val visitor = new RecordingVisitor
    val result = new IndexBackedSymbolSearch(resolver).searchMethods("incr", "aTarget", visitor)
    assertEquals(result, scala.meta.pc.SymbolSearch.Result.COMPLETE)
    assertEquals(seen.get(), ("incr", "aTarget"))
    assertEquals(visitor.workspaceSymbols.size, 1)
    val (path, symbol, kind, visitedRange) = visitor.workspaceSymbols.head
    assert(path.endsWith("Ext.scala"), s"unexpected path: $path")
    assertEquals(symbol, "example/enrichments.incr().")
    assertEquals(kind, org.eclipse.lsp4j.SymbolKind.Method)
    assertEquals(visitedRange, range)

  test("a throwing searchMethods resolver still reports COMPLETE"):
    val resolver = new PcDefinitionResolver:
      def definition(semanticdbSymbol: String, fromFileUri: String): Vector[Location] =
        Vector.empty
      override def searchMethods(query: String, buildTargetIdentifier: String): Vector[WorkspaceMethodHit] =
        throw new RuntimeException("boom: intentional resolver crash")
    val result =
      new IndexBackedSymbolSearch(resolver).searchMethods("x", "t", new RecordingVisitor)
    assertEquals(result, scala.meta.pc.SymbolSearch.Result.COMPLETE)

  test("classpath search visits scala-library classfiles for a scope query"):
    val search = new IndexBackedSymbolSearch(PcDefinitionResolver.Empty, SharedPc.libraryClasspath)
    val visitor = new RecordingVisitor
    search.search("ListBuffe", "aTarget", visitor)
    assert(
      visitor.classfiles.exists { case (pkg, file) =>
        pkg.contains("scala/collection/mutable") && file.startsWith("ListBuffer")
      },
      s"scala.collection.mutable.ListBuffer not visited: ${visitor.classfiles}"
    )

  test("the default resolver searchMethods answers empty (previous no-op behavior)"):
    val visitor = new RecordingVisitor
    val result =
      new IndexBackedSymbolSearch(PcDefinitionResolver.Empty).searchMethods("any", "t", visitor)
    assertEquals(result, scala.meta.pc.SymbolSearch.Result.COMPLETE)
    assert(visitor.workspaceSymbols.isEmpty)
