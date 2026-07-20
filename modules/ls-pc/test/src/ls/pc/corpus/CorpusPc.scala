package ls.pc.corpus

import java.nio.file.{Files, Path}
import java.util.concurrent.atomic.AtomicInteger

import scala.collection.concurrent.TrieMap

import ls.pc.{
  PcDefinitionResolver,
  PcFacade,
  PcPluginInitContext,
  PcPluginManager,
  PcSettings,
  PcTargetConfig,
  SharedPc,
  WorkspaceMethodHit
}
import org.eclipse.lsp4j.{Location, Position, Range, SymbolKind}
import scala.meta.internal.metals.Fuzzy

/** Shared plugin-free presentation-compiler facade for the ported test corpus.
  *
  * The corpus suites (ported from the Scala 3 presentation-compiler test
  * suite, see [[CorpusSuiteBase]]) assert exact completion/hover/signature
  * output, so they must NOT share [[ls.pc.SharedPc]]'s facade: its
  * marker/augment test plugins append items and locations to every result.
  * This facade registers no plugins and wires two dotty-tests mocks into our
  * [[PcDefinitionResolver]] seam: the `MockEntries` cross-file definition map
  * (definition) and a `TestingWorkspaceSearch`-style per-buffer workspace
  * method registry (searchMethods).
  */
