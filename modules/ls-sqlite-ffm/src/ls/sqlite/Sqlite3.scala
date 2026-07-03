package ls.sqlite

import java.lang.foreign.{Arena, FunctionDescriptor, Linker, MemorySegment, SymbolLookup, ValueLayout}
import java.lang.invoke.MethodHandle
import java.nio.charset.StandardCharsets
import java.nio.file.{Files, Path}

/** A failed SQLite call. `code` is the extended result code when a connection
  * was available to ask, otherwise the primary result code of the call.
  */
final class SqliteException(val code: Int, message: String) extends RuntimeException(message)

object Sqlite3:
  // Primary result codes we branch on.
  inline val OK = 0
  inline val ROW = 100
  inline val DONE = 101

  // sqlite3_open_v2 flags.
  inline val OpenReadOnly = 0x00000001
  inline val OpenReadWrite = 0x00000002
  inline val OpenCreate = 0x00000004
  inline val OpenNoMutex = 0x00008000

  // sqlite3_column_type codes.
  inline val TypeInteger = 1
  inline val TypeFloat = 2
  inline val TypeText = 3
  inline val TypeBlob = 4
  inline val TypeNull = 5

  // sqlite3_prepare_v3 flags.
  inline val PreparePersistent = 0x01

  /** SQLITE_TRANSIENT destructor sentinel: SQLite copies the buffer during the
    * bind call, so our native buffer may be freed immediately afterwards.
    */
  private[sqlite] val Transient: MemorySegment = MemorySegment.ofAddress(-1L)

  val EnvVar = "LS_SQLITE_LIB"

  /** Resolves the SQLite shared library path from the environment. This
    * project never falls back to a system SQLite: the library must be the one
    * pinned by the Nix flake and exported as LS_SQLITE_LIB.
    */
  private[sqlite] def resolveLibraryPath(env: String => Option[String]): Path =
    env(EnvVar).map(_.trim).filter(_.nonEmpty) match
      case None =>
        throw IllegalStateException(
          s"$EnvVar is not set. It must point at the Nix-provided libsqlite3 shared library " +
            "(see flake dev shell); falling back to a system SQLite is not allowed."
        )
      case Some(p) =>
        val path = Path.of(p)
        if !Files.exists(path) then
          throw IllegalStateException(
            s"$EnvVar points at '$p' but no such file exists; refusing to guess a system SQLite."
          )
        path

  /** Process-wide binding loaded from LS_SQLITE_LIB. */
  lazy val fromEnv: Sqlite3 =
    load(resolveLibraryPath(k => Option(System.getenv(k))))

  /** Loads a binding for the given libsqlite3 shared object. The library stays
    * loaded for the process lifetime (Arena.global), which matches how a
    * language server uses its metadata store.
    */
  def load(library: Path): Sqlite3 =
    new Sqlite3(SymbolLookup.libraryLookup(library, Arena.global()))

/** Java 25 FFM binding to the SQLite C API.
  *
  * Arena discipline: every native buffer handed to SQLite is either copied by
  * SQLite during the call (text/blob binds use SQLITE_TRANSIENT) or only read
  * during the call (SQL text during prepare), so each method uses a confined
  * arena that closes before it returns. Connection and statement handles are
  * raw pointers owned by SQLite itself and are released via
  * sqlite3_close_v2 / sqlite3_finalize, not by an arena.
  */
