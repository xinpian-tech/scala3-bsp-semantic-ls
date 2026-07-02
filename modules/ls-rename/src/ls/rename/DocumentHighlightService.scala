package ls.rename

import scala.collection.mutable

import ls.index.*
import ls.postings.PostingsSnapshot

enum HighlightKind:
  case Read, Write

final case class DocHighlight(span: Span, kind: HighlightKind)

/** textDocument/documentHighlight: all same-document occurrences of the
  * symbol at the cursor, split read/write by occurrence role (Definition ->
  * Write, Reference -> Read).
  *
  * Doc postings identify the cursor symbol (`symbolAt`); the same-symbol
  * occurrences inside the document are its ref-group postings restricted to
  * the document's ordinal (doc-postings records carry no symbol id in the
  * scan sink, so the group postings are the exact per-symbol view).
  */
final class DocumentHighlightService(orchestrator: QueryOrchestrator):

  def highlights(uri: String, line: Int, character: Int): Vector[DocHighlight] =
    val cursor = orchestrator.symbolAtCursor(uri, line, character)
    if cursor.pcOnly then throw LsException(LsError.PcOnlySymbol())

    val hits: Vector[DocHighlight] =
      orchestrator.snapshots
        .withCurrent(snap => snapshotHighlights(snap, cursor))
        .flatten
        .getOrElse {
          if cursor.source == ResolutionSource.RawSemanticdb then
            orchestrator
              .rawDocOccurrences(cursor.uri, cursor.semanticSymbol)
              .map((span, role) => DocHighlight(span, kindOf(role)))
          else throw LsException(LsError.NotIndexed(uri))
        }

    val seen = mutable.LinkedHashSet.empty[DocHighlight]
    hits.foreach(seen.add)
    seen.toVector.sortBy(h =>
      (h.span.startLine, h.span.startChar, h.span.endLine, h.span.endChar)
    )

  private def snapshotHighlights(
      snap: PostingsSnapshot,
      cursor: CursorSymbol
  ): Option[Vector[DocHighlight]] =
    for
      docOrd <- snap.docOrdOf(cursor.uri)
      ord <- snap.symbolOrdOf(cursor.encodedSymbol)
      group <- snap.refGroupOf(ord)
    yield
      val out = Vector.newBuilder[DocHighlight]
      val sink = new OccurrenceSink:
        override def accept(
            dOrd: Int,
            targetOrd: Int,
            docEpoch: Int,
            packedStart: Int,
            packedEnd: Int,
            flags: Int
        ): Unit =
          if dOrd == docOrd.ord then
            val span = Span(
              Span.unpackLine(packedStart),
              Span.unpackChar(packedStart),
              Span.unpackLine(packedEnd),
              Span.unpackChar(packedEnd)
            )
            val kind =
              if OccFlags.has(flags, OccFlags.Definition) then HighlightKind.Write
              else HighlightKind.Read
            out += DocHighlight(span, kind)
      snap.scanReferences(group, TargetBitset.all(snap.targetCount), sink)
      snap.scanDefinitions(group, sink)
      out.result()

  private def kindOf(role: Role): HighlightKind = role match
    case Role.Definition => HighlightKind.Write
    case Role.Reference => HighlightKind.Read
