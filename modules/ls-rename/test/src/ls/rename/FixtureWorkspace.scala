package ls.rename

import java.io.File
import java.nio.charset.StandardCharsets
import java.nio.file.{Files, Path, StandardCopyOption}

import scala.jdk.CollectionConverters.*

import ls.index.Span
import ls.postings.SnapshotManager
import ls.rename.ingest.{IngestPipeline, TargetSpec, WorkspaceTargets}
import ls.semanticdb.DocFacts
import ls.sqlite.MetaStore

/** Real-compiler fixture: three target trees compiled in-process with
  * `dotty.tools.dotc.Main` and `-Xsemanticdb`, all sharing one sourceroot.
  *
  *   - target A (`fixture-a`): the definition tree (classes, traits, enums,
  *     case classes, vars, locals, overloads, extensions, givens, an
  *     override family, generated/readonly/dependency-marked docs) plus a
  *     shared source;
  *   - target B (`fixture-b`): depends on A (classpath includes A's classes,
  *     edge B -> A) and references A's symbols; also compiles the shared
  *     source;
  *   - target C (`fixture-c`): disconnected, deliberately reuses A's
  *     package/class names (`pkga.Core`) to prove target pruning.
  *
  * The master fixture is compiled once per JVM; suites that mutate sources
  * or stores call [[cloneFixture]] to get an isolated deep copy.
  */
