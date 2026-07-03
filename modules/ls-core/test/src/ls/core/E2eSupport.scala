package ls.core

import java.io.File
import java.nio.charset.StandardCharsets
import java.nio.file.{Files, Path}
import java.util.concurrent.{CompletableFuture, ConcurrentLinkedQueue}

import scala.jdk.CollectionConverters.*

import java.util.concurrent.atomic.AtomicInteger

import ch.epfl.scala.bsp4j.{
  BuildServer,
  CompileParams,
  CompileResult,
  ScalaBuildServer,
  ScalacOptionsParams,
  ScalacOptionsResult,
  ScalacOptionsItem
}
import org.eclipse.lsp4j.{MessageActionItem, MessageParams, PublishDiagnosticsParams, ShowMessageRequestParams}
import org.eclipse.lsp4j.services.LanguageClient

import ls.bsp.FakeBuildServer
import ls.index.Span

/** Fixture workspace for the end-to-end LSP test: real scalac-generated
  * SemanticDB, laid out to match [[ls.bsp.FakeBuildServer]]'s canned build
  * model exactly:
  *
  *   - target a: source DIRECTORY `a/src`, `-Xsemanticdb` with a
  *     `-semanticdb-target:` override and `-sourceroot <root>`;
  *   - target b: source FILE `b/src/B.scala`, plain `-Ysemanticdb`
  *     (targetroot = classDirectory), depends on a;
  *   - target c: source FILE `c/src/C.scala`, NO SemanticDB at all
  *     (IndexUnavailable).
  */
object E2eFixture:

  val coreUri = "a/src/pkga/Core.scala"
  val useUri = "a/src/pkga/Use.scala"
  val bUri = "b/src/B.scala"

  val sources: Map[String, String] = Map(
    coreUri ->
      """package pkga
        |
        |class Core(val label: String):
        |  def ping: String = "core " + label
        |
        |object Core:
        |  def make(l: String): Core = new Core(l)
        |""".stripMargin,
    useUri ->
      """package pkga
        |
        |object Use:
        |  val core: Core = Core.make("a")
        |  val p: String = core.ping
        |""".stripMargin,
    bUri ->
      """package pkgb
        |
        |object B:
        |  val c: pkga.Core = pkga.Core.make("b")
        |  val s: String = c.ping
        |""".stripMargin,
    "c/src/C.scala" -> "class CUnused\n"
  )

  final case class Ws(root: Path):
    def aSourceDir: Path = root.resolve("a").resolve("src")
    def bSourceFile: Path = root.resolve(bUri)
    def cSourceFile: Path = root.resolve("c/src/C.scala")
    def semanticdbOverride: Path = root.resolve("out").resolve("a").resolve("semanticdb")
    def classDirOf(name: String): Path = root.resolve("out").resolve(name).resolve("classes")

    def fileUri(uri: String): String = Uris.toUri(root.resolve(uri))
    def sourceText(uri: String): String =
      new String(Files.readAllBytes(root.resolve(uri)), StandardCharsets.UTF_8)

    /** 0-based span of the nth whole-word occurrence of `token`. */
    def tokenSpan(uri: String, token: String, nth: Int = 0): Span =
      val spans = tokenSpans(uri, token)
      assert(nth < spans.length, s"token '$token' occurrence $nth not found in $uri")
      spans(nth)

    def tokenSpans(uri: String, token: String): Vector[Span] =
      val out = Vector.newBuilder[Span]
      for (line, ln) <- sourceText(uri).linesIterator.toVector.zipWithIndex do
        var i = line.indexOf(token)
        while i >= 0 do
          val beforeOk = i == 0 || !Character.isJavaIdentifierPart(line.charAt(i - 1))
          val after = i + token.length
          val afterOk = after >= line.length || !Character.isJavaIdentifierPart(line.charAt(after))
          if beforeOk && afterOk then out += Span(ln, i, ln, after)
          i = line.indexOf(token, i + 1)
      out.result()

    /** Cursor inside the nth occurrence of `token`. */
    def cursor(uri: String, token: String, nth: Int = 0): (Int, Int) =
      val span = tokenSpan(uri, token, nth)
      (span.startLine, span.startChar + 1)

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

  def libraryClasspath: Vector[Path] = libraryJars.map(Path.of(_))

  private def compileTree(root: Path, files: Vector[Path], args: Vector[String]): Unit =
    val reporter = dotty.tools.dotc.Main.process((args ++ files.map(_.toString)).toArray)
    assert(!reporter.hasErrors, s"scalac failed:\n${reporter.allErrors.mkString("\n")}")

  /** The compiled master fixture, built once per test JVM. */
  lazy val master: Ws =
    val root = Files.createTempDirectory("ls-core-e2e-")
    root.toFile.deleteOnExit()
    for (uri, text) <- sources do
      val p = root.resolve(uri)
      Files.createDirectories(p.getParent)
      Files.write(p, text.getBytes(StandardCharsets.UTF_8))
    val ws = Ws(root)
    Files.createDirectories(ws.semanticdbOverride)
    Files.createDirectories(ws.classDirOf("a"))
    Files.createDirectories(ws.classDirOf("b"))
    Files.createDirectories(ws.classDirOf("c"))
    val libs = libraryJars.mkString(File.pathSeparator)
    // target a: semanticdb-target override + explicit sourceroot
    compileTree(
      root,
      Vector(root.resolve(coreUri), root.resolve(useUri)),
      Vector(
        "-Xsemanticdb",
        s"-semanticdb-target:${ws.semanticdbOverride}",
        "-sourceroot",
        root.toString,
        "-d",
        ws.classDirOf("a").toString,
        "-classpath",
        libs
      )
    )
    // target b: -Ysemanticdb semantics = targetroot is the class directory
    compileTree(
      root,
      Vector(ws.bSourceFile),
      Vector(
        "-Xsemanticdb",
        "-sourceroot",
        root.toString,
        "-d",
        ws.classDirOf("b").toString,
        "-classpath",
        (libraryJars :+ ws.classDirOf("a").toString).mkString(File.pathSeparator)
      )
    )
    // target c: no SemanticDB (IndexUnavailable)
    compileTree(
      root,
      Vector(ws.cSourceFile),
      Vector("-d", ws.classDirOf("c").toString, "-classpath", libs)
    )
    ws

