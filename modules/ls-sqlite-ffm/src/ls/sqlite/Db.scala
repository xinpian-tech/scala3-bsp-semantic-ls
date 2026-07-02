package ls.sqlite

import java.lang.foreign.MemorySegment
import java.nio.file.{Files, Path}
import scala.collection.mutable

/** One SQLite connection plus a prepared-statement cache.
  *
  * Threading contract: a Db is used single-threaded-writer style. The
  * connection is opened with SQLITE_OPEN_NOMUTEX (multi-thread mode), so it
  * must never be used from two threads at once; the ingest pipeline owns one
  * Db on one writer thread, exactly as plan section 9.2 prescribes. Readers
  * that need concurrency open their own Db.
  *
  * Statements prepared through [[prepare]] are cached by SQL text and stay
  * valid until [[close]]. Every execution helper on [[Statement]] resets the
  * statement and clears bindings when it finishes, so cached statements are
  * always ready for re-binding. Because of the cache, the same SQL text must
  * not be executed reentrantly (e.g. starting the same query inside a
  * foreachRow callback over that query).
  */
final class Db private (
    private[sqlite] val sqlite: Sqlite3,
    private[sqlite] val handle: MemorySegment,
    val path: String
) extends AutoCloseable:

  private val stmtCache = mutable.LinkedHashMap.empty[String, Statement]
  private var closed = false
  private var txDepth = 0

  private def requireOpen(): Unit =
    if closed then throw IllegalStateException(s"database $path is closed")

  /** Returns the cached prepared statement for this SQL, preparing it with
    * SQLITE_PREPARE_PERSISTENT on first use.
    */
  def prepare(sql: String): Statement =
    requireOpen()
    stmtCache.getOrElseUpdate(
      sql,
      new Statement(this, sql, sqlite.prepareV3(handle, sql, persistent = true))
    )

  /** Runs a (possibly multi-statement) SQL script, discarding result rows. */
  def exec(sql: String): Unit =
    requireOpen()
    sqlite.exec(handle, sql)

  def lastInsertRowid: Long =
    requireOpen()
    sqlite.lastInsertRowid(handle)

  /** Rows changed by the most recent statement (sqlite3_changes64). */
  def changes: Long =
    requireOpen()
    sqlite.changes64(handle)

  def extendedErrcode: Int =
    requireOpen()
    sqlite.extendedErrcode(handle)

  def isInTransaction: Boolean = txDepth > 0

  /** Runs `body` inside BEGIN IMMEDIATE / COMMIT, rolling back if it throws.
    *
    * Nested calls join the ambient transaction: only the outermost call
    * commits, and an exception escaping any level rolls the whole
    * transaction back. This lets MetaStore batch APIs (each transactional on
    * its own) compose into one ingest transaction.
    */
  def withWriteTransaction[A](body: => A): A =
    requireOpen()
    if txDepth > 0 then
      txDepth += 1
      try body
      finally txDepth -= 1
    else
      exec("BEGIN IMMEDIATE")
      txDepth = 1
      try
        val result = body
        exec("COMMIT")
        result
      catch
        case t: Throwable =>
          try exec("ROLLBACK")
          catch case r: Throwable => t.addSuppressed(r)
          throw t
      finally txDepth = 0

  /** Finalizes all cached statements and closes the connection. Idempotent. */
  def close(): Unit =
    if !closed then
      closed = true
      stmtCache.valuesIterator.foreach(_.finalizeNow())
      stmtCache.clear()
      sqlite.closeV2(handle)

object Db:
  /** Opens (creating if needed) the database at `path` with
    * READWRITE | CREATE | NOMUTEX and applies the project pragmas:
    * journal_mode=WAL, synchronous=NORMAL, busy_timeout=5000, foreign_keys=ON.
    */
  def open(path: Path): Db = open(path, Sqlite3.fromEnv)

  def open(path: Path, sqlite: Sqlite3): Db =
    Option(path.toAbsolutePath.getParent).foreach(Files.createDirectories(_))
    openWithFlags(path.toString, sqlite)

  /** In-memory database, mainly for tests. */
  def openInMemory(): Db = openWithFlags(":memory:", Sqlite3.fromEnv)

  private def openWithFlags(location: String, sqlite: Sqlite3): Db =
    val handle = sqlite.openV2(
      location,
      Sqlite3.OpenReadWrite | Sqlite3.OpenCreate | Sqlite3.OpenNoMutex
    )
    val db = new Db(sqlite, handle, location)
    try
      db.exec("PRAGMA journal_mode=WAL")
      db.exec("PRAGMA synchronous=NORMAL")
      db.exec("PRAGMA busy_timeout=5000")
      db.exec("PRAGMA foreign_keys=ON")
      db
    catch
      case t: Throwable =>
        try db.close()
        catch case c: Throwable => t.addSuppressed(c)
        throw t

