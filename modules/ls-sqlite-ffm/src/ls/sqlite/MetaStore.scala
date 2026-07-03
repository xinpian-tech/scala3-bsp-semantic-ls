package ls.sqlite

import java.nio.file.Path

import ls.index.{DocId, RefGroupId, RenameGroupId, Span, SymKind, SymbolId, TargetId}

/** One symbol-interning request: SemanticDB global symbols carry no doc,
  * local symbols carry the document they belong to (plan section 6.1).
  */
final case class SymbolInternRow(
    universeId: Long,
    semanticSymbol: String,
    localDocId: Option[DocId],
    stableHash: Long
)

final case class TargetRow(
    targetId: TargetId,
    bspId: String,
    scalaVersion: String,
    classpathHash: String,
    optionsHash: String,
    semanticdbRoot: String,
    sourceroot: String,
    active: Boolean
)

final case class DocumentRow(
    docId: DocId,
    targetId: TargetId,
    uri: String,
    semanticdbPath: String,
    semanticdbMtimeMs: Long,
    md5: String,
    epoch: Long,
    active: Boolean,
    generated: Boolean,
    readonly: Boolean
)

final case class SymbolMetadataRow(
    symbolId: SymbolId,
    targetId: TargetId,
    displayName: String,
    ownerName: Option[String],
    packageName: Option[String],
    kind: SymKind,
    properties: Int,
    signatureHash: Option[Long],
    span: Option[Span]
)

final case class WorkspaceSymbolRow(
    displayName: String,
    ownerName: Option[String],
    packageName: Option[String],
    kind: SymKind,
    targetId: TargetId,
    symbolId: SymbolId
)

final case class WorkspaceSymbolHit(
    symbolId: SymbolId,
    displayName: String,
    ownerName: Option[String],
    packageName: Option[String],
    kind: SymKind,
    docId: DocId,
    targetId: TargetId
)

final case class SegmentRow(
    segmentId: Long,
    path: String,
    createdAtMs: Long,
    minEpoch: Long,
    maxEpoch: Long,
    active: Boolean,
    checksum: Long
)

/** Typed DAO over schema v1, designed for batch SemanticDB ingest.
  *
  * Inherits the Db threading contract: single-threaded-writer. Every method
  * is transactional on its own; methods compose into one larger transaction
  * when called inside `db.withWriteTransaction` (nested calls join it).
  *
  * Display/owner/package names returned by [[workspaceSymbolSearch]] come
  * from symbol_metadata: the FTS table is contentless (schema section 7.6)
  * and cannot return column values, so ingest must write
  * [[replaceSymbolMetadata]] and [[replaceWorkspaceSymbols]] for the same
  * document (normally in the same transaction).
  */