/** [[FakeBuildServer]] is final; this delegate rewrites only the
  * buildTarget/scalacOptions classpath (the fake returns an empty one) so
  * the presentation compiler gets a real classpath, exactly as a real BSP
  * server would provide.
  */
/** One-shot compile outcome staged onto [[ClasspathAugmentingServer]]: the next
  * `buildTarget/compile` emits these diagnostics and returns this status.
  */
final case class CompileScenario(
    publishes: Vector[ch.epfl.scala.bsp4j.PublishDiagnosticsParams],
    status: ch.epfl.scala.bsp4j.StatusCode
)

final class ClasspathAugmentingServer(
    val underlying: FakeBuildServer,
    extraClasspath: String => Vector[Path]
) extends BuildServer
    with ScalaBuildServer:

  export underlying.{buildTargetScalacOptions as _, buildTargetCompile as _, *}

  val compileRequests = new AtomicInteger(0)

  private val stagedCompile =
    new java.util.concurrent.atomic.AtomicReference[Option[CompileScenario]](None)

  /** Stage a one-shot outcome for the next compile, then revert to the default
    * successful, diagnostic-free compile. Lets a test drive an exact diagnostic
    * shape without perturbing the compiles other tests depend on.
    */
  def stageCompile(scenario: CompileScenario): Unit = stagedCompile.set(Some(scenario))

  override def buildTargetCompile(params: CompileParams): CompletableFuture[CompileResult] =
    compileRequests.incrementAndGet()
    val status = stagedCompile.getAndSet(None) match
      case Some(scenario) =>
        scenario.publishes.foreach(underlying.client.onBuildPublishDiagnostics)
        scenario.status
      case None => ch.epfl.scala.bsp4j.StatusCode.OK
    val result = new CompileResult(status)
    result.setOriginId(params.getOriginId)
    CompletableFuture.completedFuture(result)

  override def buildTargetScalacOptions(
      params: ScalacOptionsParams
  ): CompletableFuture[ScalacOptionsResult] =
    underlying.buildTargetScalacOptions(params).thenApply { result =>
      val items = result.getItems.asScala.map { item =>
        val name = item.getTarget.getUri.stripPrefix("bsp://workspace/")
        val classpath =
          Option(item.getClasspath).map(_.asScala.toVector).getOrElse(Vector.empty) ++
            extraClasspath(name).map(_.toUri.toString)
        new ScalacOptionsItem(item.getTarget, item.getOptions, classpath.asJava, item.getClassDirectory)
      }
      new ScalacOptionsResult(items.asJava)
    }

