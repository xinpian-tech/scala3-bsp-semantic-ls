package ls.pc

import java.net.URI
import java.util.Optional

import scala.util.control.NonFatal

import org.eclipse.lsp4j.Location
import scala.meta.pc.{ParentSymbols, SymbolDocumentation, SymbolSearch, SymbolSearchVisitor}

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
  * index-backed implementation lives in `ls-core`.
  */
trait PcDefinitionResolver:
  /** Definition locations of a SemanticDB symbol (e.g. `pkga/Core#ping().`),
    * as `file://` [[Location]]s. `fromFileUri` is the uri of the buffer the
    * request originated from. Must never throw; unknown symbols answer empty.
    */
  def definition(semanticdbSymbol: String, fromFileUri: String): Vector[Location]

object PcDefinitionResolver:
  /** Default no-op resolver: cross-file definition stays empty, exactly the
    * behavior of the PC's built-in `EmptySymbolSearch`.
    */
  object Empty extends PcDefinitionResolver:
    def definition(semanticdbSymbol: String, fromFileUri: String): Vector[Location] =
      Vector.empty

/** [[SymbolSearch]] adapter over a [[PcDefinitionResolver]]: implements ONLY
  * `definition` and no-ops the rest exactly like the PC's default
  * `EmptySymbolSearch` (empty list / COMPLETE / empty Optional), so wiring it
  * changes nothing but cross-file go-to.
  */
final class IndexBackedSymbolSearch(resolver: PcDefinitionResolver) extends SymbolSearch:

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
  ): SymbolSearch.Result = SymbolSearch.Result.COMPLETE

  override def searchMethods(
      query: String,
      buildTargetIdentifier: String,
      visitor: SymbolSearchVisitor
  ): SymbolSearch.Result = SymbolSearch.Result.COMPLETE
