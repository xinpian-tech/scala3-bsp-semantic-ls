package ls.sqlite

import java.nio.file.{Files, Path}

class Sqlite3Suite extends munit.FunSuite:

  test("FFM binding loads from LS_SQLITE_LIB and executes SQL") {
    // Production runs on Java 25 only; the Mill daemon in this dev shell may
    // fork tests on its own bundled JDK, so the binding sticks to FFM
    // signatures that are identical from JDK 21 preview through JDK 25.
    val s3 = Sqlite3.fromEnv
    val db = s3.openV2(":memory:", Sqlite3.OpenReadWrite | Sqlite3.OpenCreate | Sqlite3.OpenNoMutex)
    try
      s3.exec(db, "CREATE TABLE t (x TEXT); INSERT INTO t VALUES ('hello');")
      val stmt = s3.prepareV3(db, "SELECT x FROM t", persistent = false)
      try
        assertEquals(s3.step(stmt), Sqlite3.ROW)
        assertEquals(s3.columnText(stmt, 0), "hello")
        assertEquals(s3.step(stmt), Sqlite3.DONE)
      finally s3.finalizeStmt(stmt)
    finally s3.closeV2(db)
  }

  test("resolveLibraryPath fails clearly when LS_SQLITE_LIB is unset") {
    val ex = intercept[IllegalStateException] {
      Sqlite3.resolveLibraryPath(_ => None)
    }
    assert(ex.getMessage.contains("LS_SQLITE_LIB"), ex.getMessage)
    assert(ex.getMessage.contains("system SQLite"), ex.getMessage)
  }

  test("resolveLibraryPath fails clearly when LS_SQLITE_LIB points nowhere") {
    val ex = intercept[IllegalStateException] {
      Sqlite3.resolveLibraryPath(_ => Some("/does/not/exist/libsqlite3.so"))
    }
    assert(ex.getMessage.contains("no such file"), ex.getMessage)
  }

  test("UTF-8 text with CJK and emoji round-trips through bind and column") {
    val s3 = Sqlite3.fromEnv
    val db = s3.openV2(":memory:", Sqlite3.OpenReadWrite | Sqlite3.OpenCreate | Sqlite3.OpenNoMutex)
    try
      s3.exec(db, "CREATE TABLE t (x TEXT)")
      val text = "数据库索引🚀 — ẞsímbolo 🧵🌊字符串"
      val ins = s3.prepareV3(db, "INSERT INTO t VALUES (?)", persistent = false)
      try
        s3.bindText(ins, 1, text)
        assertEquals(s3.step(ins), Sqlite3.DONE)
      finally s3.finalizeStmt(ins)
      val sel = s3.prepareV3(db, "SELECT x, length(x) FROM t", persistent = false)
      try
        assertEquals(s3.step(sel), Sqlite3.ROW)
        assertEquals(s3.columnText(sel, 0), text)
        // sqlite length() counts characters (code points) for text
        assertEquals(s3.columnInt(sel, 1), text.codePointCount(0, text.length))
      finally s3.finalizeStmt(sel)
    finally s3.closeV2(db)
  }

  test("empty string binds as empty text, not NULL") {
    val s3 = Sqlite3.fromEnv
    val db = s3.openV2(":memory:", Sqlite3.OpenReadWrite | Sqlite3.OpenCreate | Sqlite3.OpenNoMutex)
    try
      s3.exec(db, "CREATE TABLE t (x TEXT)")
      val ins = s3.prepareV3(db, "INSERT INTO t VALUES (?)", persistent = false)
      try
        s3.bindText(ins, 1, "")
        assertEquals(s3.step(ins), Sqlite3.DONE)
      finally s3.finalizeStmt(ins)
      val sel = s3.prepareV3(db, "SELECT typeof(x), x FROM t", persistent = false)
      try
        assertEquals(s3.step(sel), Sqlite3.ROW)
        assertEquals(s3.columnText(sel, 0), "text")
        assertEquals(s3.columnText(sel, 1), "")
      finally s3.finalizeStmt(sel)
    finally s3.closeV2(db)
  }

  test("blob round-trips and prepare rejects multi-statement SQL") {
    val s3 = Sqlite3.fromEnv
    val db = s3.openV2(":memory:", Sqlite3.OpenReadWrite | Sqlite3.OpenCreate | Sqlite3.OpenNoMutex)
    try
      s3.exec(db, "CREATE TABLE t (x BLOB)")
      val payload = Array.tabulate[Byte](257)(i => (i % 251).toByte)
      val ins = s3.prepareV3(db, "INSERT INTO t VALUES (?)", persistent = false)
      try
        s3.bindBlob(ins, 1, payload)
        assertEquals(s3.step(ins), Sqlite3.DONE)
      finally s3.finalizeStmt(ins)
      val sel = s3.prepareV3(db, "SELECT x FROM t", persistent = false)
      try
        assertEquals(s3.step(sel), Sqlite3.ROW)
        assert(s3.columnBlob(sel, 0).sameElements(payload))
      finally s3.finalizeStmt(sel)
      intercept[IllegalArgumentException] {
        s3.prepareV3(db, "SELECT 1; SELECT 2", persistent = false)
      }
    finally s3.closeV2(db)
  }

  test("sqlite errors surface errmsg and extended errcode") {
    val s3 = Sqlite3.fromEnv
    val db = s3.openV2(":memory:", Sqlite3.OpenReadWrite | Sqlite3.OpenCreate | Sqlite3.OpenNoMutex)
    try
      s3.exec(db, "CREATE TABLE t (x TEXT NOT NULL)")
      val ex = intercept[SqliteException] {
        s3.exec(db, "INSERT INTO t VALUES (NULL)")
      }
      assert(ex.getMessage.contains("NOT NULL"), ex.getMessage)
      assert(ex.code != 0)
    finally s3.closeV2(db)
  }
