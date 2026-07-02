package ls.rename

import java.nio.file.Path

import scala.collection.mutable

import ls.bsp.BspCompileOutcome
import ls.index.*
import ls.postings.PostingsSnapshot
import ls.semanticdb.{SdbRole, SemanticdbParser}
import ls.sqlite.DocumentRow

/** Compile hook: the LSP core passes the real BSP session
  * (`buildTarget/compile` over the rename domain); tests stub it.
  */
trait CompileService:
  def compile(targets: Seq[String]): BspCompileOutcome

/** One text edit inside a document, span in SemanticDB/LSP coordinates
  * (zero-based, end-exclusive character).
  */
final case class TextEditSpan(span: Span, newText: String)

/** The rename result: edits grouped by SemanticDB uri (sourceroot-relative;
  * the LSP core converts to file:// URIs and LSP WorkspaceEdit types).
  */
final case class WorkspaceEditPlan(
    edits: Map[String, Vector[TextEditSpan]],
    occurrenceCount: Int
)

/** Cross-file rename with ConsistencyLevel.FreshRequired (plan 13).
  *
  * `rename` drives, in order:
  *   1. dirty-buffer / PC-only check (PC-only symbols are rejected, unsaved
  *      buffers are rejected — rename only trusts fresh on-disk SemanticDB);
  *   2. new-name validation (plain identifier or backtick-quoted);
  *   3. prepareRename pre-checks on the current state (occurrence exists,
  *      the cursor document is editable);
  *   4. compile-before-rename over the definition target's reverse
  *      dependency closure;
  *   5. full fresh ingest (new snapshot);
  *   6. re-resolution on the FRESH snapshot, rename group -> rename profile,
  *      rejection when `unsafeReasonMask != 0`;
  *   7. editable rename postings scan;
  *   8. shared-source consistency check for every edited uri that belongs
  *      to more than one target;
  *   9. md5 re-validation of every edited document against the source on
  *      disk, immediately before emitting the edit plan.
  */
