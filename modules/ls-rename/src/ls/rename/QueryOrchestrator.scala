package ls.rename

import java.nio.charset.StandardCharsets
import java.nio.file.{Files, Path}

import ls.index.*
import ls.postings.{PostingsSnapshot, SnapshotManager}
import ls.rename.ingest.{IngestPipeline, IngestReport, WorkspaceTargets}
import ls.semanticdb.{Md5, Normalizer, SemanticdbParser}
import ls.sqlite.{DocumentRow, MetaStore, TargetRow, WorkspaceSymbolHit}

/** Where a cursor resolution came from (plan 10 query paths). */
enum ResolutionSource:
  case Snapshot // IndexPath: mmap doc postings
  case RawSemanticdb // RawSemanticDBPath: parsed .semanticdb, needs reindex
  case Overlay // PCPath: dirty buffer

/** Resolved symbol under the cursor. `semanticSymbol` is the raw SemanticDB
  * string; local symbols additionally carry the persistent doc id that
  * qualifies them ([[SymbolEncoding]]). `needsReindex` is set when the
  * answer had to bypass the snapshot (RawSemanticDBPath) — the caller should
  * schedule a re-ingest.
  */
final case class CursorSymbol(
    uri: String,
    semanticSymbol: String,
    localDocId: Option[Long],
    span: Span,
    role: Role,
    source: ResolutionSource,
    needsReindex: Boolean,
    pcOnly: Boolean
):
  def isLocal: Boolean = localDocId.isDefined
  def encodedSymbol: String = SymbolEncoding.encode(semanticSymbol, localDocId)

/** Owns the stores and implements the plan-10 query paths. All uris are
  * SemanticDB uris (sourceroot-relative, forward slashes); the LSP core
  * converts to/from `file://` URIs.
  *
  * symbol-at-cursor resolution order (plan 12.1):
  *   1. dirty file: the overlay must answer, otherwise the query degrades
  *      ([[ls.index.LsError.StaleIndex]]) — the index must not pretend to
  *      know a buffer it has not seen;
  *   2. clean file whose on-disk source still matches the indexed md5:
  *      snapshot doc postings;
  *   3. stale or unindexed file: RawSemanticDBPath — parse the document's
  *      `.semanticdb` directly, md5-validate against the current source,
  *      serve from it and flag `needsReindex`.
  */
