package ls.sqlite

/** Schema v1, exactly the SQL of plan.md section 7, plus lookup indexes and a
  * PRAGMA user_version migration guard.
  *
  * The only amendment to the plan text is `contentless_delete=1` on the FTS5
  * table: plan section 7.6 mandates a contentless table (content=''), and on
  * SQLite 3.51 deletes against a contentless table require contentless_delete
  * (workspace symbol rows are replaced per document on every re-ingest).
  */
object Schema:

  val Version = 2

  private[sqlite] val tables: List[String] = List(
    // 7.1 targets
    """CREATE TABLE targets (
      |  target_id        INTEGER PRIMARY KEY,
      |  bsp_id           TEXT NOT NULL UNIQUE,
      |  scala_version    TEXT NOT NULL,
      |  classpath_hash   TEXT NOT NULL,
      |  options_hash     TEXT NOT NULL,
      |  semanticdb_root  TEXT NOT NULL,
      |  sourceroot       TEXT NOT NULL,
      |  active           INTEGER NOT NULL
      |)""".stripMargin,
    // 7.2 documents
    """CREATE TABLE documents (
      |  doc_id               INTEGER PRIMARY KEY,
      |  target_id            INTEGER NOT NULL,
      |  uri                  TEXT NOT NULL,
      |  semanticdb_path      TEXT NOT NULL,
      |  semanticdb_mtime_ms  INTEGER NOT NULL,
      |  md5                  TEXT NOT NULL,
      |  epoch                INTEGER NOT NULL,
      |  active               INTEGER NOT NULL,
      |  generated            INTEGER NOT NULL DEFAULT 0,
      |  readonly             INTEGER NOT NULL DEFAULT 0,
      |  UNIQUE(target_id, uri)
      |)""".stripMargin,
    // 7.3 symbol_intern
    """CREATE TABLE symbol_intern (
      |  symbol_id         INTEGER PRIMARY KEY,
      |  universe_id       INTEGER NOT NULL,
      |  semantic_symbol   TEXT NOT NULL,
      |  local_doc_id      INTEGER,
      |  stable_hash       INTEGER NOT NULL,
      |  UNIQUE(universe_id, semantic_symbol, local_doc_id)
      |)""".stripMargin,
    // 7.4 symbol_metadata
    """CREATE TABLE symbol_metadata (
      |  symbol_id       INTEGER NOT NULL,
      |  target_id       INTEGER NOT NULL,
      |  doc_id          INTEGER NOT NULL,
      |  display_name    TEXT NOT NULL,
      |  owner_name      TEXT,
      |  package_name    TEXT,
      |  kind            INTEGER NOT NULL,
      |  properties      INTEGER NOT NULL,
      |  signature_hash  INTEGER,
      |  start_line      INTEGER,
      |  start_char      INTEGER,
      |  end_line        INTEGER,
      |  end_char        INTEGER,
      |  PRIMARY KEY(symbol_id, target_id, doc_id)
      |)""".stripMargin,
    // 7.5 reference / rename groups
    """CREATE TABLE ref_groups (
      |  ref_group_id INTEGER PRIMARY KEY
      |)""".stripMargin,
    """CREATE TABLE rename_groups (
      |  rename_group_id INTEGER PRIMARY KEY,
      |  unsafe_reason_mask INTEGER NOT NULL DEFAULT 0
      |)""".stripMargin,
    """CREATE TABLE symbol_to_ref_group (
      |  symbol_id INTEGER PRIMARY KEY,
      |  ref_group_id INTEGER NOT NULL
      |)""".stripMargin,
    """CREATE TABLE symbol_to_rename_group (
      |  symbol_id INTEGER PRIMARY KEY,
      |  rename_group_id INTEGER NOT NULL
      |)""".stripMargin,
    // 7.6 workspace symbol FTS (contentless; see class doc for the
    // contentless_delete amendment)
    """CREATE VIRTUAL TABLE workspace_symbols_fts
      |USING fts5(
      |  display_name,
      |  owner_name,
      |  package_name,
      |  content='',
      |  contentless_delete=1
      |)""".stripMargin,
    """CREATE TABLE workspace_symbol_rows (
      |  rowid      INTEGER PRIMARY KEY,
      |  symbol_id  INTEGER NOT NULL,
      |  target_id  INTEGER NOT NULL,
      |  doc_id     INTEGER NOT NULL,
      |  kind       INTEGER NOT NULL
      |)""".stripMargin,
    // 7.7 segment manifest
    """CREATE TABLE segment_manifest (
      |  segment_id      INTEGER PRIMARY KEY,
      |  path            TEXT NOT NULL,
      |  created_at_ms   INTEGER NOT NULL,
      |  min_epoch       INTEGER NOT NULL,
      |  max_epoch       INTEGER NOT NULL,
      |  active          INTEGER NOT NULL,
      |  checksum        INTEGER NOT NULL
      |)""".stripMargin
  )

  /** Schema v2 (plan §11 fuzzy workspace-symbol): a sidecar holding, per
    * `workspace_symbol_rows.rowid`, the normalized display name and its
    * camel-hump initials for the bounded fuzzy candidate pull. `IF NOT EXISTS`
    * so it is safe in both the fresh-create path and the v1→v2 migration.
    */
  private[sqlite] val fuzzyDdl: List[String] = List(
    """CREATE TABLE IF NOT EXISTS workspace_symbol_fuzzy (
      |  rowid            INTEGER PRIMARY KEY,
      |  normalized_name  TEXT NOT NULL,
      |  initials         TEXT NOT NULL
      |)""".stripMargin,
    "CREATE INDEX IF NOT EXISTS idx_workspace_symbol_fuzzy_normalized ON workspace_symbol_fuzzy(normalized_name)",
    "CREATE INDEX IF NOT EXISTS idx_workspace_symbol_fuzzy_initials ON workspace_symbol_fuzzy(initials)"
  )

  private[sqlite] val indexes: List[String] = List(
    "CREATE INDEX idx_documents_uri ON documents(uri)",
    // The table-level UNIQUE treats NULL local_doc_id values as distinct
    // (SQLite semantics), so global symbols need their own uniqueness guard.
    """CREATE UNIQUE INDEX idx_symbol_intern_global
      |ON symbol_intern(universe_id, semantic_symbol)
      |WHERE local_doc_id IS NULL""".stripMargin,
    "CREATE INDEX idx_symbol_metadata_doc ON symbol_metadata(doc_id)",
    "CREATE INDEX idx_workspace_symbol_rows_symbol ON workspace_symbol_rows(symbol_id)",
    "CREATE INDEX idx_workspace_symbol_rows_doc ON workspace_symbol_rows(doc_id)"
  )

  def userVersion(db: Db): Int =
    db.prepare("PRAGMA user_version").queryOne(_.columnInt(0)).getOrElse(0)

  /** Creates the current schema if the database is fresh, migrates a v1
    * database to v2, no-ops when already current, and refuses databases written
    * by a newer (or unknown) schema. Idempotent.
    */
  def ensureSchema(db: Db): Unit =
    userVersion(db) match
      case 0 =>
        db.withWriteTransaction {
          tables.foreach(db.exec)
          indexes.foreach(db.exec)
          fuzzyDdl.foreach(db.exec)
          db.exec(s"PRAGMA user_version=$Version")
        }
      case 1 =>
        db.withWriteTransaction {
          migrateV1ToV2(db)
          db.exec(s"PRAGMA user_version=$Version")
        }
      case Version => ()
      case other =>
        throw IllegalStateException(
          s"database ${db.path} has schema version $other; this build only supports version $Version"
        )

  /** v1 -> v2: add the fuzzy sidecar and backfill it from the existing
    * workspace-symbol rows (normalized name + camel-hump initials of each
    * document's `display_name`). Runs inside the caller's write transaction, so
    * an interrupted migration rolls back rather than leaving a partial sidecar.
    */
  private def migrateV1ToV2(db: Db): Unit =
    fuzzyDdl.foreach(db.exec)
    val rows = db
      .prepare(
        """SELECT r.rowid, m.display_name
          |FROM workspace_symbol_rows r
          |LEFT JOIN symbol_metadata m
          |  ON m.symbol_id = r.symbol_id AND m.target_id = r.target_id AND m.doc_id = r.doc_id""".stripMargin
      )
      .queryAll(st => (st.columnLong(0), st.columnTextOpt(1).getOrElse("")))
    val insert = db.prepare(
      "INSERT INTO workspace_symbol_fuzzy (rowid, normalized_name, initials) VALUES (?, ?, ?)"
    )
    rows.foreach { (rowid, name) =>
      insert
        .bindLong(1, rowid)
        .bindText(2, FuzzyRank.normalize(name))
        .bindText(3, FuzzyRank.initials(name))
        .run()
    }