/** Minimal LSP client for the e2e Launcher: records diagnostics/messages.
  *
  * Every annotated default method of [[LanguageClient]] is overridden
  * explicitly (annotation-free): Scala 3 otherwise generates mixin
  * forwarders that copy the JSON-RPC annotations onto this class, which
  * lsp4j's launcher scan rejects as duplicate RPC methods.
  */
final class RecordingLanguageClient extends LanguageClient:
  val diagnostics = new ConcurrentLinkedQueue[PublishDiagnosticsParams]
  val logs = new ConcurrentLinkedQueue[MessageParams]

  private def done[A]: CompletableFuture[A] = CompletableFuture.completedFuture(null.asInstanceOf[A])

  override def telemetryEvent(obj: Object): Unit = ()
  override def publishDiagnostics(params: PublishDiagnosticsParams): Unit =
    diagnostics.add(params)
    ()
  override def showMessage(params: MessageParams): Unit = ()
  override def showMessageRequest(
      params: ShowMessageRequestParams
  ): CompletableFuture[MessageActionItem] = done
  override def logMessage(params: MessageParams): Unit =
    logs.add(params)
    ()
  override def applyEdit(params: org.eclipse.lsp4j.ApplyWorkspaceEditParams)
      : CompletableFuture[org.eclipse.lsp4j.ApplyWorkspaceEditResponse] = done
  override def registerCapability(params: org.eclipse.lsp4j.RegistrationParams)
      : CompletableFuture[Void] = done
  override def unregisterCapability(params: org.eclipse.lsp4j.UnregistrationParams)
      : CompletableFuture[Void] = done
  override def showDocument(params: org.eclipse.lsp4j.ShowDocumentParams)
      : CompletableFuture[org.eclipse.lsp4j.ShowDocumentResult] = done
  override def workspaceFolders()
      : CompletableFuture[java.util.List[org.eclipse.lsp4j.WorkspaceFolder]] = done
  override def configuration(params: org.eclipse.lsp4j.ConfigurationParams)
      : CompletableFuture[java.util.List[Object]] = done
  override def createProgress(params: org.eclipse.lsp4j.WorkDoneProgressCreateParams)
      : CompletableFuture[Void] = done
  override def notifyProgress(params: org.eclipse.lsp4j.ProgressParams): Unit = ()
  override def logTrace(params: org.eclipse.lsp4j.LogTraceParams): Unit = ()
  override def refreshSemanticTokens(): CompletableFuture[Void] = done
  override def refreshCodeLenses(): CompletableFuture[Void] = done
  override def refreshInlayHints(): CompletableFuture[Void] = done
  override def refreshInlineValues(): CompletableFuture[Void] = done
  override def refreshDiagnostics(): CompletableFuture[Void] = done
  override def refreshFoldingRanges(): CompletableFuture[Void] = done
  override def refreshTextDocumentContent(
      params: org.eclipse.lsp4j.TextDocumentContentRefreshParams
  ): CompletableFuture[Void] = done
