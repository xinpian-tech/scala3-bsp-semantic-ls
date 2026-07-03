package ls.rename.ingest

import java.nio.charset.StandardCharsets
import java.nio.file.{Files, Path}

import scala.collection.mutable

import ls.index.*
import ls.postings.{
  DocOcc,
  GroupOcc,
  SegmentData,
  SegmentDoc,
  SegmentReader,
  SegmentSymbol,
  SegmentWriter,
  SnapshotManager
}
import ls.rename.SymbolEncoding
import ls.semanticdb.{
  DocFacts,
  Md5,
  Normalizer,
  SdbDocument,
  SemanticBatch,
  SemanticdbLocator,
  SemanticdbParser,
  SymbolStrings
}
import ls.sqlite.{MetaStore, SymbolInternRow, SymbolMetadataRow, WorkspaceSymbolRow}

/** Result of one full-generation publish. */
final case class IngestReport(
    segmentId: Long,
    manifestSegmentId: Long,
    docsIndexed: Int,
    docsShared: Int,
    docsStale: Int,
    docsSkipped: Int,
    symbolCount: Int,
    refGroupCount: Int,
    renameGroupCount: Int,
    staleUris: Vector[String],
    skippedUris: Vector[String],
    durationMs: Long
)

/** Full-generation SemanticDB ingest (plan 9.2, adapted to the v1
  * one-segment-per-generation model of docs/index-format.md).
  *
  * One `ingest` call re-reads the complete workspace state:
  *
  *   1. locate every `.semanticdb` file per target root and parse it;
  *   2. md5-validate each TextDocument against the source file on disk
  *      (stale docs are recorded and still indexed; docs whose source file
  *      no longer exists are skipped);
  *   3. one SQLite transaction: upsert targets/documents (epoch bumps on
  *      change), normalize, build exact alias groups
  *      ([[ls.semanticdb.SemanticBatch]]), intern symbols, write symbol
  *      metadata + workspace-symbol FTS rows, assign persistent
  *      ref/rename group ids;
  *   4. build the [[ls.postings.SegmentData]] for the whole workspace
  *      (dense ordinals), write the segment, fsync;
  *   5. one SQLite manifest transaction: insertSegment + activateSegment;
  *   6. publish the mmap snapshot.
  *
  * Re-running after edits supersedes cleanly: changed documents get a new
  * epoch, the fresh segment replaces the old one atomically, and the old
  * snapshot drains via reference counting — that is the v1 incremental
  * story.
  *
  * Shared sources (one uri compiled by several targets) are indexed once:
  * the first target in workspace order that contains the uri is the
  * *primary* and owns the postings; the other (target, uri) pairs still get
  * SQLite document rows so query-time consistency checks can find them.
  *
  * Rename postings contain only genuinely renameable occurrences: the doc
  * must be editable per [[ls.semanticdb.DocFacts]] AND the source token
  * under the occurrence span must textually match the member's renameable
  * name (so explicit `.apply` call tokens, multi-line spans and other
  * non-token sites never receive edits, while `Item(1)` sugar sites do).
  *
  * Threading: single-threaded writer, same contract as [[ls.sqlite.Db]].
  */
