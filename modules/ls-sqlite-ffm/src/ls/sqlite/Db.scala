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
/** WAL checkpoint mode (SQLite `PRAGMA wal_checkpoint`). */
enum CheckpointMode(val keyword: String):
  case Passive extends CheckpointMode("PASSIVE")
  case Full extends CheckpointMode("FULL")
  case Restart extends CheckpointMode("RESTART")
  case Truncate extends CheckpointMode("TRUNCATE")

/** Result row of `PRAGMA wal_checkpoint`: `busy` (the checkpoint could not run
  * to completion), `log` (frames in the WAL, -1 if unknown), `checkpointed`
  * (frames moved into the db file, -1 if unknown).
  */
final case class CheckpointResult(busy: Boolean, log: Int, checkpointed: Int):
  /** All WAL frames are checkpointed into the db file. */
  def fullyCheckpointed: Boolean = !busy && log >= 0 && log == checkpointed

/** Outcome of a scheduled checkpoint: the PASSIVE pass plus the optional
  * TRUNCATE pass (present only when TRUNCATE was attempted).
  */
final case class CheckpointOutcome(passive: CheckpointResult, truncate: Option[CheckpointResult]):
  /** The WAL file was reset to zero length. */
  def truncated: Boolean = truncate.exists(r => !r.busy)

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

  /** Runs `PRAGMA wal_checkpoint(mode)` and returns its result row. PASSIVE
    * never blocks on readers or writers; the other modes may return busy rather
    * than blocking (bounded by the current busy_timeout). Never throws for a
    * BUSY checkpoint.
    */
  def checkpoint(mode: CheckpointMode): CheckpointResult =
    requireOpen()
    prepare(s"PRAGMA wal_checkpoint(${mode.keyword})")
      .queryOne(st => CheckpointResult(st.columnInt(0) != 0, st.columnInt(1), st.columnInt(2)))
      .getOrElse(CheckpointResult(busy = true, log = -1, checkpointed = -1))

  /** Size of the `-wal` sidecar file in bytes (0 when it does not exist). */
  def walFileSizeBytes: Long =
    val wal = Path.of(path + "-wal")
    if Files.isRegularFile(wal) then Files.size(wal) else 0L

  /** Scheduled checkpoint that never blocks the writer: a PASSIVE pass (always
    * non-blocking), then a TRUNCATE pass only when PASSIVE fully checkpointed
    * the WAL and the `-wal` file exceeds `walThresholdBytes`. The TRUNCATE runs
    * with busy_timeout=0 so a concurrent reader makes it return busy at once
    * rather than waiting; the timeout is restored afterwards.
    */
  def smartCheckpoint(walThresholdBytes: Long): CheckpointOutcome =
    requireOpen()
    val passive = checkpoint(CheckpointMode.Passive)
    val truncate =
      if !(passive.fullyCheckpointed && walFileSizeBytes > walThresholdBytes) then None
      else
        val prevTimeout = prepare("PRAGMA busy_timeout").queryOne(_.columnInt(0)).getOrElse(5000)
        exec("PRAGMA busy_timeout=0")
        try Some(checkpoint(CheckpointMode.Truncate))
        finally exec(s"PRAGMA busy_timeout=$prevTimeout")
    CheckpointOutcome(passive, truncate)

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

  /** Opens an EXISTING database read-only (no create) — for reader-pool
    * connections. Write attempts fail; it shares the writer's WAL so reads see
    * the latest committed state. The database file and its WAL must already
    * exist (the writer created them).
    */
  def openReadOnly(path: Path): Db = openReadOnly(path, Sqlite3.fromEnv)

  def openReadOnly(path: Path, sqlite: Sqlite3): Db = openReadOnlyAt(path.toString, sqlite)

  private[sqlite] def openReadOnlyAt(location: String, sqlite: Sqlite3): Db =
    val handle = sqlite.openV2(location, Sqlite3.OpenReadOnly | Sqlite3.OpenNoMutex)
    val db = new Db(sqlite, handle, location)
    try
      db.exec("PRAGMA busy_timeout=5000")
      db.exec("PRAGMA query_only=ON")
      db
    catch
      case t: Throwable =>
        try db.close()
        catch case c: Throwable => t.addSuppressed(c)
        throw t

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