final class RenameEngine(orchestrator: QueryOrchestrator, compiler: CompileService):

  /** Validates that a rename can start at this position and returns the span
    * of the symbol occurrence under the cursor.
    */
  def prepareRename(uri: String, line: Int, character: Int): Span =
    if orchestrator.overlay.isDirty(uri) then
      orchestrator.overlay.symbolAt(uri, line, character) match
        case Some(hit) if hit.pcOnly => throw LsException(LsError.PcOnlySymbol())
        case Some(hit) => hit.span
        case None => throw LsException(LsError.StaleIndex(uri))
    else
      val cursor = orchestrator.symbolAtCursor(uri, line, character)
      requireEditableCursorDoc(uri)
      cursor.span

  def rename(uri: String, line: Int, character: Int, newName: String): WorkspaceEditPlan =
    val workspace = orchestrator.workspace.getOrElse(
      throw LsException(LsError.NotIndexed(uri))
    )

    // 1. dirty buffer / PC-only
    if orchestrator.overlay.isDirty(uri) then
      orchestrator.overlay.symbolAt(uri, line, character) match
        case Some(hit) if hit.pcOnly => throw LsException(LsError.PcOnlySymbol())
        case _ =>
          throw LsException(
            LsError.RenameRejected(
              List(s"$uri has unsaved changes; save the file before renaming")
            )
          )

    // 2. new name validation
    val newText = ScalaIdentifiers.encode(newName) match
      case Right(t) => t
      case Left(msg) => throw LsException(LsError.RenameRejected(List(msg)))

    // 3. prepareRename pre-checks on current state
    val pre = orchestrator.symbolAtCursor(uri, line, character)
    if pre.pcOnly then throw LsException(LsError.PcOnlySymbol())
    requireEditableCursorDoc(uri)

    // 4. compile-before-rename over the exact affected domain
    val defBsp = currentDefinitionBsp(pre).orElse(cursorDocBsp(uri))
    val domain = defBsp match
      case Some(b) => workspace.reverseDependencyClosure(b).toVector.sorted
      case None => workspace.targets.map(_.bspId)
    compiler.compile(domain) match
      case BspCompileOutcome.Failed(_, _) =>
        throw LsException(LsError.CompileFailed(defBsp.getOrElse(domain.mkString(", "))))
      case BspCompileOutcome.Ok(_) => ()

    // 5. fresh ingest -> fresh snapshot (FreshRequired)
    orchestrator.ingest(workspace)

    // 6. resolve on the FRESH snapshot
    val cursor = orchestrator.symbolAtCursor(uri, line, character)
    if cursor.source != ResolutionSource.Snapshot then
      throw LsException(LsError.StaleIndex(uri))

    val rawEdits: Vector[(String, Span, Int)] =
      orchestrator.snapshots
        .withCurrent(snap => collectEdits(snap, cursor))
        .getOrElse(throw LsException(LsError.NotIndexed(uri)))

    if rawEdits.isEmpty then
      throw LsException(
        LsError.RenameRejected(List("rename found no editable occurrences"))
      )
    if rawEdits.forall(e => OccFlags.has(e._3, OccFlags.Synthetic)) then
      throw LsException(
        LsError.RenameRejected(UnsafeReason.explain(UnsafeReason.SyntheticOnly))
      )

    val editsByUri: Map[String, Vector[Span]] =
      rawEdits
        .groupBy(_._1)
        .view
        .mapValues(
          _.map(_._2).distinct
            .sortBy(s => (s.startLine, s.startChar, s.endLine, s.endChar))
        )
        .toMap

    // 8. shared-source consistency (plan 13.1): every target compiling an
    // edited uri must see the same symbols at every edit span.
    for (editedUri, spans) <- editsByUri do
      checkSharedSourceConsistency(editedUri, spans)

    // 9. md5 re-validation of every edited doc right before emitting
    for editedUri <- editsByUri.keys do
      val row = orchestrator
        .primaryRowOf(editedUri)
        .getOrElse(throw LsException(LsError.NotIndexed(editedUri)))
      if !orchestrator.sourceIsFresh(row) then
        throw LsException(LsError.StaleIndex(editedUri))

    WorkspaceEditPlan(
      edits = editsByUri.view.mapValues(_.map(TextEditSpan(_, newText))).toMap,
      occurrenceCount = editsByUri.valuesIterator.map(_.length).sum
    )

  // --- steps 6-7 on the fresh snapshot ---

  private def collectEdits(
      snap: PostingsSnapshot,
      cursor: CursorSymbol
  ): Vector[(String, Span, Int)] =
    val ord = snap
      .symbolOrdOf(cursor.encodedSymbol)
      .getOrElse(throw LsException(LsError.StaleIndex(cursor.uri)))
    val group = snap
      .renameGroupOf(ord)
      .getOrElse(
        throw LsException(LsError.RenameRejected(List("symbol has no rename group")))
      )
    val profile = snap.renameProfileOf(group)
    if profile.unsafeReasonMask != 0L then
      throw LsException(
        LsError.RenameRejected(UnsafeReason.explain(profile.unsafeReasonMask))
      )
    val out = Vector.newBuilder[(String, Span, Int)]
    snap.scanRenameEdits(
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
          out += ((
            snap.uriOf(DocOrd(docOrd)),
            Span(
              Span.unpackLine(packedStart),
              Span.unpackChar(packedStart),
              Span.unpackLine(packedEnd),
              Span.unpackChar(packedEnd)
            ),
            flags
          ))
    )
    out.result()

  // --- pre-checks and helpers ---

  private def requireEditableCursorDoc(uri: String): Unit =
    val facts =
      for
        workspace <- orchestrator.workspace
        row <- orchestrator.primaryRowOf(uri)
        target <- orchestrator.targetRowById(row.targetId)
      yield workspace.factsFor(target.bspId, uri)
    facts.foreach { f =>
      if !f.editable then
        var mask = 0L
        if f.generated then mask |= UnsafeReason.GeneratedOccurrence
        if f.readonly then mask |= UnsafeReason.ReadonlyOccurrence
        if f.isDependencySource then mask |= UnsafeReason.DependencySource
        throw LsException(LsError.RenameRejected(UnsafeReason.explain(mask)))
    }

  private def currentDefinitionBsp(cursor: CursorSymbol): Option[String] =
    orchestrator.snapshots
      .withCurrent { snap =>
        snap
          .symbolOrdOf(cursor.encodedSymbol)
          .flatMap(ord => orchestrator.definitionBspOf(snap, ord))
      }
      .flatten

  private def cursorDocBsp(uri: String): Option[String] =
    orchestrator
      .primaryRowOf(uri)
      .flatMap(row => orchestrator.targetRowById(row.targetId))
      .map(_.bspId)

  /** All targets that compile `uri` must agree on the symbols at every edit
    * span, otherwise the same textual edit would rename different rename
    * groups in different targets (plan 13.1 shared-source rule).
    */
  private def checkSharedSourceConsistency(uri: String, spans: Vector[Span]): Unit =
    val rows = orchestrator.meta.documentsByUri(uri)
    if rows.length <= 1 then return
    val perTarget: Vector[Map[Span, Set[String]]] = rows.map(symbolsAtSpans(_, uri, spans))
    val reference = perTarget.head
    val disagree = perTarget.exists(m => spans.exists(s => m.get(s) != reference.get(s)))
    if disagree || spans.exists(s => reference.get(s).forall(_.isEmpty)) then
      throw LsException(
        LsError.RenameRejected(UnsafeReason.explain(UnsafeReason.SharedSourceDisagreement))
      )

  private def symbolsAtSpans(
      row: DocumentRow,
      uri: String,
      spans: Vector[Span]
  ): Map[Span, Set[String]] =
    val doc =
      try
        SemanticdbParser
          .parseFile(Path.of(row.semanticdbPath))
          .documents
          .find(_.uri == uri)
      catch case _: java.io.IOException => None
    doc match
      case None => Map.empty
      case Some(d) =>
        val wanted = spans.toSet
        val acc = mutable.HashMap.empty[Span, Set[String]]
        for
          o <- d.occurrences
          r <- o.range
          if o.roleCode == SdbRole.Reference || o.roleCode == SdbRole.Definition
        do
          val span = Span(r.startLine, r.startCharacter, r.endLine, r.endCharacter)
          if wanted.contains(span) then
            acc.updateWith(span)(prev => Some(prev.getOrElse(Set.empty) + o.symbol))
        acc.toMap