final class IngestPipeline(
    val meta: MetaStore,
    val manager: SnapshotManager,
    universeId: Long = 0L,
    walCheckpointThresholdBytes: Long = MetaStore.DefaultWalThresholdBytes
):

  private final case class PrimaryDoc(
      uri: String,
      docId: DocId,
      epoch: Int,
      targetOrd: Int,
      facts: DocFacts,
      sdb: SdbDocument,
      sourceText: String
  )

  def ingest(workspace: WorkspaceTargets): IngestReport =
    val t0 = System.nanoTime()

    val primaries = mutable.LinkedHashMap.empty[String, PrimaryDoc]
    var sharedCount = 0
    val staleUris = Vector.newBuilder[String]
    val skippedUris = Vector.newBuilder[String]
    var symbolCount = 0
    var refGroupCount = 0
    var renameGroupCount = 0
    var segmentData: SegmentData | Null = null
    var minEpoch = Long.MaxValue
    var maxEpoch = 1L

    val targetIds = new Array[Long](workspace.targets.length)

    // --- SQLite metadata transaction (plan 9.2 step 5) ---
    meta.db.withWriteTransaction {
      for (spec, targetOrd) <- workspace.targets.zipWithIndex do
        val targetId = meta.upsertTarget(
          bspId = spec.bspId,
          scalaVersion = spec.scalaVersion,
          classpathHash = spec.classpathHash,
          optionsHash = spec.optionsHash,
          semanticdbRoot = spec.semanticdbRoot.toString,
          sourceroot = spec.sourceroot.toString,
          active = true
        )
        targetIds(targetOrd) = targetId.value
        val locator = SemanticdbLocator(spec.semanticdbRoot)
        for
          file <- locator.listSemanticdbFiles()
          sdb <- SemanticdbParser.parseFile(file).documents
        do
          val uri = sdb.uri
          val sourcePath = spec.sourceroot.resolve(uri)
          if !Files.isRegularFile(sourcePath) then skippedUris += uri
          else
            val sourceText =
              new String(Files.readAllBytes(sourcePath), StandardCharsets.UTF_8)
            if !Md5.validate(sourceText, sdb).isFresh then staleUris += uri
            val facts = spec.docFacts(uri)
            val mtime = Files.getLastModifiedTime(file).toMillis
            val (docId, epoch) = meta.upsertDocument(
              targetId = targetId,
              uri = uri,
              semanticdbPath = file.toString,
              semanticdbMtimeMs = mtime,
              md5 = sdb.md5,
              generated = facts.generated,
              readonly = facts.readonly
            )
            if primaries.contains(uri) then sharedCount += 1
            else
              primaries.update(
                uri,
                PrimaryDoc(uri, docId, epoch.toInt, targetOrd, facts, sdb, sourceText)
              )
              minEpoch = math.min(minEpoch, epoch)
              maxEpoch = math.max(maxEpoch, epoch)

      val docs = primaries.values.toVector
      val normalized = docs.map(p => Normalizer.normalize(p.sdb, p.docId))
      val factsByUri: Map[String, DocFacts] =
        docs.iterator.map(p => p.uri -> p.facts).toMap
      val batch = SemanticBatch.assemble(normalized, factsByUri)

      // deterministic symbol universe: every key of the batch, sorted
      val keys: Vector[SymbolKey] = batch.groups.refGroupIndex.keys.toVector
        .sortBy(k => (k.semanticSymbol, k.localDoc.fold(-1L)(_.value)))
      val callerOrdOf: Map[SymbolKey, Int] = keys.iterator.zipWithIndex.toMap

      // interning (idempotent, tolerates zero-occurrence synthesized keys)
      val internRows = keys.map(k =>
        SymbolInternRow(universeId, k.semanticSymbol, k.localDoc, stableHash(k))
      )
      val internedByRow = meta.internSymbols(internRows)
      val symbolIdOf: Map[SymbolKey, SymbolId] =
        keys.iterator.zip(internRows.iterator).map((k, r) => k -> internedByRow(r)).toMap

      // persistent group ids (plan 6.1) + assignments
      val renamePostings =
        Vector.fill(batch.groups.renameGroups.length)(Vector.newBuilder[GroupOcc])
      val refPostings =
        Vector.fill(batch.groups.refGroups.length)(Vector.newBuilder[GroupOcc])
      val defPostings =
        Vector.fill(batch.groups.refGroups.length)(Vector.newBuilder[GroupOcc])

      val refGroupIds = batch.groups.refGroups.map(_ => meta.newRefGroup())
      val renameGroupIds =
        batch.renameProfiles.map(p => meta.newRenameGroup(p.unsafeReasonMask))
      meta.assignRefGroups(
        keys.iterator
          .map(k => symbolIdOf(k) -> refGroupIds(batch.groups.refGroupIndex(k)))
          .toMap
      )
      meta.assignRenameGroups(
        keys.iterator
          .map(k => symbolIdOf(k) -> renameGroupIds(batch.groups.renameGroupIndex(k)))
          .toMap
      )

      // display names (for rename token checks): SymbolInformation first
      val displayNameOf = mutable.HashMap.empty[SymbolKey, String]
      for doc <- normalized; s <- doc.symbols do
        displayNameOf.getOrElseUpdate(s.key, s.displayName)

      // per-doc metadata + FTS rows, doc postings, group postings
      val docOccs = Vector.fill(docs.length)(Vector.newBuilder[DocOcc])
      val defTargetOf = mutable.HashMap.empty[SymbolKey, Int]

      for ((p, doc), docOrd) <- docs.zip(normalized).zipWithIndex do
        val targetId = TargetId(targetIds(p.targetOrd))
        val defSpanOf: Map[SymbolKey, Span] =
          doc.occurrences.iterator
            .filter(_.role == Role.Definition)
            .map(o => o.key -> o.span)
            .toMap
        val metadataRows = doc.symbols.map { s =>
          SymbolMetadataRow(
            symbolId = symbolIdOf(s.key),
            targetId = targetId,
            displayName = s.displayName,
            ownerName = s.ownerName,
            packageName = s.packageName,
            kind = s.kind,
            properties = s.properties,
            signatureHash = None,
            span = defSpanOf.get(s.key)
          )
        }
        val wsRows = doc.symbols.collect {
          case s
              if !s.key.isLocal && defSpanOf.contains(s.key) &&
                workspaceSymbolKind(s.kind) =>
            WorkspaceSymbolRow(
              displayName = s.displayName,
              ownerName = s.ownerName,
              packageName = s.packageName,
              kind = s.kind,
              targetId = targetId,
              symbolId = symbolIdOf(s.key)
            )
        }
        meta.replaceSymbolMetadata(p.docId, metadataRows)
        meta.replaceWorkspaceSymbols(p.docId, wsRows)

        val lines = p.sourceText.linesWithSeparators.toVector
        for occ <- doc.occurrences do
          val flags = occFlags(occ, p.facts)
          docOccs(docOrd) += DocOcc(callerOrdOf(occ.key), occ.span, flags)
          val g = batch.groups.refGroupIndex(occ.key)
          val groupOcc = GroupOcc(docOrd, p.epoch, p.targetOrd, occ.span, flags)
          if occ.role == Role.Definition then
            defPostings(g) += groupOcc
            if !defTargetOf.contains(occ.key) then
              defTargetOf.update(occ.key, p.targetOrd)
          else refPostings(g) += groupOcc
          if p.facts.editable && !occ.synthetic &&
            renameTokenMatches(occ, lines, displayNameOf, batch)
          then
            val rg = batch.groups.renameGroupIndex(occ.key)
            renamePostings(rg) += GroupOcc(
              docOrd,
              p.epoch,
              p.targetOrd,
              occ.span,
              flags | OccFlags.Editable
            )

      val renameOccurrences = renamePostings.map(_.result())
      // profiles: batch truth, with the editable count aligned to the
      // postings actually materialized in this segment.
      val profiles = batch.renameProfiles.zipWithIndex.map((prof, rg) =>
        prof.copy(editableOccurrenceCount = renameOccurrences(rg).length)
      )

      val symbols = keys.map { k =>
        SegmentSymbol(
          semanticSymbol = SymbolEncoding.encode(k),
          symbolId = symbolIdOf(k).value,
          refGroupOrd = batch.groups.refGroupIndex(k),
          renameGroupOrd = batch.groups.renameGroupIndex(k),
          defTargetOrd = defTargetOf.getOrElse(k, -1)
        )
      }

      segmentData = SegmentData(
        docs = docs.map(p =>
          SegmentDoc(
            uri = p.uri,
            docId = p.docId.value,
            epoch = p.epoch,
            targetOrd = p.targetOrd,
            generated = p.facts.generated,
            readonly = p.facts.readonly
          )
        ),
        targets = targetIds.toVector,
        symbols = symbols,
        refOccurrences = refPostings.map(_.result()),
        defOccurrences = defPostings.map(_.result()),
        renameOccurrences = renameOccurrences,
        renameProfiles = profiles,
        docOccurrences = docOccs.map(_.result())
      )
      symbolCount = keys.length
      refGroupCount = batch.groups.refGroups.length
      renameGroupCount = batch.groups.renameGroups.length
    }

    // --- segment build + fsync (plan 9.2 steps 6-7) ---
    val data = segmentData.nn
    val segmentId = manager.nextSegmentId()
    val createdAtMs = System.currentTimeMillis()
    val segmentDir = SegmentWriter.write(manager.root, segmentId, data, createdAtMs)
    val reader = SegmentReader.open(segmentDir)

    // --- manifest transaction (step 8) + snapshot publish (steps 9-10) ---
    val manifestId =
      try
        meta.db.withWriteTransaction {
          val id = meta.insertSegment(
            path = segmentDir.toString,
            createdAtMs = createdAtMs,
            minEpoch = if minEpoch == Long.MaxValue then 1L else minEpoch,
            maxEpoch = maxEpoch,
            checksum = headerChecksum(segmentDir)
          )
          meta.activateSegment(id)
          id
        }
      catch
        case t: Throwable =>
          try reader.close()
          catch case c: Throwable => t.addSuppressed(c)
          throw t
    manager.publish(reader)
    // Reclaim drained superseded segment directories so a long-running server
    // does not leak one directory per re-ingest; snapshots still held by
    // readers are left for a later pass.
    manager.deleteSuperseded()
    // Keep the SQLite WAL bounded without ever blocking the writer.
    try meta.checkpoint(walCheckpointThresholdBytes)
    catch case scala.util.control.NonFatal(t) => ()

    IngestReport(
      segmentId = segmentId,
      manifestSegmentId = manifestId,
      docsIndexed = primaries.size,
      docsShared = sharedCount,
      docsStale = staleUris.result().length,
      docsSkipped = skippedUris.result().length,
      symbolCount = symbolCount,
      refGroupCount = refGroupCount,
      renameGroupCount = renameGroupCount,
      staleUris = staleUris.result(),
      skippedUris = skippedUris.result(),
      durationMs = (System.nanoTime() - t0) / 1_000_000L
    )

  // --- helpers ---

  private def occFlags(occ: Occurrence, facts: DocFacts): Int =
    var f = 0
    if occ.role == Role.Definition then f |= OccFlags.Definition
    if facts.editable then f |= OccFlags.Editable
    if facts.generated then f |= OccFlags.Generated
    if facts.readonly then f |= OccFlags.Readonly
    if occ.synthetic then f |= OccFlags.Synthetic
    f

  /** True when the source token under `occ.span` is the identifier a rename
    * would rewrite. Multi-line spans never match. The expected token is the
    * member's renameable base name:
    *   - setters `x_=` expect `x`;
    *   - constructors expect the class name;
    *   - `apply`/`unapply` merged with a companion class expect the class
    *     name (sugar sites), so explicit `.apply` tokens are excluded;
    *   - everything else expects its display name.
    * Unknown display names fall back to accepting the token.
    */
  private def renameTokenMatches(
      occ: Occurrence,
      lines: Vector[String],
      displayNameOf: mutable.HashMap[SymbolKey, String],
      batch: SemanticBatch
  ): Boolean =
    val span = occ.span
    if span.startLine != span.endLine then false
    else
      tokenAt(lines, span) match
        case None => false
        case Some(token) =>
          expectedToken(occ.key, displayNameOf, batch) match
            case Some(expected) => token == expected
            case None => true

  private def tokenAt(lines: Vector[String], span: Span): Option[String] =
    if span.startLine < 0 || span.startLine >= lines.length then None
    else
      val line = lines(span.startLine)
      if span.startChar < 0 || span.endChar > line.length || span.startChar > span.endChar
      then None
      else
        val raw = line.substring(span.startChar, span.endChar)
        val token =
          if raw.length >= 2 && raw.head == '`' && raw.last == '`' then
            raw.substring(1, raw.length - 1)
          else raw
        Some(token)

  private def expectedToken(
      key: SymbolKey,
      displayNameOf: mutable.HashMap[SymbolKey, String],
      batch: SemanticBatch
  ): Option[String] =
    if key.isLocal then displayNameOf.get(key)
    else
      SymbolStrings.splitLast(key.semanticSymbol) match
        case Some((owner, SymbolStrings.Descriptor.Method(name, _))) =>
          if name == SymbolStrings.ConstructorName then
            SymbolStrings.displayName(owner)
          else if name.length > 2 && name.endsWith("_=") then Some(name.dropRight(2))
          else if name == "apply" || name == "unapply" then
            // merged with a companion class -> only class-name sugar tokens
            // are renameable; a standalone apply/unapply renames its own name
            val classKey = SymbolStrings.companion(owner).map(SymbolKey.global)
            val sameGroup = classKey.exists(ck =>
              batch.groups.renameGroupOf(ck) == batch.groups.renameGroupOf(key)
            )
            if sameGroup then SymbolStrings.displayName(owner) else Some(name)
          else Some(name)
        case _ =>
          displayNameOf
            .get(key)
            .orElse(SymbolStrings.displayName(key.semanticSymbol))

  private def workspaceSymbolKind(kind: SymKind): Boolean = kind match
    case SymKind.Class | SymKind.Trait | SymKind.Interface | SymKind.Object |
        SymKind.PackageObject | SymKind.Method | SymKind.Macro | SymKind.Type |
        SymKind.Field =>
      true
    case _ => false

  /** Stable FNV-1a 64 over the symbol string and local doc id. */
  private def stableHash(key: SymbolKey): Long =
    var h = 0xcbf29ce484222325L
    def mix(b: Int): Unit =
      h ^= (b & 0xff)
      h *= 0x100000001b3L
    for b <- key.semanticSymbol.getBytes(StandardCharsets.UTF_8) do mix(b)
    key.localDoc.foreach { d =>
      var v = d.value
      var i = 0
      while i < 8 do
        mix((v & 0xff).toInt)
        v >>>= 8
        i += 1
    }
    h

  /** The header.bin trailing checksum field of a written segment. */
  private def headerChecksum(segmentDir: Path): Long =
    val bytes = Files.readAllBytes(segmentDir.resolve("header.bin"))
    val bb = java.nio.ByteBuffer.wrap(bytes).order(java.nio.ByteOrder.LITTLE_ENDIAN)
    bb.getLong(56)
