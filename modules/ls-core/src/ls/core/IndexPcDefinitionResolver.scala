package ls.core

import java.nio.file.Path

import scala.util.control.NonFatal

import ls.index.{DocOrd, OccurrenceSink, Span, TargetOrd}
import ls.pc.PcDefinitionResolver
import ls.postings.SnapshotManager
import ls.sqlite.MetaStore
import org.eclipse.lsp4j.Location

/** Index-backed cross-file go-to-definition for the presentation compiler
  * (the [[ls.pc.PcDefinitionResolver]] seam behind `SymbolSearch.definition`).
  *
  * Query: SemanticDB symbol -> snapshot symbol ordinal -> alias (ref) group ->
  * `scanDefinitions` occurrences -> `file://` Locations, mirroring how
  * [[ls.rename.ReferencesEngine]] serves `includeDeclaration` hits. The index
  * stores global SemanticDB symbols verbatim ([[ls.rename.SymbolEncoding]]),
  * which is exactly the string the dotty PC passes (its
  * `SemanticdbSymbols.symbolName`), so the lookup is a direct dictionary hit;
  * local symbols are per-document and never cross files, so they simply miss.
  *
  * Threading: this runs on PC executor threads (in-process backend) or on the
  * forked worker's jsonrpc message thread (parent side of the child->parent
  * callback) — NEVER on the single index executor, which may itself be
  * blocked inside a PC request via the dirty-buffer overlay. It therefore
  * touches only thread-safe state: the immutable postings snapshot
  * (retain/release) and the SQLite READER pool for sourceroots. The
  * single-threaded writer connection is never used, and nothing here waits on
  * a PC or worker response, so no executor cycle can deadlock.
  */
final class IndexPcDefinitionResolver(meta: MetaStore, snapshots: SnapshotManager)
    extends PcDefinitionResolver:

  override def definition(semanticdbSymbol: String, fromFileUri: String): Vector[Location] =
    if semanticdbSymbol == null || semanticdbSymbol.isEmpty then Vector.empty
    else
      try
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
                    // object/apply, a getter/setter pair, …). `SymbolSearch`
                    // asked for ONE symbol, so keep only occurrences that define
                    // exactly `sym` — otherwise cross-file go-to would also jump
                    // to unrelated alias declarations. Collect raw first, then
                    // filter by the symbol at each occurrence's name start.
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
                    val sourcerootOf: Map[Long, Option[Path]] =
                      collected.map(_._2).distinct.map(id => id -> sourcerootOnReader(id)).toMap
                    collected
                      .flatMap { (docOrd, targetId, span) =>
                        sourcerootOf.getOrElse(targetId, None).map { root =>
                          val abs = Uris.fromSdbUri(root, snap.uriOf(DocOrd(docOrd)))
                          LspConvert.location(Uris.toUri(abs), span)
                        }
                      }
                      .distinct
                      .sortBy(l =>
                        (l.getUri, l.getRange.getStart.getLine, l.getRange.getStart.getCharacter)
                      )
          }
          .getOrElse(Vector.empty)
      catch case NonFatal(_) => Vector.empty

  /** Sourceroot of a target row, read on a READER connection (this thread must
    * never touch the single-threaded writer; precedent: workspaceSymbolSearch).
    */
  private def sourcerootOnReader(targetId: Long): Option[Path] =
    meta.readers
      .withReader { conn =>
        conn
          .prepare("SELECT sourceroot FROM targets WHERE target_id = ?")
          .bindLong(1, targetId)
          .queryOne(_.columnText(0))
      }
      .map(Path.of(_))
