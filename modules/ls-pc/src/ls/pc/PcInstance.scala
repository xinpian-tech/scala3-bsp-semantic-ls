package ls.pc

import java.net.URI
import java.nio.charset.StandardCharsets.UTF_8
import java.nio.file.Path
import java.util.concurrent.{CompletableFuture, ExecutionException, TimeUnit, TimeoutException}

import scala.jdk.CollectionConverters.*
import scala.jdk.OptionConverters.*

import com.google.gson.Gson
import org.eclipse.lsp4j.{CompletionItem, CompletionList, Diagnostic, Hover, Range, SignatureHelp}
import scala.meta.pc.{OffsetParams, PresentationCompiler}

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

  // --- ABI v2 payload queries ---

  /** Inlay hints for `[startOffset, endOffset]`; `flags` is the boundary
    * hint-category bitset decoded by [[PcInlayHintFlags.paramsFor]]. Results
    * are converted to the spi carriers, with opaque `data` carried verbatim as
    * canonical JSON bytes.
    */
  def inlayHints(uri: String, text: String, startOffset: Int, endOffset: Int, flags: Int): Vector[PcInlayHint] =
    val params = PcInlayHintFlags.paramsFor(URI.create(uri), text, startOffset, endOffset, flags)
    await("inlayHints", underlying.inlayHints(params)).asScala.toVector.map(toPcInlayHint)

  /** Semantic tokens of the whole buffer, as `[start, end)` offset nodes. */
  def semanticTokens(uri: String, text: String): Vector[PcSemanticNode] =
    await("semanticTokens", underlying.semanticTokens(PcVirtualFileParams(URI.create(uri), text)))
      .asScala
      .toVector
      .map(n => PcSemanticNode(n.start(), n.end(), n.tokenType(), n.tokenModifier()))

  /** Per query offset, the chain of enclosing selection ranges, innermost
    * first (each lsp4j `SelectionRange`'s parent links walked outward).
    */
  def selectionRanges(uri: String, text: String, offsets: Vector[Int]): Vector[Vector[Range]] =
    val params: java.util.List[OffsetParams] =
      offsets.map(off => PcOffsetParams(URI.create(uri), text, off): OffsetParams).asJava
    await("selectionRange", underlying.selectionRange(params)).asScala.toVector.map { sel =>
      val chain = Vector.newBuilder[Range]
      var current = sel
      while current != null do
        if current.getRange != null then chain += current.getRange
        current = current.getParent
      chain.result()
    }

  /** Auto-import candidates for `name` at `offset`, best first. */
  def autoImports(uri: String, text: String, name: String, offset: Int, isExtension: Boolean): Vector[PcAutoImport] =
    val params = PcOffsetParams(URI.create(uri), text, offset)
    await("autoImports", underlying.autoImports(name, params, java.lang.Boolean.valueOf(isExtension)))
      .asScala
      .toVector
      .map { r =>
        PcAutoImport(
          packageName = r.packageName(),
          edits = r.edits().asScala.toVector,
          symbol = r.symbol().toScala
        )
      }

  // --- code actions (each returns the raw edits; DisplayableException refusal
  // mapping is the facade's concern) ---

  def convertToNamedArguments(uri: String, text: String, offset: Int, argIndices: Vector[Int]): Vector[org.eclipse.lsp4j.TextEdit] =
    val indices: java.util.List[Integer] = argIndices.map(Integer.valueOf).asJava
    await(
      "convertToNamedArguments",
      underlying.convertToNamedArguments(PcOffsetParams(URI.create(uri), text, offset), indices)
    ).asScala.toVector

  def implementAbstractMembers(uri: String, text: String, offset: Int): Vector[org.eclipse.lsp4j.TextEdit] =
    await("implementAbstractMembers", underlying.implementAbstractMembers(PcOffsetParams(URI.create(uri), text, offset)))
      .asScala
      .toVector

  def extractMethod(uri: String, text: String, startOffset: Int, endOffset: Int, extractionOffset: Int): Vector[org.eclipse.lsp4j.TextEdit] =
    await(
      "extractMethod",
      underlying.extractMethod(
        PcRangeParams(URI.create(uri), text, startOffset, endOffset),
        PcOffsetParams(URI.create(uri), text, extractionOffset)
      )
    ).asScala.toVector

  def inlineValue(uri: String, text: String, offset: Int): Vector[org.eclipse.lsp4j.TextEdit] =
    await("inlineValue", underlying.inlineValue(PcOffsetParams(URI.create(uri), text, offset))).asScala.toVector

  def insertInferredType(uri: String, text: String, offset: Int): Vector[org.eclipse.lsp4j.TextEdit] =
    await("insertInferredType", underlying.insertInferredType(PcOffsetParams(URI.create(uri), text, offset)))
      .asScala
      .toVector

  /** `insertInferredMethod` has no typed entry point on the abstract
    * `PresentationCompiler` (the dotty implementation adds it), so it is
    * dispatched through the generic `codeAction` id seam.
    */
  def insertInferredMethod(uri: String, text: String, offset: Int): Vector[org.eclipse.lsp4j.TextEdit] =
    codeActionById("insertInferredMethod", scala.meta.pc.CodeActionId.InsertInferredMethod, uri, text, offset)

  /** The other id-only PC code action (`PcConvertToNamedLambdaParameters.codeActionId`). */
  def convertToNamedLambdaParameters(uri: String, text: String, offset: Int): Vector[org.eclipse.lsp4j.TextEdit] =
    codeActionById(
      "convertToNamedLambdaParameters",
      scala.meta.pc.CodeActionId.ConvertToNamedLambdaParameters,
      uri,
      text,
      offset
    )

  private def codeActionById(operation: String, id: String, uri: String, text: String, offset: Int): Vector[org.eclipse.lsp4j.TextEdit] =
    await(
      operation,
      underlying.codeAction[Object](
        PcOffsetParams(URI.create(uri), text, offset),
        id,
        java.util.Optional.empty[Object]()
      )
    ).asScala.toVector

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

  /** lsp4j `InlayHint` → spi carrier. A plain-string label becomes one part;
    * part tooltips keep the string (or the markup's raw value); opaque `data`
    * is serialized to its canonical JSON bytes and never interpreted.
    */
  private def toPcInlayHint(h: org.eclipse.lsp4j.InlayHint): PcInlayHint =
    val label = h.getLabel
    val parts =
      if label == null then Vector.empty
      else if label.isLeft then Vector(PcInlayLabelPart(label.getLeft))
      else
        label.getRight.asScala.toVector.map { p =>
          val tooltip = Option(p.getTooltip).map { t =>
            if t.isLeft then t.getLeft else t.getRight.getValue
          }
          PcInlayLabelPart(p.getValue, Option(p.getLocation), tooltip)
        }
    PcInlayHint(
      position = h.getPosition,
      labelParts = parts,
      kind = Option(h.getKind).map(_.getValue).getOrElse(0),
      paddingLeft = Option(h.getPaddingLeft).exists(_.booleanValue),
      paddingRight = Option(h.getPaddingRight).exists(_.booleanValue),
      textEdits = Option(h.getTextEdits).map(_.asScala.toVector),
      data = Option(h.getData).map(d => PcInstance.gson.toJson(d).getBytes(UTF_8).toIndexedSeq)
    )

  private def await[A](operation: String, future: CompletableFuture[A]): A =
    try future.get(timeoutMillis, TimeUnit.MILLISECONDS)
    catch
      case _: TimeoutException =>
        future.cancel(true)
        throw new PcTimeoutException(targetId, operation, timeoutMillis)
      case e: ExecutionException =>
        throw (if e.getCause != null then e.getCause else e)

object PcInstance:
  /** Serializer for opaque LSP4J payloads (`InlayHint.data`): the same GSON
    * codec LSP4J itself uses, matching the host boundary's byte-carrier idiom.
    */
  private val gson = new Gson()