final class MetaStore(val db: Db) extends AutoCloseable:

  def close(): Unit = db.close()

  /** Size of the WAL sidecar file in bytes (0 when absent). */
  def walSizeBytes: Long = db.walFileSizeBytes

  /** Scheduled WAL checkpoint, run on the single writer thread after an ingest
    * publish. Non-blocking: PASSIVE always, then TRUNCATE only when the WAL is
    * fully checkpointed and larger than `walThresholdBytes`.
    */
  def checkpoint(walThresholdBytes: Long = MetaStore.DefaultWalThresholdBytes): CheckpointOutcome =
    db.smartCheckpoint(walThresholdBytes)

  // --- targets ---

  def upsertTarget(
      bspId: String,
      scalaVersion: String,
      classpathHash: String,
      optionsHash: String,
      semanticdbRoot: String,
      sourceroot: String,
      active: Boolean
  ): TargetId =
    db.withWriteTransaction {
      db.prepare(
        """INSERT INTO targets
          |  (bsp_id, scala_version, classpath_hash, options_hash, semanticdb_root, sourceroot, active)
          |VALUES (?, ?, ?, ?, ?, ?, ?)
          |ON CONFLICT(bsp_id) DO UPDATE SET
          |  scala_version   = excluded.scala_version,
          |  classpath_hash  = excluded.classpath_hash,
          |  options_hash    = excluded.options_hash,
          |  semanticdb_root = excluded.semanticdb_root,
          |  sourceroot      = excluded.sourceroot,
          |  active          = excluded.active
          |RETURNING target_id""".stripMargin
      )
        .bindText(1, bspId)
        .bindText(2, scalaVersion)
        .bindText(3, classpathHash)
        .bindText(4, optionsHash)
        .bindText(5, semanticdbRoot)
        .bindText(6, sourceroot)
        .bindBool(7, active)
        .queryOne(st => TargetId(st.columnLong(0)))
        .getOrElse(throw IllegalStateException("upsertTarget returned no row"))
    }

  def targetByBspId(bspId: String): Option[TargetRow] =
    db.prepare(
      """SELECT target_id, bsp_id, scala_version, classpath_hash, options_hash,
        |       semanticdb_root, sourceroot, active
        |FROM targets WHERE bsp_id = ?""".stripMargin
    )
      .bindText(1, bspId)
      .queryOne(readTarget)

  def allTargets(): Vector[TargetRow] =
    db.prepare(
      """SELECT target_id, bsp_id, scala_version, classpath_hash, options_hash,
        |       semanticdb_root, sourceroot, active
        |FROM targets ORDER BY target_id""".stripMargin
    ).queryAll(readTarget)

  private def readTarget(st: Statement): TargetRow =
    TargetRow(
      targetId = TargetId(st.columnLong(0)),
      bspId = st.columnText(1),
      scalaVersion = st.columnText(2),
      classpathHash = st.columnText(3),
      optionsHash = st.columnText(4),
      semanticdbRoot = st.columnText(5),
      sourceroot = st.columnText(6),
      active = st.columnBool(7)
    )

  // --- documents ---

  /** Inserts or updates a document row. The epoch starts at 1 on first insert
    * and increments exactly when md5 or mtime changed; unchanged re-ingests
    * keep the current epoch. Returns the persistent doc id and the epoch now
    * in effect.
    */
  def upsertDocument(
      targetId: TargetId,
      uri: String,
      semanticdbPath: String,
      semanticdbMtimeMs: Long,
      md5: String,
      generated: Boolean,
      readonly: Boolean
  ): (DocId, Long) =
    db.withWriteTransaction {
      val existing = db
        .prepare(
          "SELECT doc_id, semanticdb_mtime_ms, md5, epoch FROM documents WHERE target_id = ? AND uri = ?"
        )
        .bindLong(1, targetId.value)
        .bindText(2, uri)
        .queryOne(st => (st.columnLong(0), st.columnLong(1), st.columnText(2), st.columnLong(3)))
      existing match
        case None =>
          db.prepare(
            """INSERT INTO documents
              |  (target_id, uri, semanticdb_path, semanticdb_mtime_ms, md5, epoch, active, generated, readonly)
              |VALUES (?, ?, ?, ?, ?, 1, 1, ?, ?)""".stripMargin
          )
            .bindLong(1, targetId.value)
            .bindText(2, uri)
            .bindText(3, semanticdbPath)
            .bindLong(4, semanticdbMtimeMs)
            .bindText(5, md5)
            .bindBool(6, generated)
            .bindBool(7, readonly)
            .run()
          (DocId(db.lastInsertRowid), 1L)
        case Some((docId, oldMtime, oldMd5, oldEpoch)) =>
          val changed = oldMtime != semanticdbMtimeMs || oldMd5 != md5
          val epoch = if changed then oldEpoch + 1 else oldEpoch
          db.prepare(
            """UPDATE documents SET
              |  semanticdb_path = ?, semanticdb_mtime_ms = ?, md5 = ?, epoch = ?,
              |  active = 1, generated = ?, readonly = ?
              |WHERE doc_id = ?""".stripMargin
          )
            .bindText(1, semanticdbPath)
            .bindLong(2, semanticdbMtimeMs)
            .bindText(3, md5)
            .bindLong(4, epoch)
            .bindBool(5, generated)
            .bindBool(6, readonly)
            .bindLong(7, docId)
            .run()
          (DocId(docId), epoch)
    }

  def document(targetId: TargetId, uri: String): Option[DocumentRow] =
    db.prepare(
      """SELECT doc_id, target_id, uri, semanticdb_path, semanticdb_mtime_ms, md5,
        |       epoch, active, generated, readonly
        |FROM documents WHERE target_id = ? AND uri = ?""".stripMargin
    )
      .bindLong(1, targetId.value)
      .bindText(2, uri)
      .queryOne(readDocument)

  def documentsByUri(uri: String): Vector[DocumentRow] =
    db.prepare(
      """SELECT doc_id, target_id, uri, semanticdb_path, semanticdb_mtime_ms, md5,
        |       epoch, active, generated, readonly
        |FROM documents WHERE uri = ? ORDER BY doc_id""".stripMargin
    )
      .bindText(1, uri)
      .queryAll(readDocument)

  private def readDocument(st: Statement): DocumentRow =
    DocumentRow(
      docId = DocId(st.columnLong(0)),
      targetId = TargetId(st.columnLong(1)),
      uri = st.columnText(2),
      semanticdbPath = st.columnText(3),
      semanticdbMtimeMs = st.columnLong(4),
      md5 = st.columnText(5),
      epoch = st.columnLong(6),
      active = st.columnBool(7),
      generated = st.columnBool(8),
      readonly = st.columnBool(9)
    )

  // --- symbol interning ---

  /** Interns a batch of symbols in one transaction. Existing rows keep their
    * ids, so re-running the same batch is a no-op that returns identical ids
    * and never duplicates rows (global symbols are additionally guarded by a
    * partial unique index because SQLite UNIQUE treats NULLs as distinct).
    */
  def internSymbols(rows: Seq[SymbolInternRow]): Map[SymbolInternRow, SymbolId] =
    if rows.isEmpty then Map.empty
    else
      db.withWriteTransaction {
        val select = db.prepare(
          "SELECT symbol_id FROM symbol_intern WHERE universe_id = ? AND semantic_symbol = ? AND local_doc_id IS ?"
        )
        val insert = db.prepare(
          "INSERT INTO symbol_intern (universe_id, semantic_symbol, local_doc_id, stable_hash) VALUES (?, ?, ?, ?)"
        )
        val out = Map.newBuilder[SymbolInternRow, SymbolId]
        rows.distinct.foreach { row =>
          val existing = select
            .bindLong(1, row.universeId)
            .bindText(2, row.semanticSymbol)
            .bindLongOpt(3, row.localDocId.map(_.value))
            .queryOne(_.columnLong(0))
          val id = existing.getOrElse {
            insert
              .bindLong(1, row.universeId)
              .bindText(2, row.semanticSymbol)
              .bindLongOpt(3, row.localDocId.map(_.value))
              .bindLong(4, row.stableHash)
              .run()
            db.lastInsertRowid
          }
          out += row -> SymbolId(id)
        }
        out.result()
      }

  def symbolCount(): Long =
    db.prepare("SELECT count(*) FROM symbol_intern").queryOne(_.columnLong(0)).getOrElse(0L)

  // --- symbol metadata ---

  /** Replaces all symbol_metadata rows of a document with `rows`. */
  def replaceSymbolMetadata(docId: DocId, rows: Seq[SymbolMetadataRow]): Unit =
    db.withWriteTransaction {
      db.prepare("DELETE FROM symbol_metadata WHERE doc_id = ?").bindLong(1, docId.value).run()
      val insert = db.prepare(
        """INSERT INTO symbol_metadata
          |  (symbol_id, target_id, doc_id, display_name, owner_name, package_name,
          |   kind, properties, signature_hash, start_line, start_char, end_line, end_char)
          |VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)""".stripMargin
      )
      rows.foreach { r =>
        insert
          .bindLong(1, r.symbolId.value)
          .bindLong(2, r.targetId.value)
          .bindLong(3, docId.value)
          .bindText(4, r.displayName)
          .bindTextOpt(5, r.ownerName)
          .bindTextOpt(6, r.packageName)
          .bindInt(7, r.kind.code)
          .bindInt(8, r.properties)
          .bindLongOpt(9, r.signatureHash)
          .bindIntOpt(10, r.span.map(_.startLine))
          .bindIntOpt(11, r.span.map(_.startChar))
          .bindIntOpt(12, r.span.map(_.endLine))
          .bindIntOpt(13, r.span.map(_.endChar))
          .run()
      }
    }

  def symbolMetadataFor(docId: DocId): Vector[SymbolMetadataRow] =
    db.prepare(
      """SELECT symbol_id, target_id, display_name, owner_name, package_name,
        |       kind, properties, signature_hash, start_line, start_char, end_line, end_char
        |FROM symbol_metadata WHERE doc_id = ?
        |ORDER BY symbol_id, target_id""".stripMargin
    )
      .bindLong(1, docId.value)
      .queryAll { st =>
        val span =
          for
            sl <- st.columnIntOpt(8)
            sc <- st.columnIntOpt(9)
            el <- st.columnIntOpt(10)
            ec <- st.columnIntOpt(11)
          yield Span(sl, sc, el, ec)
        SymbolMetadataRow(
          symbolId = SymbolId(st.columnLong(0)),
          targetId = TargetId(st.columnLong(1)),
          displayName = st.columnText(2),
          ownerName = st.columnTextOpt(3),
          packageName = st.columnTextOpt(4),
          kind = SymKind.fromCode(st.columnInt(5)),
          properties = st.columnInt(6),
          signatureHash = st.columnLongOpt(7),
          span = span
        )
      }

  // --- reference / rename groups ---

  def newRefGroup(): RefGroupId =
    db.withWriteTransaction {
      db.prepare("INSERT INTO ref_groups DEFAULT VALUES").run()
      RefGroupId(db.lastInsertRowid)
    }

  def newRenameGroup(unsafeReasonMask: Long = 0L): RenameGroupId =
    db.withWriteTransaction {
      db.prepare("INSERT INTO rename_groups (unsafe_reason_mask) VALUES (?)")
        .bindLong(1, unsafeReasonMask)
        .run()
      RenameGroupId(db.lastInsertRowid)
    }

  def setRenameGroupUnsafeMask(groupId: RenameGroupId, unsafeReasonMask: Long): Unit =
    db.withWriteTransaction {
      val changed = db
        .prepare("UPDATE rename_groups SET unsafe_reason_mask = ? WHERE rename_group_id = ?")
        .bindLong(1, unsafeReasonMask)
        .bindLong(2, groupId.value)
        .run()
      if changed != 1 then
        throw IllegalArgumentException(s"rename group ${groupId.value} does not exist")
    }

  def renameGroupUnsafeMask(groupId: RenameGroupId): Option[Long] =
    db.prepare("SELECT unsafe_reason_mask FROM rename_groups WHERE rename_group_id = ?")
      .bindLong(1, groupId.value)
      .queryOne(_.columnLong(0))

  /** Assigns (or reassigns) symbols to reference groups, one transaction. */
  def assignRefGroups(assignments: Map[SymbolId, RefGroupId]): Unit =
    db.withWriteTransaction {
      val upsert = db.prepare(
        """INSERT INTO symbol_to_ref_group (symbol_id, ref_group_id) VALUES (?, ?)
          |ON CONFLICT(symbol_id) DO UPDATE SET ref_group_id = excluded.ref_group_id""".stripMargin
      )
      assignments.foreach { (sym, group) =>
        upsert.bindLong(1, sym.value).bindLong(2, group.value).run()
      }
    }

  /** Assigns (or reassigns) symbols to rename groups, one transaction. Unsafe
    * reason masks live on the group row: set them via [[newRenameGroup]] /
    * [[setRenameGroupUnsafeMask]].
    */
  def assignRenameGroups(assignments: Map[SymbolId, RenameGroupId]): Unit =
    db.withWriteTransaction {
      val upsert = db.prepare(
        """INSERT INTO symbol_to_rename_group (symbol_id, rename_group_id) VALUES (?, ?)
          |ON CONFLICT(symbol_id) DO UPDATE SET rename_group_id = excluded.rename_group_id""".stripMargin
      )
      assignments.foreach { (sym, group) =>
        upsert.bindLong(1, sym.value).bindLong(2, group.value).run()
      }
    }

  def refGroupOf(symbolId: SymbolId): Option[RefGroupId] =
    db.prepare("SELECT ref_group_id FROM symbol_to_ref_group WHERE symbol_id = ?")
      .bindLong(1, symbolId.value)
      .queryOne(st => RefGroupId(st.columnLong(0)))

  def renameGroupOf(symbolId: SymbolId): Option[RenameGroupId] =
    db.prepare("SELECT rename_group_id FROM symbol_to_rename_group WHERE symbol_id = ?")
      .bindLong(1, symbolId.value)
      .queryOne(st => RenameGroupId(st.columnLong(0)))

  // --- workspace symbols (FTS) ---

  /** Replaces the workspace-symbol rows of a document, keeping the contentless
    * FTS index and workspace_symbol_rows in sync: old FTS entries are deleted
    * by rowid before the sidecar rows are dropped and re-inserted.
    */
  def replaceWorkspaceSymbols(docId: DocId, rows: Seq[WorkspaceSymbolRow]): Unit =
    db.withWriteTransaction {
      val oldRowids = db
        .prepare("SELECT rowid FROM workspace_symbol_rows WHERE doc_id = ?")
        .bindLong(1, docId.value)
        .queryAll(_.columnLong(0))
      val deleteFts = db.prepare("DELETE FROM workspace_symbols_fts WHERE rowid = ?")
      oldRowids.foreach(rowid => deleteFts.bindLong(1, rowid).run())
      db.prepare("DELETE FROM workspace_symbol_rows WHERE doc_id = ?")
        .bindLong(1, docId.value)
        .run()
      val insertRow = db.prepare(
        "INSERT INTO workspace_symbol_rows (symbol_id, target_id, doc_id, kind) VALUES (?, ?, ?, ?)"
      )
      val insertFts = db.prepare(
        "INSERT INTO workspace_symbols_fts (rowid, display_name, owner_name, package_name) VALUES (?, ?, ?, ?)"
      )
      rows.foreach { r =>
        insertRow
          .bindLong(1, r.symbolId.value)
          .bindLong(2, r.targetId.value)
          .bindLong(3, docId.value)
          .bindInt(4, r.kind.code)
          .run()
        val rowid = db.lastInsertRowid
        insertFts
          .bindLong(1, rowid)
          .bindText(2, r.displayName)
          .bindTextOpt(3, r.ownerName)
          .bindTextOpt(4, r.packageName)
          .run()
      }
    }

  /** FTS5 prefix search: each whitespace-separated token is quoted and
    * suffixed `*`, tokens are AND-ed, results come back in bm25 order.
    * Names are joined from symbol_metadata (the FTS table is contentless).
    */
  def workspaceSymbolSearch(query: String, limit: Int): Vector[WorkspaceSymbolHit] =
    val tokens = query.trim.split("\\s+").toVector.filter(_.nonEmpty)
    if tokens.isEmpty || limit <= 0 then Vector.empty
    else
      val matchExpr = tokens
        .map(t => "\"" + t.replace("\"", "\"\"") + "\"*")
        .mkString(" ")
      db.prepare(
        """SELECT r.symbol_id, m.display_name, m.owner_name, m.package_name,
          |       r.kind, r.doc_id, r.target_id
          |FROM workspace_symbols_fts
          |JOIN workspace_symbol_rows r ON r.rowid = workspace_symbols_fts.rowid
          |LEFT JOIN symbol_metadata m
          |  ON m.symbol_id = r.symbol_id AND m.target_id = r.target_id AND m.doc_id = r.doc_id
          |WHERE workspace_symbols_fts MATCH ?
          |ORDER BY bm25(workspace_symbols_fts)
          |LIMIT ?""".stripMargin
      )
        .bindText(1, matchExpr)
        .bindInt(2, limit)
        .queryAll { st =>
          WorkspaceSymbolHit(
            symbolId = SymbolId(st.columnLong(0)),
            displayName = st.columnTextOpt(1).getOrElse(""),
            ownerName = st.columnTextOpt(2),
            packageName = st.columnTextOpt(3),
            kind = SymKind.fromCode(st.columnInt(4)),
            docId = DocId(st.columnLong(5)),
            targetId = TargetId(st.columnLong(6))
          )
        }

  // --- segment manifest ---

  /** Registers a new (inactive) postings segment. */
  def insertSegment(
      path: String,
      createdAtMs: Long,
      minEpoch: Long,
      maxEpoch: Long,
      checksum: Long
  ): Long =
    db.withWriteTransaction {
      db.prepare(
        """INSERT INTO segment_manifest (path, created_at_ms, min_epoch, max_epoch, active, checksum)
          |VALUES (?, ?, ?, ?, 0, ?)""".stripMargin
      )
        .bindText(1, path)
        .bindLong(2, createdAtMs)
        .bindLong(3, minEpoch)
        .bindLong(4, maxEpoch)
        .bindLong(5, checksum)
        .run()
      db.lastInsertRowid
    }

  /** Atomically makes `segmentId` the single active segment (plan 9.2 step 8:
    * the manifest transaction that publishes a new postings generation).
    */
  def activateSegment(segmentId: Long): Unit =
    db.withWriteTransaction {
      val exists = db
        .prepare("SELECT 1 FROM segment_manifest WHERE segment_id = ?")
        .bindLong(1, segmentId)
        .queryOne(_ => ())
        .isDefined
      if !exists then throw IllegalArgumentException(s"segment $segmentId does not exist")
      db.prepare("UPDATE segment_manifest SET active = CASE WHEN segment_id = ? THEN 1 ELSE 0 END")
        .bindLong(1, segmentId)
        .run()
    }

  def activeSegment(): Option[SegmentRow] =
    db.prepare(
      """SELECT segment_id, path, created_at_ms, min_epoch, max_epoch, active, checksum
        |FROM segment_manifest WHERE active = 1""".stripMargin
    ).queryOne(readSegment)

  def allSegments(): Vector[SegmentRow] =
    db.prepare(
      """SELECT segment_id, path, created_at_ms, min_epoch, max_epoch, active, checksum
        |FROM segment_manifest ORDER BY segment_id""".stripMargin
    ).queryAll(readSegment)

  private def readSegment(st: Statement): SegmentRow =
    SegmentRow(
      segmentId = st.columnLong(0),
      path = st.columnText(1),
      createdAtMs = st.columnLong(2),
      minEpoch = st.columnLong(3),
      maxEpoch = st.columnLong(4),
      active = st.columnBool(5),
      checksum = st.columnLong(6)
    )

object MetaStore:
  /** Default `-wal` size above which a fully-checkpointed WAL is truncated. */
  val DefaultWalThresholdBytes: Long = 16L * 1024 * 1024

  /** Opens (creating and migrating if needed) the metadata store at `path`. */
  def open(path: Path): MetaStore =
    val db = Db.open(path)
    try
      Schema.ensureSchema(db)
      new MetaStore(db)
    catch
      case t: Throwable =>
        try db.close()
        catch case c: Throwable => t.addSuppressed(c)
        throw t