/** Typed wrapper over one prepared statement. Bind indexes are 1-based and
  * column indexes 0-based, following the C API. All execution helpers reset
  * the statement and clear bindings on completion (also on failure), so the
  * statement can be re-bound immediately afterwards.
  */
final class Statement private[sqlite] (
    db: Db,
    val sql: String,
    private[sqlite] val handle: MemorySegment
) extends AutoCloseable:

  private def s3: Sqlite3 = db.sqlite
  private var finalized = false

  // --- binds ---

  def bindText(idx: Int, value: String): this.type =
    s3.bindText(handle, idx, value); this

  def bindTextOpt(idx: Int, value: Option[String]): this.type =
    value match
      case Some(v) => bindText(idx, v)
      case None => bindNull(idx)

  def bindInt(idx: Int, value: Int): this.type =
    s3.bindInt(handle, idx, value); this

  def bindLong(idx: Int, value: Long): this.type =
    s3.bindInt64(handle, idx, value); this

  def bindLongOpt(idx: Int, value: Option[Long]): this.type =
    value match
      case Some(v) => bindLong(idx, v)
      case None => bindNull(idx)

  def bindIntOpt(idx: Int, value: Option[Int]): this.type =
    value match
      case Some(v) => bindInt(idx, v)
      case None => bindNull(idx)

  def bindDouble(idx: Int, value: Double): this.type =
    s3.bindDouble(handle, idx, value); this

  def bindBlob(idx: Int, value: Array[Byte]): this.type =
    s3.bindBlob(handle, idx, value); this

  def bindNull(idx: Int): this.type =
    s3.bindNull(handle, idx); this

  def bindBool(idx: Int, value: Boolean): this.type =
    bindInt(idx, if value then 1 else 0)

  // --- stepping ---

  /** One sqlite3_step: true while a row is available. Throws on error. */
  def step(): Boolean = s3.step(handle) == Sqlite3.ROW

  def reset(): this.type =
    s3.reset(handle); this

  def clearBindings(): this.type =
    s3.clearBindings(handle); this

  private def resetAndClear(): Unit =
    s3.reset(handle)
    s3.clearBindings(handle)

  // --- execution helpers ---

  /** Steps to completion (DML / DDL) and returns sqlite3_changes64. */
  def run(): Long =
    try
      while step() do ()
      db.changes
    finally resetAndClear()

  /** Reads the first row, if any. */
  def queryOne[A](read: Statement => A): Option[A] =
    try if step() then Some(read(this)) else None
    finally resetAndClear()

  /** Reads every row eagerly. */
  def queryAll[A](read: Statement => A): Vector[A] =
    val out = Vector.newBuilder[A]
    try
      while step() do out += read(this)
      out.result()
    finally resetAndClear()

  /** Iterates rows without materializing them. */
  def foreachRow(f: Statement => Unit): Unit =
    try while step() do f(this)
    finally resetAndClear()

  // --- columns ---

  def columnCount: Int = s3.columnCount(handle)
  def columnType(idx: Int): Int = s3.columnType(handle, idx)
  def isNull(idx: Int): Boolean = columnType(idx) == Sqlite3.TypeNull
  def columnInt(idx: Int): Int = s3.columnInt(handle, idx)
  def columnLong(idx: Int): Long = s3.columnInt64(handle, idx)
  def columnDouble(idx: Int): Double = s3.columnDouble(handle, idx)
  def columnText(idx: Int): String = s3.columnText(handle, idx)
  def columnBlob(idx: Int): Array[Byte] = s3.columnBlob(handle, idx)
  def columnBool(idx: Int): Boolean = columnInt(idx) != 0

  def columnTextOpt(idx: Int): Option[String] =
    if isNull(idx) then None else Some(columnText(idx))

  def columnLongOpt(idx: Int): Option[Long] =
    if isNull(idx) then None else Some(columnLong(idx))

  def columnIntOpt(idx: Int): Option[Int] =
    if isNull(idx) then None else Some(columnInt(idx))

  private[sqlite] def finalizeNow(): Unit =
    if !finalized then
      finalized = true
      s3.finalizeStmt(handle)

  /** Finalizes the statement. Cached statements are owned by their Db and are
    * finalized on Db.close; call this only for statements you own.
    */
  def close(): Unit = finalizeNow()
