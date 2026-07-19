package ls.pc

import java.net.URI
import java.nio.file.Path
import java.util.Optional

import scala.util.control.NonFatal

import org.eclipse.lsp4j.{Location, Range, SymbolKind}
import scala.meta.internal.metals.{ClasspathSearch, ExcludedPackagesHandler, WorkspaceSymbolQuery}
import scala.meta.pc.{ParentSymbols, SymbolDocumentation, SymbolSearch, SymbolSearchVisitor}

/** One workspace extension-method / implicit-class-member hit answered through
  * the [[PcDefinitionResolver.searchMethods]] seam.
  *
  * Deliberately lsp4j-light and ABI-encodable (strings + ints + a plain
  * range): `kind` is the raw lsp4j [[SymbolKind]] value, and
  * `semanticdbSymbol` is the SemanticDB string the PC resolves back to a
  * compiler symbol (`CompilerSearchVisitor.visitWorkspaceSymbol`).
  */
final case class WorkspaceMethodHit(
    fileUri: String,
    semanticdbSymbol: String,
    kind: Int,
    range: Range
)

/** Cross-file definition lookup for the presentation compiler.
  *
  * The dotty PC resolves the cursor to a compiler symbol; when that symbol is
  * NOT defined in the open buffer it asks its `scala.meta.pc.SymbolSearch`
  * for the definition location of the symbol's SemanticDB string. This trait
  * is the one seam the LS plugs there:
  *
  *   - in-process backend: an index-backed resolver queries the postings
  *     snapshot directly (same JVM);
  *   - forked backend: the child JVM's resolver RPCs back to the parent over
  *     the existing worker jsonrpc channel
  *     ([[PcWorkerClient.symbolDefinition]]), and the parent answers from its
  *     index-backed resolver.
  *
  * `ls-pc` stays index-free (plan 4.3): only this abstraction lives here; the
  * index-backed implementation lives in the Rust host (the `symbol_definition`
  * vtable slot backed by the immutable snapshot).
  */
trait PcDefinitionResolver:
  /** Definition locations of a SemanticDB symbol (e.g. `pkga/Core#ping().`),
    * as `file://` [[Location]]s. `fromFileUri` is the uri of the buffer the
    * request originated from. Must never throw; unknown symbols answer empty.
    */
  def definition(semanticdbSymbol: String, fromFileUri: String): Vector[Location]

  /** Workspace extension methods / implicit-class members whose display name
    * matches `query` (member-mode completion: the PC asks through
    * `SymbolSearch.searchMethods` for out-of-scope candidates and filters them
    * by receiver type itself). Must never throw; the default answers empty so
    * index-less embedders keep the previous no-hit behavior.
    */
  def searchMethods(query: String, buildTargetIdentifier: String): Vector[WorkspaceMethodHit] =
    Vector.empty

object PcDefinitionResolver:
  /** Default no-op resolver: cross-file definition stays empty, exactly the
    * behavior of the PC's built-in `EmptySymbolSearch`.
    */
  object Empty extends PcDefinitionResolver:
    def definition(semanticdbSymbol: String, fromFileUri: String): Vector[Location] =
      Vector.empty

/** [[SymbolSearch]] adapter over a [[PcDefinitionResolver]] plus the target's
  * classpath:
  *
  *   - `definition` routes to the resolver (cross-file go-to);
  *   - `search` (scope-mode completion of unimported classes/objects) runs the
  *     PC-vendored `scala.meta.internal.metals.ClasspathSearch` over
  *     `classpath` — a pure island-local lookup, no index involved;
  *   - `searchMethods` (member-mode workspace extension-method discovery)
  *     forwards the resolver's [[WorkspaceMethodHit]]s to the visitor;
  *   - documentation stays empty like the PC's `EmptySymbolSearch`.
  *
  * The classpath index is built lazily on first `search` and is expensive:
  * construct one instance per distinct classpath and reuse it
  * ([[PcWorkerManager]] caches instances across PC re-creations). Never
  * throws; every failure degrades to the empty/COMPLETE answer.
  */
final class IndexBackedSymbolSearch(
    resolver: PcDefinitionResolver,
    classpath: Vector[Path] = Vector.empty
) extends SymbolSearch:

  private lazy val classpathSearch: ClasspathSearch =
    try
      if classpath.isEmpty then ClasspathSearch.empty
      else ClasspathSearch.fromClasspath(classpath, ExcludedPackagesHandler.default)
    catch case NonFatal(_) => ClasspathSearch.empty

  override def definition(symbol: String, sourceUri: URI): java.util.List[Location] =
    try
      val locs = resolver.definition(
        if symbol == null then "" else symbol,
        if sourceUri == null then "" else sourceUri.toString
      )
      val out = new java.util.ArrayList[Location](locs.length)
      locs.foreach(out.add)
      out
    catch case NonFatal(_) => java.util.Collections.emptyList()

  override def definitionSourceToplevels(symbol: String, sourceUri: URI): java.util.List[String] =
    java.util.Collections.emptyList()

  override def documentation(symbol: String, parents: ParentSymbols): Optional[SymbolDocumentation] =
    Optional.empty()

  override def search(
      query: String,
      buildTargetIdentifier: String,
      visitor: SymbolSearchVisitor
  ): SymbolSearch.Result =
    try
      if query == null || query.isEmpty then SymbolSearch.Result.COMPLETE
      else classpathSearch.search(WorkspaceSymbolQuery.exact(query), visitor)._1
    catch case NonFatal(_) => SymbolSearch.Result.COMPLETE

  override def searchMethods(
      query: String,
      buildTargetIdentifier: String,
      visitor: SymbolSearchVisitor
  ): SymbolSearch.Result =
    try
      val hits = resolver.searchMethods(
        if query == null then "" else query,
        if buildTargetIdentifier == null then "" else buildTargetIdentifier
      )
      hits.foreach { hit =>
        // a malformed single hit (bad uri / kind value) must not lose the rest
        try
          visitor.visitWorkspaceSymbol(
            Path.of(URI(hit.fileUri)),
            hit.semanticdbSymbol,
            SymbolKind.forValue(hit.kind),
            hit.range
          )
        catch case NonFatal(_) => ()
      }
      SymbolSearch.Result.COMPLETE
    catch case NonFatal(_) => SymbolSearch.Result.COMPLETE
