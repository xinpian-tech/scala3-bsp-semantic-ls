package ls.rename

import scala.collection.mutable

import ls.index.*
import ls.postings.PostingsSnapshot

/** One reference location. `fromOverlay` marks dirty-buffer additions (they
  * are not index truth); `role` is Definition for declaration hits surfaced
  * by includeDeclaration.
  */
final case class ReferenceHit(loc: Loc, role: Role, fromOverlay: Boolean)

final case class ReferencesResult(hits: Vector[ReferenceHit], needsReindex: Boolean):
  def locations: Vector[Loc] = hits.map(_.loc)

/** Workspace references over exact mmap postings (plan 12.3).
  *
  * Pipeline: symbol-at-cursor -> ref group -> allowed targets = reverse
  * dependency closure of the definition target (exact upper bound from the
  * BSP graph; all targets when the definition target is unknown) ->
  * scanReferences (+ scanDefinitions when includeDeclaration; definitions
  * are also restricted to the allowed targets so disconnected targets that
  * reuse symbol names never leak in) -> dedupe -> sorted by (uri, position).
  * The epoch filter runs inside the segment reader. The dirty-buffer overlay
  * may add hits, marked `fromOverlay`.
  */
final class ReferencesEngine(orchestrator: QueryOrchestrator):

  def references(
      uri: String,
      line: Int,
      character: Int,
      includeDeclaration: Boolean
  ): ReferencesResult =
    val cursor = orchestrator.symbolAtCursor(uri, line, character)
    if cursor.pcOnly then throw LsException(LsError.PcOnlySymbol())

    val indexHits: Vector[ReferenceHit] =
      orchestrator.snapshots
        .withCurrent(snap => snapshotHits(snap, cursor, includeDeclaration))
        .getOrElse {
          if cursor.source == ResolutionSource.RawSemanticdb then
            rawFallback(cursor, includeDeclaration)
          else throw LsException(LsError.NotIndexed(uri))
        }

    val overlayHits: Vector[ReferenceHit] =
      orchestrator.overlay
        .occurrencesOf(cursor.semanticSymbol)
        .getOrElse(Vector.empty)
        .map(loc => ReferenceHit(loc, Role.Reference, fromOverlay = true))

    ReferencesResult(
      dedupeAndSort(indexHits ++ overlayHits),
      needsReindex = cursor.needsReindex
    )

  private def snapshotHits(
      snap: PostingsSnapshot,
      cursor: CursorSymbol,
      includeDeclaration: Boolean
  ): Vector[ReferenceHit] =
    snap.symbolOrdOf(cursor.encodedSymbol) match
      case None =>
        // Fresh symbol not in the snapshot yet: RawSemanticDBPath serves its
        // own document; anything else is a stale index we refuse to guess on.
        if cursor.source == ResolutionSource.RawSemanticdb then
          rawFallback(cursor, includeDeclaration)
        else throw LsException(LsError.StaleIndex(cursor.uri))
      case Some(ord) =>
        snap.refGroupOf(ord) match
          case None => Vector.empty
          case Some(group) =>
            val allowed = orchestrator.allowedTargetsFor(snap, ord)
            val out = Vector.newBuilder[ReferenceHit]
            val sink = collectingSink(snap, allowed, out)
            snap.scanReferences(group, allowed, sink)
            if includeDeclaration then snap.scanDefinitions(group, sink)
            out.result()

  /** Sink converting postings records to hits; also enforces the allowed
    * target set for definition scans (the reader only prunes reference
    * scans).
    */
  private def collectingSink(
      snap: PostingsSnapshot,
      allowed: TargetBitset,
      out: mutable.Builder[ReferenceHit, Vector[ReferenceHit]]
  ): OccurrenceSink =
    new OccurrenceSink:
      override def accept(
          docOrd: Int,
          targetOrd: Int,
          docEpoch: Int,
          packedStart: Int,
          packedEnd: Int,
          flags: Int
      ): Unit =
        if allowed.contains(targetOrd) then
          val span = Span(
            Span.unpackLine(packedStart),
            Span.unpackChar(packedStart),
            Span.unpackLine(packedEnd),
            Span.unpackChar(packedEnd)
          )
          val role =
            if OccFlags.has(flags, OccFlags.Definition) then Role.Definition
            else Role.Reference
          out += ReferenceHit(
            Loc(snap.uriOf(DocOrd(docOrd)), span),
            role,
            fromOverlay = false
          )

  private def rawFallback(
      cursor: CursorSymbol,
      includeDeclaration: Boolean
  ): Vector[ReferenceHit] =
    orchestrator
      .rawDocOccurrences(cursor.uri, cursor.semanticSymbol)
      .collect {
        case (span, role) if includeDeclaration || role == Role.Reference =>
          ReferenceHit(Loc(cursor.uri, span), role, fromOverlay = false)
      }

  private def dedupeAndSort(hits: Vector[ReferenceHit]): Vector[ReferenceHit] =
    val seen = mutable.LinkedHashMap.empty[Loc, ReferenceHit]
    for h <- hits do if !seen.contains(h.loc) then seen.update(h.loc, h)
    seen.values.toVector.sortBy(h =>
      (
        h.loc.uri,
        h.loc.span.startLine,
        h.loc.span.startChar,
        h.loc.span.endLine,
        h.loc.span.endChar
      )
    )
