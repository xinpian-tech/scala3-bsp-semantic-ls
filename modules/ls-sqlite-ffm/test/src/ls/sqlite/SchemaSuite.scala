package ls.sqlite

class SchemaSuite extends munit.FunSuite with TempDbFixture:

  private val expectedTables = Set(
    "targets",
    "documents",
    "symbol_intern",
    "symbol_metadata",
    "ref_groups",
    "rename_groups",
    "symbol_to_ref_group",
    "symbol_to_rename_group",
    "workspace_symbols_fts",
    "workspace_symbol_rows",
    "segment_manifest"
  )

  tempDir.test("ensureSchema creates all schema v1 tables and sets user_version") { dir =>
    val db = Db.open(dir.resolve("meta.sqlite"))
    try
      assertEquals(Schema.userVersion(db), 0)
      Schema.ensureSchema(db)
      assertEquals(Schema.userVersion(db), 1)
      val names = db
        .prepare("SELECT name FROM sqlite_master WHERE type IN ('table','view') ORDER BY name")
        .queryAll(_.columnText(0))
        .toSet
      assert(
        expectedTables.subsetOf(names),
        s"missing tables: ${expectedTables -- names}"
      )
      val indexNames = db
        .prepare("SELECT name FROM sqlite_master WHERE type = 'index'")
        .queryAll(_.columnText(0))
        .toSet
      assert(indexNames.contains("idx_documents_uri"))
      assert(indexNames.contains("idx_symbol_intern_global"))
      assert(indexNames.contains("idx_workspace_symbol_rows_symbol"))
    finally db.close()
  }

  tempDir.test("ensureSchema is idempotent") { dir =>
    val path = dir.resolve("meta.sqlite")
    val db = Db.open(path)
    try
      Schema.ensureSchema(db)
      Schema.ensureSchema(db)
    finally db.close()
    // and across connections
    val db2 = Db.open(path)
    try
      Schema.ensureSchema(db2)
      assertEquals(Schema.userVersion(db2), 1)
      // schema still functional after three ensureSchema calls
      db2.exec("INSERT INTO ref_groups DEFAULT VALUES")
      assertEquals(
        db2.prepare("SELECT count(*) FROM ref_groups").queryOne(_.columnLong(0)),
        Some(1L)
      )
    finally db2.close()
  }

  tempDir.test("ensureSchema refuses unknown future schema versions") { dir =>
    val db = Db.open(dir.resolve("meta.sqlite"))
    try
      db.exec("PRAGMA user_version=99")
      val ex = intercept[IllegalStateException](Schema.ensureSchema(db))
      assert(ex.getMessage.contains("99"), ex.getMessage)
    finally db.close()
  }

  tempDir.test("global symbol uniqueness is enforced despite NULL local_doc_id") { dir =>
    val db = Db.open(dir.resolve("meta.sqlite"))
    try
      Schema.ensureSchema(db)
      db.exec(
        "INSERT INTO symbol_intern (universe_id, semantic_symbol, local_doc_id, stable_hash) VALUES (1, 'a/B#', NULL, 7)"
      )
      // SQLite UNIQUE treats NULLs as distinct; the partial index must reject this
      intercept[SqliteException] {
        db.exec(
          "INSERT INTO symbol_intern (universe_id, semantic_symbol, local_doc_id, stable_hash) VALUES (1, 'a/B#', NULL, 7)"
        )
      }
    finally db.close()
  }
