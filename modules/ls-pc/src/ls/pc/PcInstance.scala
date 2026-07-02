package ls.pc

import java.net.URI
import java.nio.file.Path
import java.util.concurrent.{CompletableFuture, ExecutionException, TimeUnit, TimeoutException}

import scala.jdk.CollectionConverters.*
import scala.jdk.OptionConverters.*

import org.eclipse.lsp4j.{CompletionItem, CompletionList, Diagnostic, Hover, Range, SignatureHelp}
import scala.meta.pc.PresentationCompiler

/** Thrown when a synchronous PC request exceeds its budget. The owning
  * [[PcWorkerManager]] treats the instance as wedged: it is shut down and
  * lazily recreated on the next request for the same target.
  */
final class PcTimeoutException(val targetId: String, val operation: String, val timeoutMillis: Long)
    extends RuntimeException(
      s"presentation compiler request '$operation' for target '$targetId' timed out after ${timeoutMillis}ms"
    )

/** Synchronous wrapper around one `scala.meta.pc.PresentationCompiler` for one
  * build target. All queries take the full in-memory text (dirty buffer) plus
  * either a UTF-16 code-unit offset or an LSP line/character position, which
  * is converted against that text with [[Utf16Text]].
  *
  * Instances are created by [[PcWorkerManager]] with plugin-patched options
  * and source path; results are plain lsp4j model objects and are never
  * persisted anywhere.
  */
final class PcInstance private[pc] (
    val targetId: String,
    /** Compiler options actually passed to `newInstance`, after compiler-plugin
      * config injection and service-plugin `patchOptions`.
      */
    val effectiveOptions: Vector[String],
    /** Source path actually supplied to the PC, after synthetic-source
      * materialization and service-plugin `patchSourcePath`.
      */
    val effectiveSourcePath: Vector[Path],
    /** file: URIs of materialized plugin synthetic sources for this target. */
    val syntheticUris: Set[String],
    private[pc] val underlying: PresentationCompiler,
    val timeoutMillis: Long):

  def scalaVersion: String = underlying.scalaVersion()
  def isLoaded: Boolean = underlying.isLoaded()

  // --- completion ---

  def complete(uri: String, text: String, offset: Int): CompletionList =
    await("completion", underlying.complete(PcOffsetParams(URI.create(uri), text, offset)))

  def complete(uri: String, text: String, line: Int, character: Int): CompletionList =
    complete(uri, text, Utf16Text.offsetAt(text, line, character))

  def completionItemResolve(item: CompletionItem, symbol: String): CompletionItem =
    await("completionItemResolve", underlying.completionItemResolve(item, symbol))

  // --- hover ---

  def hover(uri: String, text: String, offset: Int): Option[Hover] =
    await("hover", underlying.hover(PcOffsetParams(URI.create(uri), text, offset))).toScala
      .map(_.toLsp())

  def hover(uri: String, text: String, line: Int, character: Int): Option[Hover] =
    hover(uri, text, Utf16Text.offsetAt(text, line, character))

  // --- signature help ---

  def signatureHelp(uri: String, text: String, offset: Int): SignatureHelp =
    await("signatureHelp", underlying.signatureHelp(PcOffsetParams(URI.create(uri), text, offset)))

  def signatureHelp(uri: String, text: String, line: Int, character: Int): SignatureHelp =
    signatureHelp(uri, text, Utf16Text.offsetAt(text, line, character))

  // --- definition / typeDefinition ---

  def definition(uri: String, text: String, offset: Int): DefinitionResult =
    toDefinitionResult(
      await("definition", underlying.definition(PcOffsetParams(URI.create(uri), text, offset)))
    )

  def definition(uri: String, text: String, line: Int, character: Int): DefinitionResult =
    definition(uri, text, Utf16Text.offsetAt(text, line, character))

  def typeDefinition(uri: String, text: String, offset: Int): DefinitionResult =
    toDefinitionResult(
      await("typeDefinition", underlying.typeDefinition(PcOffsetParams(URI.create(uri), text, offset)))
    )

  def typeDefinition(uri: String, text: String, line: Int, character: Int): DefinitionResult =
    typeDefinition(uri, text, Utf16Text.offsetAt(text, line, character))

  // --- prepareRename ---

  def prepareRename(uri: String, text: String, offset: Int): Option[Range] =
    await("prepareRename", underlying.prepareRename(PcOffsetParams(URI.create(uri), text, offset))).toScala

  def prepareRename(uri: String, text: String, line: Int, character: Int): Option[Range] =
    prepareRename(uri, text, Utf16Text.offsetAt(text, line, character))

  // --- lifecycle / diagnostics ---

  /** Push new text to the PC and collect its (secondary) diagnostics. */
  def didChange(uri: String, text: String): Vector[Diagnostic] =
    await(
      "didChange",
      underlying.didChange(PcVirtualFileParams(URI.create(uri), text, returnDiagnostics = true))
    ).asScala.toVector

  def didClose(uri: String): Unit = underlying.didClose(URI.create(uri))

  def restart(): Unit = underlying.restart()

  def shutdown(): Unit = underlying.shutdown()

  private def toDefinitionResult(raw: scala.meta.pc.DefinitionResult): DefinitionResult =
    val locs = raw.locations().asScala.toVector.map { loc =>
      val origin =
        if syntheticUris.contains(loc.getUri) then DefinitionOrigin.Synthetic
        else DefinitionOrigin.Workspace
      DefinitionLocation(loc, origin)
    }
    DefinitionResult(raw.symbol(), locs)

  private def await[A](operation: String, future: CompletableFuture[A]): A =
    try future.get(timeoutMillis, TimeUnit.MILLISECONDS)
    catch
      case _: TimeoutException =>
        future.cancel(true)
        throw new PcTimeoutException(targetId, operation, timeoutMillis)
      case e: ExecutionException =>
        throw (if e.getCause != null then e.getCause else e)