final class Sqlite3 private (lookup: SymbolLookup):
  import Sqlite3.*
  import ValueLayout.{ADDRESS, JAVA_BYTE, JAVA_DOUBLE, JAVA_INT, JAVA_LONG}

  private val linker = Linker.nativeLinker()

  private def dh(name: String, desc: FunctionDescriptor): MethodHandle =
    val addr = lookup
      .find(name)
      .orElseThrow(() => IllegalStateException(s"symbol $name not found in SQLite library"))
    linker.downcallHandle(addr, desc)

  private val hOpenV2 =
    dh("sqlite3_open_v2", FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_INT, ADDRESS))
  private val hCloseV2 = dh("sqlite3_close_v2", FunctionDescriptor.of(JAVA_INT, ADDRESS))
  private val hPrepareV3 =
    dh(
      "sqlite3_prepare_v3",
      FunctionDescriptor.of(JAVA_INT, ADDRESS, ADDRESS, JAVA_INT, JAVA_INT, ADDRESS, ADDRESS)
    )
  private val hBindText =
    dh("sqlite3_bind_text", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, ADDRESS, JAVA_INT, ADDRESS))
  private val hBindInt =
    dh("sqlite3_bind_int", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, JAVA_INT))
  private val hBindInt64 =
    dh("sqlite3_bind_int64", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, JAVA_LONG))
  private val hBindDouble =
    dh("sqlite3_bind_double", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, JAVA_DOUBLE))
  private val hBindNull =
    dh("sqlite3_bind_null", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT))
  private val hBindBlob =
    dh("sqlite3_bind_blob", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT, ADDRESS, JAVA_INT, ADDRESS))
  private val hStep = dh("sqlite3_step", FunctionDescriptor.of(JAVA_INT, ADDRESS))
  private val hColumnCount = dh("sqlite3_column_count", FunctionDescriptor.of(JAVA_INT, ADDRESS))
  private val hColumnType =
    dh("sqlite3_column_type", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT))
  private val hColumnInt =
    dh("sqlite3_column_int", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT))
  private val hColumnInt64 =
    dh("sqlite3_column_int64", FunctionDescriptor.of(JAVA_LONG, ADDRESS, JAVA_INT))
  private val hColumnDouble =
    dh("sqlite3_column_double", FunctionDescriptor.of(JAVA_DOUBLE, ADDRESS, JAVA_INT))
  private val hColumnText =
    dh("sqlite3_column_text", FunctionDescriptor.of(ADDRESS, ADDRESS, JAVA_INT))
  private val hColumnBlob =
    dh("sqlite3_column_blob", FunctionDescriptor.of(ADDRESS, ADDRESS, JAVA_INT))
  private val hColumnBytes =
    dh("sqlite3_column_bytes", FunctionDescriptor.of(JAVA_INT, ADDRESS, JAVA_INT))
  private val hReset = dh("sqlite3_reset", FunctionDescriptor.of(JAVA_INT, ADDRESS))
  private val hClearBindings = dh("sqlite3_clear_bindings", FunctionDescriptor.of(JAVA_INT, ADDRESS))
  private val hFinalize = dh("sqlite3_finalize", FunctionDescriptor.of(JAVA_INT, ADDRESS))
  private val hErrmsg = dh("sqlite3_errmsg", FunctionDescriptor.of(ADDRESS, ADDRESS))
  private val hExtendedErrcode =
    dh("sqlite3_extended_errcode", FunctionDescriptor.of(JAVA_INT, ADDRESS))
  private val hLastInsertRowid =
    dh("sqlite3_last_insert_rowid", FunctionDescriptor.of(JAVA_LONG, ADDRESS))
  private val hChanges64 = dh("sqlite3_changes64", FunctionDescriptor.of(JAVA_LONG, ADDRESS))
  private val hDbHandle = dh("sqlite3_db_handle", FunctionDescriptor.of(ADDRESS, ADDRESS))

  private inline def withArena[A](inline f: Arena => A): A =
    val arena = Arena.ofConfined()
    try f(arena)
    finally arena.close()

  private def utf8(arena: Arena, bytes: Array[Byte]): MemorySegment =
    // max(len, 1): a zero-length allocation would hand SQLite a pointer it may
    // not dereference, and a NULL pointer would bind SQL NULL instead of ''.
    val seg = arena.allocate(math.max(bytes.length, 1).toLong)
    if bytes.length > 0 then MemorySegment.copy(bytes, 0, seg, JAVA_BYTE, 0L, bytes.length)
    seg

  /** NUL-terminated UTF-8 C string. Written byte-wise on purpose: only FFM
    * methods whose signatures are identical on JDK 21..25 are used, so the
    * module compiles no matter which JDK the build daemon runs on.
    */
  private def cString(arena: Arena, value: String): MemorySegment =
    val bytes = value.getBytes(StandardCharsets.UTF_8)
    val seg = arena.allocate(bytes.length + 1L)
    if bytes.length > 0 then MemorySegment.copy(bytes, 0, seg, JAVA_BYTE, 0L, bytes.length)
    seg.set(JAVA_BYTE, bytes.length.toLong, 0: Byte)
    seg

  /** Reads a NUL-terminated UTF-8 C string from a raw pointer. */
  private def readCString(ptr: MemorySegment, maxLen: Long): String =
    val seg = ptr.reinterpret(maxLen)
    var len = 0L
    while len < maxLen && seg.get(JAVA_BYTE, len) != 0 do len += 1
    val bytes = new Array[Byte](len.toInt)
    MemorySegment.copy(seg, JAVA_BYTE, 0L, bytes, 0, len.toInt)
    new String(bytes, StandardCharsets.UTF_8)

  private def dbError(db: MemorySegment, fallbackRc: Int, context: String): Nothing =
    val (code, msg) =
      if db.address() == 0 then (fallbackRc, s"sqlite error $fallbackRc")
      else (extendedErrcode(db), errmsg(db))
    throw SqliteException(code, s"$context: $msg")

  private def stmtError(stmt: MemorySegment, rc: Int, context: String): Nothing =
    val db = (hDbHandle.invokeExact(stmt): MemorySegment)
    dbError(db, rc, context)

  private def checkBind(stmt: MemorySegment, rc: Int, what: String): Unit =
    if rc != OK then stmtError(stmt, rc, s"sqlite3_bind_$what failed")

  // --- connection lifecycle ---

  def openV2(path: String, flags: Int): MemorySegment =
    withArena { arena =>
      val cPath = cString(arena, path)
      val out = arena.allocate(ADDRESS)
      val rc = (hOpenV2.invokeExact(cPath, out, flags, MemorySegment.NULL): Int)
      val db = out.get(ADDRESS, 0)
      if rc != OK then
        val msg =
          if db.address() == 0 then s"sqlite3_open_v2($path) failed with code $rc"
          else
            val m = errmsg(db)
            val _ = (hCloseV2.invokeExact(db): Int)
            s"sqlite3_open_v2($path) failed: $m"
        throw SqliteException(rc, msg)
      db
    }

  def closeV2(db: MemorySegment): Unit =
    val rc = (hCloseV2.invokeExact(db): Int)
    if rc != OK then throw SqliteException(rc, s"sqlite3_close_v2 failed with code $rc")

  // --- statements ---

  /** Prepares exactly one statement. Trailing content other than whitespace or
    * a terminating semicolon is an error; use [[exec]] for scripts.
    */
  def prepareV3(db: MemorySegment, sql: String, persistent: Boolean): MemorySegment =
    withArena { arena =>
      val bytes = sql.getBytes(StandardCharsets.UTF_8)
      val seg = arena.allocate(bytes.length + 1L)
      if bytes.length > 0 then MemorySegment.copy(bytes, 0, seg, JAVA_BYTE, 0L, bytes.length)
      seg.set(JAVA_BYTE, bytes.length.toLong, 0: Byte)
      val stmtOut = arena.allocate(ADDRESS)
      val tailOut = arena.allocate(ADDRESS)
      val flags = if persistent then PreparePersistent else 0
      val rc = (hPrepareV3.invokeExact(db, seg, bytes.length + 1, flags, stmtOut, tailOut): Int)
      if rc != OK then dbError(db, rc, s"sqlite3_prepare_v3 failed for: $sql")
      val stmt = stmtOut.get(ADDRESS, 0)
      if stmt.address() == 0 then
        throw SqliteException(rc, s"SQL contains no statement: $sql")
      val tailOff = (tailOut.get(ADDRESS, 0).address() - seg.address()).toInt
      val rest = new String(bytes, tailOff, bytes.length - tailOff, StandardCharsets.UTF_8)
      if rest.exists(c => !c.isWhitespace) then
        val _ = (hFinalize.invokeExact(stmt): Int)
        throw IllegalArgumentException(
          s"prepare expects a single statement but got trailing SQL '${rest.trim}'; use exec for scripts"
        )
      stmt
    }

  /** Raw sqlite3_step: returns ROW or DONE, throws on any error code. */
  def step(stmt: MemorySegment): Int =
    val rc = (hStep.invokeExact(stmt): Int)
    if rc != ROW && rc != DONE then stmtError(stmt, rc, "sqlite3_step failed")
    rc

  /** Resets the statement. Never throws: a failed step has already thrown, and
    * sqlite3_reset merely repeats that step's result code.
    */
  def reset(stmt: MemorySegment): Unit =
    val _ = (hReset.invokeExact(stmt): Int)

  def clearBindings(stmt: MemorySegment): Unit =
    val _ = (hClearBindings.invokeExact(stmt): Int)

  def finalizeStmt(stmt: MemorySegment): Unit =
    val _ = (hFinalize.invokeExact(stmt): Int)

  // --- binds (1-based indexes, as in the C API) ---

  def bindText(stmt: MemorySegment, idx: Int, value: String): Unit =
    withArena { arena =>
      val bytes = value.getBytes(StandardCharsets.UTF_8)
      val seg = utf8(arena, bytes)
      checkBind(stmt, (hBindText.invokeExact(stmt, idx, seg, bytes.length, Transient): Int), "text")
    }

  def bindInt(stmt: MemorySegment, idx: Int, value: Int): Unit =
    checkBind(stmt, (hBindInt.invokeExact(stmt, idx, value): Int), "int")

  def bindInt64(stmt: MemorySegment, idx: Int, value: Long): Unit =
    checkBind(stmt, (hBindInt64.invokeExact(stmt, idx, value): Int), "int64")

  def bindDouble(stmt: MemorySegment, idx: Int, value: Double): Unit =
    checkBind(stmt, (hBindDouble.invokeExact(stmt, idx, value): Int), "double")

  def bindNull(stmt: MemorySegment, idx: Int): Unit =
    checkBind(stmt, (hBindNull.invokeExact(stmt, idx): Int), "null")

  def bindBlob(stmt: MemorySegment, idx: Int, value: Array[Byte]): Unit =
    withArena { arena =>
      val seg = utf8(arena, value)
      checkBind(stmt, (hBindBlob.invokeExact(stmt, idx, seg, value.length, Transient): Int), "blob")
    }

  // --- columns (0-based indexes, as in the C API) ---

  def columnCount(stmt: MemorySegment): Int =
    (hColumnCount.invokeExact(stmt): Int)

  def columnType(stmt: MemorySegment, idx: Int): Int =
    (hColumnType.invokeExact(stmt, idx): Int)

  def columnInt(stmt: MemorySegment, idx: Int): Int =
    (hColumnInt.invokeExact(stmt, idx): Int)

  def columnInt64(stmt: MemorySegment, idx: Int): Long =
    (hColumnInt64.invokeExact(stmt, idx): Long)

  def columnDouble(stmt: MemorySegment, idx: Int): Double =
    (hColumnDouble.invokeExact(stmt, idx): Double)

  /** UTF-8 text of the column. Returns "" for SQL NULL; callers that need to
    * distinguish NULL from '' must check columnType first.
    */
  def columnText(stmt: MemorySegment, idx: Int): String =
    val ptr = (hColumnText.invokeExact(stmt, idx): MemorySegment)
    if ptr.address() == 0 then ""
    else
      // Per the C API contract: call column_text first, then column_bytes,
      // so the byte length matches the UTF-8 text representation.
      val len = (hColumnBytes.invokeExact(stmt, idx): Int)
      if len == 0 then ""
      else
        val bytes = new Array[Byte](len)
        MemorySegment.copy(ptr.reinterpret(len.toLong), JAVA_BYTE, 0L, bytes, 0, len)
        new String(bytes, StandardCharsets.UTF_8)

  /** Blob bytes of the column. Returns an empty array for NULL or empty. */
  def columnBlob(stmt: MemorySegment, idx: Int): Array[Byte] =
    val ptr = (hColumnBlob.invokeExact(stmt, idx): MemorySegment)
    val len = (hColumnBytes.invokeExact(stmt, idx): Int)
    if ptr.address() == 0 || len == 0 then Array.emptyByteArray
    else
      val bytes = new Array[Byte](len)
      MemorySegment.copy(ptr.reinterpret(len.toLong), JAVA_BYTE, 0L, bytes, 0, len)
      bytes

  def columnBytes(stmt: MemorySegment, idx: Int): Int =
    (hColumnBytes.invokeExact(stmt, idx): Int)

  // --- connection state ---

  def errmsg(db: MemorySegment): String =
    val ptr = (hErrmsg.invokeExact(db): MemorySegment)
    if ptr.address() == 0 then "unknown sqlite error"
    else readCString(ptr, 1L << 20)

  def extendedErrcode(db: MemorySegment): Int =
    (hExtendedErrcode.invokeExact(db): Int)

  def lastInsertRowid(db: MemorySegment): Long =
    (hLastInsertRowid.invokeExact(db): Long)

  def changes64(db: MemorySegment): Long =
    (hChanges64.invokeExact(db): Long)

  /** sqlite3_exec equivalent built on prepare/step: runs every statement in
    * the script, discarding any result rows.
    */
  def exec(db: MemorySegment, sql: String): Unit =
    withArena { arena =>
      val bytes = sql.getBytes(StandardCharsets.UTF_8)
      if bytes.length == 0 then return
      val seg = arena.allocate(bytes.length + 1L)
      MemorySegment.copy(bytes, 0, seg, JAVA_BYTE, 0L, bytes.length)
      seg.set(JAVA_BYTE, bytes.length.toLong, 0: Byte)
      var off = 0L
      while off < bytes.length do
        val stmtOut = arena.allocate(ADDRESS)
        val tailOut = arena.allocate(ADDRESS)
        val cur = seg.asSlice(off)
        val rc = (hPrepareV3.invokeExact(db, cur, -1, 0, stmtOut, tailOut): Int)
        if rc != OK then dbError(db, rc, s"exec failed to prepare at offset $off of: $sql")
        val stmt = stmtOut.get(ADDRESS, 0)
        val next = tailOut.get(ADDRESS, 0).address() - seg.address()
        if stmt.address() != 0 then
          try
            var r = (hStep.invokeExact(stmt): Int)
            while r == ROW do r = (hStep.invokeExact(stmt): Int)
            if r != DONE then dbError(db, r, s"exec failed while stepping: $sql")
          finally
            val _ = (hFinalize.invokeExact(stmt): Int)
        off = next
    }