object FixtureWorkspace:

  final case class Fixture(root: Path):
    def outA: Path = root.resolve("out-a")
    def outB: Path = root.resolve("out-b")
    def outC: Path = root.resolve("out-c")

    def sourcePath(uri: String): Path = root.resolve(uri)
    def sourceText(uri: String): String =
      new String(Files.readAllBytes(sourcePath(uri)), StandardCharsets.UTF_8)

    /** 0-based (line, startChar, endChar) of the nth occurrence of `token`
      * as a whole word in the source, for exact span assertions.
      */
    def tokenSpan(uri: String, token: String, nth: Int = 0): Span =
      val spans = tokenSpans(uri, token)
      if nth < spans.length then spans(nth)
      else throw AssertionError(s"token '$token' (occurrence $nth) not found in $uri")

    /** All whole-word occurrences of `token`, in source order. */
    def tokenSpans(uri: String, token: String): Vector[Span] =
      val out = Vector.newBuilder[Span]
      for (line, ln) <- sourceText(uri).linesIterator.toVector.zipWithIndex do
        var from = 0
        var i = line.indexOf(token, from)
        while i >= 0 do
          val beforeOk = i == 0 || !Character.isJavaIdentifierPart(line.charAt(i - 1))
          val after = i + token.length
          val afterOk =
            after >= line.length || !Character.isJavaIdentifierPart(line.charAt(after))
          if beforeOk && afterOk then out += Span(ln, i, ln, after)
          from = i + 1
          i = line.indexOf(token, from)
      out.result()

    /** Cursor position inside the nth whole-word occurrence of `token`. */
    def cursor(uri: String, token: String, nth: Int = 0): (Int, Int) =
      val span = tokenSpan(uri, token, nth)
      (span.startLine, span.startChar + 1)

  val sources: Map[String, String] = Map(
    // ---------------- target A ----------------
    "a/src/pkga/Core.scala" ->
      """package pkga
        |
        |class Core(val label: String):
        |  def ping: String = "core " + label
        |
        |object Core:
        |  def make(l: String): Core = new Core(l)
        |
        |trait Greeter:
        |  def greet(name: String): String
        |
        |enum Color:
        |  case Red, Green, Blue
        |
        |extension (c: Core) def shout: String = c.ping.toUpperCase
        |
        |given defaultCore: Core = Core.make("given")
        |""".stripMargin,
    "a/src/pkga/Impl.scala" ->
      """package pkga
        |
        |class LoudGreeter extends Greeter:
        |  def greet(name: String): String = "HI " + name
        |
        |object UseA:
        |  val core: Core = Core.make("a")
        |  val loud: String = core.shout
        |  val g2: Core = defaultCore
        |""".stripMargin,
    "a/src/pkga/Item.scala" ->
      """package pkga
        |
        |case class Item(id: Int)
        |
        |object MakeItems:
        |  val i1 = Item(1)
        |  val i2 = Item.apply(2)
        |  val i3 = new Item(3)
        |""".stripMargin,
    "a/src/pkga/Vars.scala" ->
      """package pkga
        |
        |class Counter:
        |  var value: Int = 0
        |
        |object UseCounter:
        |  def bump(c: Counter): Int =
        |    val tmp = c.value + 1
        |    c.value = tmp
        |    tmp
        |""".stripMargin,
    "a/src/pkga/Over.scala" ->
      """package pkga
        |
        |object Over:
        |  def fmt(i: Int): String = i.toString
        |  def fmt(s: String): String = s
        |  val a = fmt(1)
        |  val b = fmt("x")
        |""".stripMargin,
    "a/src/pkga/Widget.scala" ->
      """package pkga
        |
        |case class Widget(w: Int)
        |""".stripMargin,
    "a/src/pkga/GeneratedUse.scala" ->
      """package pkga
        |
        |object GeneratedUse:
        |  val z = Widget(9)
        |""".stripMargin,
    "a/src/pkga/Gadget.scala" ->
      """package pkga
        |
        |case class Gadget(g: Int)
        |""".stripMargin,
    "a/src/pkga/ReadonlyUse.scala" ->
      """package pkga
        |
        |object ReadonlyUse:
        |  val q = Gadget(3)
        |""".stripMargin,
    "dep/src/pkgdep/DepThing.scala" ->
      """package pkgdep
        |
        |case class DepThing(d: Int)
        |
        |object UseDep:
        |  val dt = DepThing(1)
        |""".stripMargin,
    "a/src/pkga/Alpha.scala" ->
      """package pkga
        |
        |case class Alpha(a: Int)
        |""".stripMargin,
    "a/src/pkga/Exported.scala" ->
      """package pkga
        |
        |object OriginalOwner:
        |  def exported(n: Int): Int = n + 1
        |
        |object ForwarderOwner:
        |  export OriginalOwner.exported
        |
        |object ExportedUse:
        |  val r = ForwarderOwner.exported(3)
        |""".stripMargin,
    "a/src/pkga/Copyable.scala" ->
      """package pkga
        |
        |case class Copyable(id: Int)
        |
        |object CopyableUse:
        |  val c = Copyable(1)
        |  val d = c.copy(id = 2)
        |""".stripMargin,
    "a/src/pkga/Beta.scala" ->
      """package pkga
        |
        |object Beta:
        |  val al = Alpha(5)
        |""".stripMargin,
    "a/src/pkga/Inline.scala" ->
      """package pkga
        |
        |object Inlines:
        |  inline def twice(x: Int): Int = x + x
        |  val here: Int = twice(1)
        |""".stripMargin,
    "a/src/pkga/Private.scala" ->
      """package pkga
        |
        |class Secretive:
        |  private val state: Int = 1
        |  private def helper(n: Int): Int = n + state
        |  def use: Int = helper(state)
        |""".stripMargin,
    "a/src/pkga/LocalDef.scala" ->
      """package pkga
        |
        |object LocalDefs:
        |  def countdown(n: Int): Int =
        |    def loop(m: Int): Int = if m <= 0 then 0 else m + loop(m - 1)
        |    loop(n)
        |""".stripMargin,
    "a/src/pkga/Using.scala" ->
      """package pkga
        |
        |object Rendering:
        |  def render(using c: Core): String = c.ping
        |  val rendered: String = render(using defaultCore)
        |""".stripMargin,
    "a/src/pkga/TopLevel.scala" ->
      """package pkga
        |
        |def topHelper(n: Int): Int = n * 2
        |val topConst: Int = 42
        |""".stripMargin,
    "a/src/pkga/Opaque.scala" ->
      """package pkga
        |
        |object Ids:
        |  opaque type UserId = Long
        |  object UserId:
        |    def wrap(l: Long): UserId = l
        |  val sample: UserId = UserId.wrap(7L)
        |""".stripMargin,
    "a/src/pkga/Named.scala" ->
      """package pkga
        |
        |class Named(val title: String):
        |  def shown: String = "[" + title + "]"
        |""".stripMargin,
    "a/src/pkga/Externals.scala" ->
      """package pkga
        |
        |object UsesList:
        |  val xs: List[Int] = List(1, 2, 3)
        |  val total: Int = xs.sum
        |""".stripMargin,
    "shared/src/shared/Shared.scala" ->
      """package shared
        |
        |object SharedThing:
        |  def tag: String = "shared"
        |""".stripMargin,
    // ---------------- target B (depends on A) ----------------
    "b/src/pkgb/UseB.scala" ->
      """package pkgb
        |
        |import pkga.*
        |
        |object UseB:
        |  val core: Core = Core.make("b")
        |  val item: Item = Item(42)
        |  def greetAll(g: Greeter): String = g.greet("b")
        |  val color: Color = Color.Green
        |  def loud(c: Core): String = c.shout
        |  val g2: Core = pkga.defaultCore
        |  val s: String = shared.SharedThing.tag
        |  val twiced: Int = pkga.Inlines.twice(21)
        |  val named: Named = Named("b")
        |  val theTitle: String = named.title
        |  val topUse: Int = pkga.topHelper(pkga.topConst)
        |""".stripMargin,
    // ---------------- target C (disconnected, reuses names) ----------------
    "c/src/pkga/CopyCore.scala" ->
      """package pkga
        |
        |class Core(val label: String):
        |  def ping: String = "c " + label
        |
        |object Core:
        |  def make(l: String): Core = new Core(l)
        |
        |object UseC:
        |  val core: Core = Core.make("c")
        |""".stripMargin
  )

  val targetASources: Vector[String] =
    sources.keys.filter(u => u.startsWith("a/") || u.startsWith("dep/")).toVector.sorted
  val sharedSources: Vector[String] =
    sources.keys.filter(_.startsWith("shared/")).toVector.sorted
  val targetBSources: Vector[String] =
    sources.keys.filter(_.startsWith("b/")).toVector.sorted
  val targetCSources: Vector[String] =
    sources.keys.filter(_.startsWith("c/")).toVector.sorted

  private lazy val libraryJars: Vector[String] =
    val jars = System
      .getProperty("java.class.path")
      .split(File.pathSeparator)
      .filter { p =>
        val name = Path.of(p).getFileName.toString
        name.startsWith("scala3-library") || name.startsWith("scala-library")
      }
      .toVector
    assert(jars.nonEmpty, "scala library jars not found on java.class.path")
    jars

  /** Compiles one target tree with `-Xsemanticdb` into `out`. */
  def compileTree(
      root: Path,
      uris: Vector[String],
      out: Path,
      extraClasspath: Vector[Path]
  ): Unit =
    Files.createDirectories(out)
    val cp = (libraryJars ++ extraClasspath.map(_.toString)).mkString(File.pathSeparator)
    val args = Array(
      "-Xsemanticdb",
      "-sourceroot",
      root.toString,
      "-d",
      out.toString,
      "-classpath",
      cp
    ) ++ uris.map(u => root.resolve(u).toString)
    val reporter = dotty.tools.dotc.Main.process(args)
    assert(
      !reporter.hasErrors,
      s"scalac failed for $out:\n${reporter.allErrors.mkString("\n")}"
    )

  /** Recompiles every tree of a fixture (used after source edits). */
  def compileAll(fx: Fixture): Unit =
    compileTree(fx.root, targetASources ++ sharedSources, fx.outA, Vector.empty)
    compileTree(fx.root, targetBSources ++ sharedSources, fx.outB, Vector(fx.outA))
    compileTree(fx.root, targetCSources, fx.outC, Vector.empty)

  /** The master fixture, compiled exactly once per test JVM. Never mutate. */
  lazy val master: Fixture =
    val root = Files.createTempDirectory("ls-rename-fixture-")
    root.toFile.deleteOnExit()
    for (uri, text) <- sources do
      val p = root.resolve(uri)
      Files.createDirectories(p.getParent)
      Files.write(p, text.getBytes(StandardCharsets.UTF_8))
    val fx = Fixture(root)
    compileAll(fx)
    fx

  /** Deep copy of the master fixture for suites that mutate files. */
  def cloneFixture(): Fixture =
    val src = master.root
    val dst = Files.createTempDirectory("ls-rename-fixture-clone-")
    dst.toFile.deleteOnExit()
    val stream = Files.walk(src)
    try
      for p <- stream.iterator().asScala do
        val rel = src.relativize(p)
        val target = dst.resolve(rel.toString)
        if Files.isDirectory(p) then Files.createDirectories(target)
        else
          Files.createDirectories(target.getParent)
          Files.copy(p, target, StandardCopyOption.COPY_ATTRIBUTES)
    finally stream.close()
    Fixture(dst)

  /** DocFacts for target A: generated/readonly/dependency-marked fixtures. */
  def factsA(uri: String): DocFacts =
    if uri == "a/src/pkga/GeneratedUse.scala" then
      DocFacts(generated = true, readonly = false, isDependencySource = false)
    else if uri == "a/src/pkga/ReadonlyUse.scala" then
      DocFacts(generated = false, readonly = true, isDependencySource = false)
    else if uri.startsWith("dep/") then
      DocFacts(generated = false, readonly = false, isDependencySource = true)
    else DocFacts.workspaceSource

  def workspaceFor(fx: Fixture): WorkspaceTargets =
    WorkspaceTargets(
      Vector(
        TargetSpec(
          bspId = "fixture-a",
          semanticdbRoot = fx.outA,
          sourceroot = fx.root,
          directDeps = Vector.empty,
          docFacts = factsA
        ),
        TargetSpec(
          bspId = "fixture-b",
          semanticdbRoot = fx.outB,
          sourceroot = fx.root,
          directDeps = Vector("fixture-a")
        ),
        TargetSpec(
          bspId = "fixture-c",
          semanticdbRoot = fx.outC,
          sourceroot = fx.root,
          directDeps = Vector.empty
        )
      )
    )

  /** One isolated store stack (MetaStore + SnapshotManager + orchestrator). */
  final case class Stack(
      storeDir: Path,
      meta: MetaStore,
      manager: SnapshotManager,
      orchestrator: QueryOrchestrator
  ) extends AutoCloseable:
    override def close(): Unit =
      manager.close()
      meta.close()

  def newStack(
      overlay: DirtyBufferOverlay = NoopOverlay,
      walCheckpointThresholdBytes: Long = MetaStore.DefaultWalThresholdBytes,
      syncWriteThrough: Boolean = true
  ): Stack =
    val dir = Files.createTempDirectory("ls-rename-store-")
    dir.toFile.deleteOnExit()
    val meta = MetaStore.open(dir.resolve("meta.sqlite"))
    val manager = SnapshotManager(dir.resolve("postings"))
    val pipeline = IngestPipeline(meta, manager, walCheckpointThresholdBytes = walCheckpointThresholdBytes)
    Stack(dir, meta, manager, QueryOrchestrator(meta, manager, pipeline, overlay, syncWriteThrough))
