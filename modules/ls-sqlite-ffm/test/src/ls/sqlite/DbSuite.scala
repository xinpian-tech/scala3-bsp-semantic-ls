package ls.sqlite

import java.nio.file.Files

class DbSuite extends munit.FunSuite with TempDbFixture:

  tempDir.test("open creates the file and WAL mode is actually on") { dir =>
    val db = Db.open(dir.resolve("meta.sqlite"))
    try
      assert(Files.exists(dir.resolve("meta.sqlite")))
      val mode = db.prepare("PRAGMA journal_mode").queryOne(_.columnText(0))
      assertEquals(mode, Some("wal"))
      val sync = db.prepare("PRAGMA synchronous").queryOne(_.columnInt(0))
      assertEquals(sync, Some(1)) // NORMAL
      val busy = db.prepare("PRAGMA busy_timeout").queryOne(_.columnInt(0))
      assertEquals(busy, Some(5000))
      val fk = db.prepare("PRAGMA foreign_keys").queryOne(_.columnInt(0))
      assertEquals(fk, Some(1))
    finally db.close()
  }

  tempDir.test("prepared statements are cached by SQL text") { dir =>
    val db = Db.open(dir.resolve("meta.sqlite"))
    try
      val a = db.prepare("SELECT 1")
      val b = db.prepare("SELECT 1")
      assert(a eq b)
      assert(a ne db.prepare("SELECT 2"))
    finally db.close()
  }

  tempDir.test("withWriteTransaction commits on success") { dir =>
    val db = Db.open(dir.resolve("meta.sqlite"))
    try
      db.exec("CREATE TABLE t (x INTEGER)")
      db.withWriteTransaction {
        db.prepare("INSERT INTO t VALUES (?)").bindInt(1, 42).run()
      }
      assertEquals(db.prepare("SELECT count(*) FROM t").queryOne(_.columnLong(0)), Some(1L))
    finally db.close()
  }

  tempDir.test("withWriteTransaction rolls back on exception") { dir =>
    val db = Db.open(dir.resolve("meta.sqlite"))
    try
      db.exec("CREATE TABLE t (x INTEGER)")
      db.exec("INSERT INTO t VALUES (1)")
      val boom = intercept[RuntimeException] {
        db.withWriteTransaction {
          db.prepare("INSERT INTO t VALUES (?)").bindInt(1, 2).run()
          db.prepare("INSERT INTO t VALUES (?)").bindInt(1, 3).run()
          throw new RuntimeException("boom")
        }
      }
      assertEquals(boom.getMessage, "boom")
      assert(!db.isInTransaction)
      assertEquals(db.prepare("SELECT count(*) FROM t").queryOne(_.columnLong(0)), Some(1L))
      // connection is still usable after rollback
      db.withWriteTransaction {
        db.prepare("INSERT INTO t VALUES (?)").bindInt(1, 4).run()
      }
      assertEquals(db.prepare("SELECT count(*) FROM t").queryOne(_.columnLong(0)), Some(2L))
    finally db.close()
  }

  tempDir.test("nested withWriteTransaction joins the outer transaction") { dir =>
    val db = Db.open(dir.resolve("meta.sqlite"))
    try
      db.exec("CREATE TABLE t (x INTEGER)")
      intercept[RuntimeException] {
        db.withWriteTransaction {
          db.withWriteTransaction {
            db.prepare("INSERT INTO t VALUES (1)").run()
          }
          throw new RuntimeException("outer fails after inner 'committed'")
        }
      }
      // the inner block joined the outer transaction, so its insert rolled back
      assertEquals(db.prepare("SELECT count(*) FROM t").queryOne(_.columnLong(0)), Some(0L))
    finally db.close()
  }

  tempDir.test("typed binds and columns round-trip, including NULL and UTF-8") { dir =>
    val db = Db.open(dir.resolve("meta.sqlite"))
    try
      db.exec("CREATE TABLE t (i INTEGER, r REAL, s TEXT, b BLOB)")
      val text = "索引🌊 rename参照 ẞ🚀"
      val blob = Array[Byte](0, 1, -1, 127, -128)
      db.prepare("INSERT INTO t VALUES (?, ?, ?, ?)")
        .bindLong(1, Long.MaxValue)
        .bindDouble(2, 2.5)
        .bindText(3, text)
        .bindBlob(4, blob)
        .run()
      db.prepare("INSERT INTO t VALUES (?, ?, ?, ?)")
        .bindNull(1)
        .bindNull(2)
        .bindTextOpt(3, None)
        .bindNull(4)
        .run()
      val rows = db
        .prepare("SELECT i, r, s, b FROM t ORDER BY i IS NULL")
        .queryAll(st =>
          (st.columnLongOpt(0), if st.isNull(1) then None else Some(st.columnDouble(1)), st.columnTextOpt(2), st.columnBlob(3))
        )
      assertEquals(rows.length, 2)
      assertEquals(rows(0)._1, Some(Long.MaxValue))
      assertEquals(rows(0)._2, Some(2.5))
      assertEquals(rows(0)._3, Some(text))
      assert(rows(0)._4.sameElements(blob))
      assertEquals(rows(1)._1, None)
      assertEquals(rows(1)._2, None)
      assertEquals(rows(1)._3, None)
      assertEquals(rows(1)._4.length, 0)
    finally db.close()
  }

  tempDir.test("run returns the changed-row count and lastInsertRowid works") { dir =>
    val db = Db.open(dir.resolve("meta.sqlite"))
    try
      db.exec("CREATE TABLE t (id INTEGER PRIMARY KEY, x INTEGER)")
      db.prepare("INSERT INTO t (x) VALUES (?)").bindInt(1, 1).run()
      val firstId = db.lastInsertRowid
      db.prepare("INSERT INTO t (x) VALUES (?)").bindInt(1, 2).run()
      assertEquals(db.lastInsertRowid, firstId + 1)
      val changed = db.prepare("UPDATE t SET x = x + 10").run()
      assertEquals(changed, 2L)
    finally db.close()
  }

  tempDir.test("exec runs multi-statement scripts") { dir =>
    val db = Db.open(dir.resolve("meta.sqlite"))
    try
      db.exec(
        """CREATE TABLE a (x INTEGER);
          |CREATE TABLE b (y TEXT);
          |INSERT INTO a VALUES (7);
          |INSERT INTO b VALUES ('seven');
          |""".stripMargin
      )
      assertEquals(db.prepare("SELECT x FROM a").queryOne(_.columnInt(0)), Some(7))
      assertEquals(db.prepare("SELECT y FROM b").queryOne(_.columnText(0)), Some("seven"))
    finally db.close()
  }

  tempDir.test("statements and connection refuse use after close") { dir =>
    val db = Db.open(dir.resolve("meta.sqlite"))
    db.exec("CREATE TABLE t (x INTEGER)")
    db.close()
    db.close() // idempotent
    intercept[IllegalStateException](db.prepare("SELECT 1"))
    intercept[IllegalStateException](db.exec("SELECT 1"))
  }

  tempDir.test("queryAll resets the statement so it can be re-bound") { dir =>
    val db = Db.open(dir.resolve("meta.sqlite"))
    try
      db.exec("CREATE TABLE t (x INTEGER); INSERT INTO t VALUES (1); INSERT INTO t VALUES (2)")
      val st = db.prepare("SELECT x FROM t WHERE x >= ? ORDER BY x")
      assertEquals(st.bindInt(1, 1).queryAll(_.columnInt(0)), Vector(1, 2))
      assertEquals(st.bindInt(1, 2).queryAll(_.columnInt(0)), Vector(2))
      var seen = List.empty[Int]
      st.bindInt(1, 1).foreachRow(s => seen ::= s.columnInt(0))
      assertEquals(seen.sorted, List(1, 2))
    finally db.close()
  }
