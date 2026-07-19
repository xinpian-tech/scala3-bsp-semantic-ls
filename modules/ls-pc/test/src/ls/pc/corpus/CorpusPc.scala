package ls.pc.corpus

import java.nio.file.{Files, Path}
import java.util.concurrent.atomic.AtomicInteger

import ls.pc.{
  PcDefinitionResolver,
  PcFacade,
  PcPluginInitContext,
  PcPluginManager,
  PcSettings,
  PcTargetConfig,
  SharedPc
}
import org.eclipse.lsp4j.{Location, Position, Range}

/** Shared plugin-free presentation-compiler facade for the ported test corpus.
  *
  * The corpus suites (ported from the Scala 3 presentation-compiler test
  * suite, see [[CorpusSuiteBase]]) assert exact completion/hover/signature
  * output, so they must NOT share [[ls.pc.SharedPc]]'s facade: its
  * marker/augment test plugins append items and locations to every result.
  * This facade registers no plugins and wires the dotty-tests `MockEntries`
  * cross-file definition map into our [[PcDefinitionResolver]] seam.
  */
object CorpusPc:

  val targetId = "corpusTarget"

  lazy val generatedSourcesRoot: Path =
    Files.createTempDirectory("ls-pc-corpus-gen")

  /** Port of dotty's `MockEntries.MockLocation(symbol, path)`: the location
    * "uri" is the string `"<semanticdb-symbol> <filename>"` at 0:0, which the
    * definition corpus renders as an inline expected comment.
    */
  private def mockLocation(symbol: String, path: String): (String, Vector[Location]) =
    symbol -> Vector(
      new Location(s"$symbol $path", new Range(new Position(0, 0), new Position(0, 0)))
    )

  /** Union of the `mockEntries.definitions` maps of dotty's
    * `PcDefinitionSuite` and `TypeDefinitionSuite` (the two overlap only on
    * identical entries).
    */
  val mockDefinitions: Map[String, Vector[Location]] = Map(
    // PcDefinitionSuite
    mockLocation("scala/Int#", "Int.scala"),
    mockLocation("scala/concurrent/Future#", "Future.scala"),
    mockLocation("scala/concurrent/Future.", "Future.scala"),
    mockLocation("scala/Option#withFilter().", "Option.scala"),
    mockLocation("scala/Option#flatMap().", "Option.scala"),
    mockLocation("scala/Option#map().", "Option.scala"),
    mockLocation("scala/Option#get().", "Option.scala"),
    mockLocation("scala/Predef.assert().", "Predef.scala"),
    mockLocation("scala/Predef.assert(+1).", "Predef.scala"),
    mockLocation("scala/Predef.Ensuring#ensuring().", "Predef.scala"),
    mockLocation("scala/Predef.Ensuring#ensuring(+1).", "Predef.scala"),
    mockLocation("scala/Predef.Ensuring#ensuring(+2).", "Predef.scala"),
    mockLocation("scala/Predef.Ensuring#ensuring(+3).", "Predef.scala"),
    mockLocation("scala/collection/immutable/List#`::`().", "List.scala"),
    mockLocation("scala/package.List.", "package.scala"),
    mockLocation("scala/collection/immutable/Vector.", "Vector.scala"),
    // TypeDefinitionSuite
    mockLocation("scala/Option#", "Option.scala"),
    mockLocation("scala/Unit#", "Unit.scala"),
    mockLocation("scala/List#", "Unit.scala"),
    mockLocation("scala/Boolean#", "Boolean.scala"),
    mockLocation("scala/collection/WithFilter#", "WithFilter.scala"),
    mockLocation("scala/Option#WithFilter#", "Option.scala"),
    mockLocation("scala/collection/immutable/List#", "List.scala"),
    mockLocation("scala/Predef.String#", "Predef.scala"),
    mockLocation("java/lang/String#", "String.java"),
  )

  /** Cross-file lookups the PC delegates to `SymbolSearch.definition` reach
    * this resolver with the SemanticDB symbol string; answer from the mock
    * map exactly like dotty's `MockSymbolSearch.definition`.
    */
  object MockResolver extends PcDefinitionResolver:
    def definition(semanticdbSymbol: String, fromFileUri: String): Vector[Location] =
      // Return fresh copies: the definition renderer mutates nothing, but the
      // facade result is shared across concurrent suites.
      mockDefinitions.getOrElse(semanticdbSymbol, Vector.empty)

  lazy val facade: PcFacade =
    val pm = new PcPluginManager(PcPluginInitContext(None, generatedSourcesRoot))
    val f = new PcFacade(
      pm,
      PcSettings(
        workspaceRoot = None,
        generatedSourcesRoot = generatedSourcesRoot,
        maxLiveInstances = 4,
        requestTimeoutMillis = 90000L
      ),
      MockResolver
    )
    f.registerTarget(PcTargetConfig(targetId, SharedPc.libraryClasspath, Vector.empty))
    f

  private val counter = new AtomicInteger(0)

  /** Open a fresh dirty buffer on the corpus facade; the returned uri ends in
    * `filename` so filename-sensitive PC behavior matches the dotty harness
    * (which always compiles `A.scala` / `Hover.scala`).
    */
  def openBuffer(text: String, filename: String = "A.scala"): String =
    val uri = s"file:///ls-pc-corpus/${counter.incrementAndGet()}/$filename"
    facade.didOpen(targetId, uri, text)
    uri
