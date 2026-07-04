package ls.core

import java.nio.file.Path

import scala.util.control.NonFatal

import ls.index.{DocOrd, OccurrenceSink, Span, TargetOrd}
import ls.pc.PcDefinitionResolver
import ls.postings.SnapshotManager
import ls.rename.ingest.WorkspaceTargets
import ls.sqlite.MetaStore
import org.eclipse.lsp4j.Location

/** Index-backed cross-file go-to-definition for the presentation compiler
  * (the [[ls.pc.PcDefinitionResolver]] seam behind `SymbolSearch.definition`).
  *
  * Query: SemanticDB symbol -> snapshot symbol ordinal -> alias (ref) group ->
  * `scanDefinitions` occurrences -> keep only the requested symbol's own
  * declarations -> restrict to the requesting buffer's target context ->
  * `file://` Locations.
  *
  * The index stores global SemanticDB symbols verbatim
  * ([[ls.rename.SymbolEncoding]]), exactly the string the dotty PC passes (its
  * `SemanticdbSymbols.symbolName`), so the lookup is a direct dictionary hit;
  * local symbols are per-document and never cross files, so they simply miss.
  *
  * Target scoping (mirrors [[ls.rename.ReferencesEngine]]'s allowed-target
  * pruning): the same SemanticDB string can be DEFINED in more than one
  * disconnected target (two modules reusing a package/class name). Go-to from a
  * buffer in target T must only reach a definition T can SEE — a target in T's
  * forward dependency closure — otherwise editors jump to an unrelated
  * duplicate. `workspace` supplies the live dependency graph; the buffer's
  * target is found by its deepest containing sourceroot.
  *
  * Threading: this runs on PC executor threads (in-process backend) or on the
  * forked worker's jsonrpc message thread — NEVER on the single index executor,
  * which may itself be blocked inside a PC request via the dirty-buffer overlay.
  * It touches only thread-safe state: the immutable postings snapshot
  * (retain/release), the SQLite READER pool (never the single-writer NOMUTEX
  * connection), and the immutable `@volatile` workspace graph. Nothing here
  * waits on a PC or worker response, so no executor cycle can deadlock.
  */
final class IndexPcDefinitionResolver(
    meta: MetaStore,
    snapshots: SnapshotManager,
    workspace: () => Option[WorkspaceTargets] = () => None
) extends PcDefinitionResolver:

  override def definition(semanticdbSymbol: String, fromFileUri: String): Vector[Location] =
    if semanticdbSymbol == null || semanticdbSymbol.isEmpty then Vector.empty
    else
      try
        // The bspIds a source in the requesting buffer's target can see. None
        // when no target owns the buffer (then results are not target-scoped).
        val allowedBspIds: Option[Set[String]] = requestingForwardClosure(fromFileUri)
        snapshots
          .withCurrent { snap =>
            snap.symbolOrdOf(semanticdbSymbol) match
              case None => Vector.empty[Location]
              case Some(sym) =>
                snap.refGroupOf(sym) match
                  case None => Vector.empty[Location]
                  case Some(group) =>
                    // scanDefinitions returns the definitions of the WHOLE ref
                    // group, which unions aliases (a class with its companion
                    // object/apply, a getter/setter pair, …). Collect raw first.
                    val raw = Vector.newBuilder[(Int, Int, Int, Int)]
                    snap.scanDefinitions(
                      group,
                      new OccurrenceSink:
                        override def accept(
                            docOrd: Int,
                            targetOrd: Int,
                            docEpoch: Int,
                            packedStart: Int,
                            packedEnd: Int,
                            flags: Int
                        ): Unit =
                          raw += ((docOrd, targetOrd, packedStart, packedEnd))
                    )
                    // Keep only occurrences that define EXACTLY `sym` (not the
                    // other members of its ref group).
                    val collected: Vector[(Int, Long, Span)] =
                      raw.result().flatMap { (docOrd, targetOrd, packedStart, packedEnd) =>
                        val sl = Span.unpackLine(packedStart)
                        val sc = Span.unpackChar(packedStart)
                        if snap.symbolAt(DocOrd(docOrd), sl, sc).exists(_.symbolOrd == sym) then
                          Some(
                            (
                              docOrd,
                              snap.targetIdOf(TargetOrd(targetOrd)).value,
                              Span(sl, sc, Span.unpackLine(packedEnd), Span.unpackChar(packedEnd))
                            )
                          )
                        else None
                      }
                    // (sourceroot, bspId) per distinct target, on the reader pool.
                    val targetInfo: Map[Long, Option[(Path, String)]] =
                      collected.map(_._2).distinct.map(id => id -> targetInfoOnReader(id)).toMap
                    collected
                      .flatMap { (docOrd, targetId, span) =>
                        targetInfo.getOrElse(targetId, None).flatMap { (root, bspId) =>
                          // Restrict to targets the requesting buffer can see.
                          if allowedBspIds.forall(_.contains(bspId)) then
                            val abs = Uris.fromSdbUri(root, snap.uriOf(DocOrd(docOrd)))
                            Some(LspConvert.location(Uris.toUri(abs), span))
                          else None
                        }
                      }
                      .distinct
                      .sortBy(l =>
                        (l.getUri, l.getRange.getStart.getLine, l.getRange.getStart.getCharacter)
                      )
          }
          .getOrElse(Vector.empty)
      catch case NonFatal(_) => Vector.empty

  /** The forward dependency closure (bspIds) of the target owning `fromFileUri`,
    * found by its deepest containing sourceroot in the live workspace graph, or
    * None when no target owns the buffer (results are then unscoped). Reads only
    * the immutable `@volatile` workspace — safe on PC threads.
    */
  private def requestingForwardClosure(fromFileUri: String): Option[Set[String]] =
    for
      ws <- workspace()
      path <- toPathOpt(fromFileUri)
      spec <- ws.targets
        .filter(t => path.startsWith(t.sourceroot.toAbsolutePath.normalize))
        .sortBy(-_.sourceroot.getNameCount)
        .headOption
    yield ws.forwardDependencyClosure(spec.bspId)

  private def toPathOpt(fileUri: String): Option[Path] =
    try Some(Uris.toPath(fileUri).toAbsolutePath.normalize)
    catch case NonFatal(_) => None

  /** (sourceroot, bsp_id) of a target row, read on a READER connection (this
    * thread must never touch the single-threaded writer).
    */
  private def targetInfoOnReader(targetId: Long): Option[(Path, String)] =
    meta.readers.withReader { conn =>
      conn
        .prepare("SELECT sourceroot, bsp_id FROM targets WHERE target_id = ?")
        .bindLong(1, targetId)
        .queryOne(st => (Path.of(st.columnText(0)), st.columnText(1)))
    }
