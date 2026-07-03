package ls.core

import scala.collection.mutable
import scala.jdk.CollectionConverters.*

import org.eclipse.lsp4j.{Diagnostic as LspDiagnostic, PublishDiagnosticsParams as LspPublish}
import ch.epfl.scala.bsp4j.PublishDiagnosticsParams as BspPublish

/** Routes BSP `build/publishDiagnostics` (scoped to a `(document, build target)`
  * pair) to LSP `textDocument/publishDiagnostics` (scoped to a document only).
  *
  * Because several build targets can report diagnostics for one shared source,
  * the router keeps per-`(uri, target)` state and merges every target's current
  * diagnostics into a single publish per uri. The BSP `reset` flag is honored
  * per target: a reset replaces that target's list (and clears it when empty)
  * without touching sibling targets on the same uri. A uri that has never been
  * published non-empty and is still empty produces no publish, so a clean
  * recompile does not spam empty notifications; a uri that transitions from
  * non-empty to empty publishes one clearing (empty-list) notification.
  *
  * BSP notifications for one connection are delivered on a single jsonrpc reader
  * thread, but `accept` synchronizes so state stays consistent even if the
  * transport ever delivers concurrently.
  */
final class DiagnosticRouter(sink: LspPublish => Unit, toFileUri: String => String = identity):

  /** fileUri -> (targetUri -> diagnostics), insertion-ordered by target so the
    * merged publish is deterministic.
    */
  private val byUri = mutable.Map.empty[String, mutable.LinkedHashMap[String, Vector[LspDiagnostic]]]
  private val publishedNonEmpty = mutable.Set.empty[String]

  def accept(params: BspPublish): Unit = synchronized {
    val fileUri = toFileUri(params.getTextDocument.getUri)
    val target = Option(params.getBuildTarget).map(_.getUri).getOrElse("")
    val reset = Option(params.getReset).exists(_.booleanValue)
    val incoming =
      Option(params.getDiagnostics)
        .map(_.asScala.toVector.map(LspConvert.diagnostic))
        .getOrElse(Vector.empty)

    val perTarget = byUri.getOrElseUpdate(fileUri, mutable.LinkedHashMap.empty)
    if reset then
      if incoming.isEmpty then perTarget.remove(target)
      else perTarget(target) = incoming
    else
      perTarget(target) = perTarget.getOrElse(target, Vector.empty) ++ incoming

    val union = perTarget.valuesIterator.flatten.toVector
    if union.nonEmpty then
      publishedNonEmpty += fileUri
      sink(new LspPublish(fileUri, union.asJava))
    else
      byUri.remove(fileUri)
      if publishedNonEmpty.remove(fileUri) then
        sink(new LspPublish(fileUri, java.util.List.of[LspDiagnostic]()))
    // else: empty and never published non-empty -> suppress the spurious notify.
  }
