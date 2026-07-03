package ls.core

import java.nio.file.{Files, Path}
import java.util.concurrent.{CompletableFuture, TimeUnit}

import scala.jdk.CollectionConverters.*
import scala.jdk.OptionConverters.*
import scala.util.control.NonFatal

import org.eclipse.lsp4j.*
import org.eclipse.lsp4j.services.LanguageClient

/** Headless AOT-training workload.
  *
  * Boots the real [[ScalaLs]] against a workspace in-process (no stdio, no
  * pipes) and drives the runtime hot paths once so a JVM launched with
  * `-XX:AOTMode=record` observes the classes the server loads and links:
  * LSP initialize, BSP initialize (real mill-bsp when the workspace has a
  * `.bsp` connection, otherwise the graceful no-BSP fallback), the SQLite +
  * snapshot store open, a SemanticDB-backed workspace/symbol and references
  * query, and a presentation-compiler completion — then a clean shutdown.
  *
  * Every step is bounded and failure-tolerant: a missing `.bsp`, an empty
  * index, or a slow subsystem degrades to a logged skip rather than a hang, so
  * the run always terminates and returns 0 (the training workload is about
  * loading classes, not asserting results).
  */
object AotTrain:

  private val InitTimeoutMillis = 60000L
  private val BootstrapTimeoutMillis = 180000L
  private val RequestTimeoutMillis = 60000L

  def run(workspaceRoot: Path, log: String => Unit = msg => System.err.println(s"[aot-train] $msg")): Int =
    val root = workspaceRoot.toAbsolutePath.normalize
    log(s"training workload over $root")

    val server = new ScalaLs(ScalaLs.Config(exitProcessOnExit = false))
    server.connect(new SilentLanguageClient)

    def step(name: String)(body: => Unit): Unit =
      try body
      catch case NonFatal(t) => log(s"step '$name' skipped: $t")

    def await[A](name: String, f: CompletableFuture[A]): Option[A] =
      try Some(f.get(RequestTimeoutMillis, TimeUnit.MILLISECONDS))
      catch case NonFatal(t) => { log(s"request '$name' skipped: $t"); None }

    try
      // 1. LSP initialize + 2/3. bootstrap (BSP initialize, SQLite open,
      //    snapshot recovery, initial ingest) run on the initialized thread.
      val initParams = new InitializeParams()
      initParams.setRootUri(Uris.toUri(root))
      step("initialize")(server.initialize(initParams).get(InitTimeoutMillis, TimeUnit.MILLISECONDS))
      server.initialized(new InitializedParams())
      if !server.awaitBootstrap(BootstrapTimeoutMillis) then log("bootstrap did not finish in time; continuing")

      val docs = server.getTextDocumentService
      val workspace = server.getWorkspaceService

      // 4. workspace/symbol (SQLite FTS + fuzzy sidecar).
      step("workspace/symbol") {
        await("workspace/symbol", workspace.symbol(new WorkspaceSymbolParams("a")))
      }

      // 5/6. open a real source, then references + PC completion over it.
      firstScalaSource(root).foreach { source =>
        val uri = Uris.toUri(source)
        val text =
          try Files.readString(source)
          catch case NonFatal(_) => ""
        step("didOpen") {
          val item = new TextDocumentItem(uri, "scala", 1, text)
          docs.didOpen(new DidOpenTextDocumentParams(item))
        }
        val pos = firstIdentifierPosition(text)
        step("references") {
          val params = new ReferenceParams(new TextDocumentIdentifier(uri), pos, new ReferenceContext(true))
          await("references", docs.references(params))
        }
        step("completion") {
          val params = new CompletionParams(new TextDocumentIdentifier(uri), pos)
          await("completion", docs.completion(params))
        }
      }

      // 7. clean shutdown.
      step("shutdown")(server.shutdown().get(RequestTimeoutMillis, TimeUnit.MILLISECONDS))
      server.exit()
      log("training workload complete")
      0
    catch
      case NonFatal(t) =>
        log(s"training workload failed: $t")
        try server.exit()
        catch case NonFatal(_) => ()
        // A failed workload still recorded whatever it loaded before failing;
        // exit 0 so the record run does not abort the record→create pipeline.
        0

  /** First `.scala` source under the workspace, skipping generated/build dirs. */
  private def firstScalaSource(root: Path): Option[Path] =
    val skip = Set("out", ".bsp", ".scala3-bsp-semantic-ls", ".git", "target")
    val stream =
      try Files.walk(root)
      catch case NonFatal(_) => return None
    try
      stream
        .filter(p => Files.isRegularFile(p))
        .filter(p => p.getFileName.toString.endsWith(".scala"))
        .filter(p => !root.relativize(p).iterator.asScala.exists(seg => skip.contains(seg.toString)))
        .findFirst()
        .map(identity)
        .toScala
    finally stream.close()

  /** A position at the first identifier-looking token, so references/completion
    * have a plausible cursor; falls back to the file start.
    */
  private def firstIdentifierPosition(text: String): Position =
    text.linesIterator.zipWithIndex
      .map { (line, ln) =>
        val idx = line.indexWhere(Character.isJavaIdentifierStart)
        Option.when(idx >= 0 && !line.take(idx).trim.startsWith("//"))(new Position(ln, idx + 1))
      }
      .collectFirst { case Some(p) => p }
      .getOrElse(new Position(0, 0))

/** No-op LSP client for the headless training run: swallows every server
  * notification/request. ls-core compiles with `-Xmixin-force-forwarders:false`,
  * so only the abstract [[LanguageClient]] methods need bodies.
  */
private final class SilentLanguageClient extends LanguageClient:
  private def done[A]: CompletableFuture[A] = CompletableFuture.completedFuture(null.asInstanceOf[A])
  override def telemetryEvent(obj: Object): Unit = ()
  override def publishDiagnostics(params: PublishDiagnosticsParams): Unit = ()
  override def showMessage(params: MessageParams): Unit = ()
  override def showMessageRequest(params: ShowMessageRequestParams): CompletableFuture[MessageActionItem] = done
  override def logMessage(params: MessageParams): Unit = ()