final class QueryOrchestrator(
    val meta: MetaStore,
    val snapshots: SnapshotManager,
    val pipeline: IngestPipeline,
    val overlay: DirtyBufferOverlay = NoopOverlay
):
  @volatile private var currentWorkspace: Option[WorkspaceTargets] = None

  /** Runs a full-generation ingest and remembers the workspace description
    * for target-graph pruning.
    */
  def ingest(workspace: WorkspaceTargets): IngestReport =
    val report = pipeline.ingest(workspace)
    currentWorkspace = Some(workspace)
    report

  def workspace: Option[WorkspaceTargets] = currentWorkspace

  // --- workspace symbol (BestEffort, plan 11) ---

  def workspaceSymbol(query: String, limit: Int = 200): Vector[WorkspaceSymbolHit] =
    meta.workspaceSymbolSearch(query, limit)

  // --- symbol at cursor ---

  def symbolAtCursor(uri: String, line: Int, character: Int): CursorSymbol =
    if overlay.isDirty(uri) then
      overlay.symbolAt(uri, line, character) match
        case Some(hit) =>
          CursorSymbol(
            uri = uri,
            semanticSymbol = hit.semanticSymbol,
            localDocId = None,
            span = hit.span,
            role = hit.role,
            source = ResolutionSource.Overlay,
            needsReindex = false,
            pcOnly = hit.pcOnly
          )
        case None =>
          // Dirty buffer and PC cannot answer: degrade, never guess from a
          // snapshot that has not seen the buffer (plan 12.1).
          throw LsException(LsError.StaleIndex(uri))
    else
      val fromSnapshot: Option[CursorSymbol] =
        snapshots.withCurrent(snap => snapshotCursor(snap, uri, line, character)).flatten
      fromSnapshot.getOrElse(rawSemanticdbCursor(uri, line, character))

  /** Snapshot path: only when the doc is present and its on-disk source
    * still matches the ingested md5. Throws NoSymbolAtCursor when the doc is
    * fresh but no occurrence covers the position.
    */
  private def snapshotCursor(
      snap: PostingsSnapshot,
      uri: String,
      line: Int,
      character: Int
  ): Option[CursorSymbol] =
    snap.docOrdOf(uri).flatMap { docOrd =>
      val docId = snap.reader.docIdOf(docOrd.ord)
      val row = meta.documentsByUri(uri).find(_.docId.value == docId)
      val fresh = row.exists(r => sourceIsFresh(r))
      if !fresh then None
      else
        snap.symbolAt(docOrd, line, character) match
          case Some(hit) =>
            val encoded = snap.semanticSymbolOf(hit.symbolOrd)
            val (raw, localDoc) = SymbolEncoding.decode(encoded)
            Some(
              CursorSymbol(
                uri = uri,
                semanticSymbol = raw,
                localDocId = localDoc,
                span = hit.span,
                role = hit.role,
                source = ResolutionSource.Snapshot,
                needsReindex = false,
                pcOnly = false
              )
            )
          case None =>
            throw LsException(LsError.NoSymbolAtCursor(uri, line, character))
    }

  /** RawSemanticDBPath: parse the `.semanticdb` for the doc, validate md5
    * against the source on disk, serve from it, flag needsReindex.
    */
  private def rawSemanticdbCursor(uri: String, line: Int, character: Int): CursorSymbol =
    val row = primaryRowOf(uri).getOrElse(throw LsException(LsError.NotIndexed(uri)))
    val doc = rawNormalizedDoc(row, uri)
    occurrenceAt(doc.occurrences, line, character) match
      case Some(occ) =>
        CursorSymbol(
          uri = uri,
          semanticSymbol = occ.key.semanticSymbol,
          localDocId = occ.key.localDoc.map(_.value),
          span = occ.span,
          role = occ.role,
          source = ResolutionSource.RawSemanticdb,
          needsReindex = true,
          pcOnly = false
        )
      case None =>
        throw LsException(LsError.NoSymbolAtCursor(uri, line, character))

  /** All same-document occurrences of `semanticSymbol` served straight from
    * the raw `.semanticdb` of `uri` — the RawSemanticDBPath fallback for
    * references/highlights when a fresh symbol is not in the snapshot yet.
    */
  def rawDocOccurrences(uri: String, semanticSymbol: String): Vector[(Span, Role)] =
    primaryRowOf(uri) match
      case None => Vector.empty
      case Some(row) =>
        val doc = rawNormalizedDoc(row, uri)
        doc.occurrences
          .filter(_.key.semanticSymbol == semanticSymbol)
          .map(o => (o.span, o.role))

  private def rawNormalizedDoc(row: DocumentRow, uri: String): NormalizedDocument =
    val sdbPath = Path.of(row.semanticdbPath)
    val sdb =
      try
        SemanticdbParser
          .parseFile(sdbPath)
          .documents
          .find(_.uri == uri)
          .getOrElse(throw LsException(LsError.StaleIndex(uri)))
      catch
        case _: java.io.IOException => throw LsException(LsError.StaleIndex(uri))
    val text = sourceTextOf(row).getOrElse(throw LsException(LsError.StaleIndex(uri)))
    if !Md5.validate(text, sdb).isFresh then throw LsException(LsError.StaleIndex(uri))
    Normalizer.normalize(sdb, row.docId)

  /** Exact occurrence covering the position: smallest packed span wins, then
    * earliest start — the same rule as the segment reader.
    */
  private def occurrenceAt(
      occurrences: Vector[Occurrence],
      line: Int,
      character: Int
  ): Option[Occurrence] =
    val q = Span.pack(line, character)
    var best: Option[Occurrence] = None
    var bestSize = Int.MaxValue
    var bestStart = Int.MaxValue
    for o <- occurrences do
      val ps = Span.pack(o.span.startLine, o.span.startChar)
      val pe = Span.pack(o.span.endLine, o.span.endChar)
      if ps <= q && q <= pe then
        val size = pe - ps
        if size < bestSize || (size == bestSize && ps < bestStart) then
          best = Some(o)
          bestSize = size
          bestStart = ps
    best

  // --- document facts / freshness plumbing shared with the engines ---

  private def targetRows: Map[Long, TargetRow] =
    meta.allTargets().iterator.map(t => t.targetId.value -> t).toMap

  def targetRowById(id: TargetId): Option[TargetRow] = targetRows.get(id.value)

  /** The document row owning the postings for `uri` (first row by doc id,
    * matching ingest's primary-target claim).
    */
  def primaryRowOf(uri: String): Option[DocumentRow] =
    meta.documentsByUri(uri).headOption

  def absoluteSourcePath(uri: String): Option[Path] =
    primaryRowOf(uri).flatMap(sourcePathOf)

  private def sourcePathOf(row: DocumentRow): Option[Path] =
    targetRows.get(row.targetId.value).map(t => Path.of(t.sourceroot).resolve(row.uri))

  def sourceTextOf(row: DocumentRow): Option[String] =
    sourcePathOf(row).filter(Files.isRegularFile(_)).map { p =>
      new String(Files.readAllBytes(p), StandardCharsets.UTF_8)
    }

  /** True when the on-disk source still matches the md5 recorded at ingest
    * (which is the SemanticDB TextDocument md5).
    */
  def sourceIsFresh(row: DocumentRow): Boolean =
    sourceTextOf(row).exists(text => Md5.validate(text, row.md5).isFresh)

  // --- target graph pruning (plan 12.2) ---

  /** Exact allowed-target set for references of `sym`: the reverse dependency
    * closure of its definition target, mapped to snapshot ordinals. Falls
    * back to all targets when the definition target is unknown.
    */
  def allowedTargetsFor(snap: PostingsSnapshot, sym: SymbolOrd): TargetBitset =
    val all = TargetBitset.all(snap.targetCount)
    snap.definitionTargetOf(sym) match
      case None => all
      case Some(defOrd) =>
        val rows = targetRows
        val ws = currentWorkspace
        val defBsp = rows.get(snap.targetIdOf(defOrd).value).map(_.bspId)
        (ws, defBsp) match
          case (Some(workspace), Some(bspId)) =>
            val closure = workspace.reverseDependencyClosure(bspId)
            if closure.isEmpty then all
            else
              val bspToId = rows.valuesIterator.map(t => t.bspId -> t.targetId).toMap
              val ords = closure.toVector.flatMap(b =>
                bspToId.get(b).flatMap(snap.targetOrdOfId).map(_.ord)
              )
              TargetBitset.of(snap.targetCount, (ords :+ defOrd.ord).distinct)
          case _ => all

  /** The bsp id of the target defining `sym` in the current snapshot. */
  def definitionBspOf(snap: PostingsSnapshot, sym: SymbolOrd): Option[String] =
    snap.definitionTargetOf(sym).flatMap { defOrd =>
      targetRows.get(snap.targetIdOf(defOrd).value).map(_.bspId)
    }