object CorpusPc:

  val targetId = "corpusTarget"

  /** A second target sharing the classpath but compiled with `-explain`, for
    * the ported ExplainDiagnosticProviderSuite (dotty's `options =
    * List("-explain")` override).
    */
  val explainTargetId = "corpusExplainTarget"

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

  /** One workspace method a corpus case declares for the searchMethods seam:
    * the semanticdb symbol is hand-derived from the case source exactly as
    * dotty's `TestingWorkspaceSearch` symbol collector would emit it (the
    * upstream harness indexes the test source itself), and the definition
    * range/uri are resolved against the case's own buffer at [[openBuffer]].
    */
  final case class WorkspaceMethod(
      displayName: String,
      semanticdbSymbol: String,
      kind: Int = SymbolKind.Method.getValue
  )

  /** Live registry backing [[MockResolver.searchMethods]], keyed by the buffer
    * uri that seeded it (seeded in [[openBuffer]], cleared in [[closeBuffer]]).
    */
  private val workspaceMethods =
    TrieMap.empty[String, Vector[(String, WorkspaceMethodHit)]]

  /** Live registry backing [[MockResolver.definitionSourceToplevels]] — the
    * port of dotty `MockEntries.definitionSourceTopLevels` (e.g.
    * `scala/Option#` -> [`scala/Some.`, `scala/None.`]): per seeding buffer
    * uri, the sealed parent's SemanticDB symbol -> the HAND-ORDERED toplevel
    * child list. The PC's exhaustive-match sorter
    * (`CaseKeywordCompletion.sortSubclasses`) consults the seam through
    * `SymbolSearch.definitionSourceToplevels` only when the parent's children
    * carry no compiler source positions, and must respect this order. Keyed
    * by uri because the sorter passes the COMPLETION buffer's uri as the
    * `sourceUri`, which scopes each case's seed to its own buffer.
    */
  private val mockToplevels =
    TrieMap.empty[String, Map[String, Vector[String]]]

  /** Every [[MockResolver.definitionSourceToplevels]] consultation in arrival
    * order (`(semanticdbSymbol, sourceUri)`), so a corpus case can assert the
    * seam WAS consulted (the mock-driven ordering cases) or was NOT (the
    * source-position-ordered shapes). Append-only across the shared facade;
    * assert by filtering on your own buffer uri.
    */
  private val toplevelsLog =
    new java.util.concurrent.ConcurrentLinkedQueue[(String, String)]

  def toplevelsQueries: Vector[(String, String)] =
    import scala.jdk.CollectionConverters.*
    toplevelsLog.iterator.asScala.toVector

  /** Range of `name` at its definition site (`def`/`val`/`var`/`class name`)
    * in `text`; the corpus registers real definition coordinates even though
    * the PC's `CompilerSearchVisitor` re-resolves hits purely by symbol.
    */
  private def definitionRange(text: String, name: String): Range =
    val matcher = java.util.regex.Pattern
      .compile("(?:def|val|var|class)\\s+" + java.util.regex.Pattern.quote(name) + "\\b")
      .matcher(text)
    if !matcher.find() then Range(Position(0, 0), Position(0, 0))
    else
      def toPos(offset: Int): Position =
        val before = text.substring(0, offset)
        val line = before.count(_ == '\n')
        Position(line, offset - (before.lastIndexOf('\n') + 1))
      Range(toPos(matcher.end() - name.length), toPos(matcher.end()))

  /** Cross-file lookups the PC delegates to `SymbolSearch.definition` reach
    * this resolver with the SemanticDB symbol string; answer from the mock
    * map exactly like dotty's `MockSymbolSearch.definition`. `searchMethods`
    * answers from the per-buffer registry with dotty's
    * `TestingWorkspaceSearch` matching semantics (the vendored
    * `Fuzzy.matches`: query is a — camel-hump aware — prefix of the display
    * name; an empty query matches everything).
    */
  object MockResolver extends PcDefinitionResolver:
    def definition(semanticdbSymbol: String, fromFileUri: String): Vector[Location] =
      // Return fresh copies: the definition renderer mutates nothing, but the
      // facade result is shared across concurrent suites.
      mockDefinitions.getOrElse(semanticdbSymbol, Vector.empty)

    override def searchMethods(
        query: String,
        buildTargetIdentifier: String
    ): Vector[WorkspaceMethodHit] =
      workspaceMethods.values.toVector.flatten.collect {
        case (name, hit) if Fuzzy.matches(query, name) => hit
      }

    /** The dotty `MockEntries.definitionSourceTopLevels` seam: answer the
      * hand-ordered child list the querying buffer seeded (empty otherwise —
      * the trait default the index-less embedders keep), recording every
      * consultation for the corpus ordering cases.
      */
    override def definitionSourceToplevels(
        semanticdbSymbol: String,
        sourceUri: String
    ): Vector[String] =
      toplevelsLog.add(semanticdbSymbol -> sourceUri)
      mockToplevels
        .getOrElse(sourceUri, Map.empty)
        .getOrElse(semanticdbSymbol, Vector.empty)

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
    f.registerTarget(
      PcTargetConfig(explainTargetId, SharedPc.libraryClasspath, Vector("-explain"))
    )
    f

  private val counter = new AtomicInteger(0)

  /** Open a fresh dirty buffer on the corpus facade; the returned uri ends in
    * `filename` so filename-sensitive PC behavior matches the dotty harness
    * (which always compiles `A.scala` / `Hover.scala`). `methods` seeds the
    * searchMethods registry with hits pointing into this very buffer,
    * mirroring dotty's harness, whose `TestingWorkspaceSearch` indexes the
    * case source itself; close with [[closeBuffer]] to unregister them.
    */
  def openBuffer(
      text: String,
      filename: String = "A.scala",
      methods: Seq[WorkspaceMethod] = Nil,
      toplevels: Map[String, Vector[String]] = Map.empty
  ): String =
    openBufferFor(targetId, text, filename, methods, toplevels)

  /** [[openBuffer]] under an explicit registered target (the `-explain`
    * diagnostics corpus opens under [[explainTargetId]]). `toplevels` seeds
    * the definitionSourceToplevels seam for this buffer (the dotty
    * `MockEntries.definitionSourceTopLevels` map: sealed parent symbol ->
    * hand-ordered toplevel children).
    */
  def openBufferFor(
      target: String,
      text: String,
      filename: String = "A.scala",
      methods: Seq[WorkspaceMethod] = Nil,
      toplevels: Map[String, Vector[String]] = Map.empty
  ): String =
    val uri = s"file:///ls-pc-corpus/${counter.incrementAndGet()}/$filename"
    if methods.nonEmpty then
      workspaceMethods.put(
        uri,
        methods.iterator.map { m =>
          m.displayName -> WorkspaceMethodHit(
            uri,
            m.semanticdbSymbol,
            m.kind,
            definitionRange(text, m.displayName)
          )
        }.toVector
      )
    if toplevels.nonEmpty then mockToplevels.put(uri, toplevels)
    facade.didOpen(target, uri, text)
    uri

  /** Close a corpus buffer and drop any workspace methods or mock toplevels
    * it registered.
    */
  def closeBuffer(uri: String): Unit =
    workspaceMethods.remove(uri)
    mockToplevels.remove(uri)
    facade.didClose(uri)
