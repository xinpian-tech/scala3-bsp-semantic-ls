package ls.sqlite

import java.util.concurrent.ArrayBlockingQueue

/** A bounded, fixed-size pool of read-only [[Db]] connections for concurrent
  * read paths. The writer connection lives separately and never enters this
  * pool.
  *
  * The pool is an [[ArrayBlockingQueue]] pre-filled with `size` read-only
  * connections. [[borrow]] takes one (blocking — excess borrowers queue — when
  * all are checked out); [[giveBack]] returns it for reuse. A borrowed
  * connection is out of the queue until returned, so it is never handed to two
  * threads at once. Each connection is opened with `SQLITE_OPEN_NOMUTEX`, which
  * is safe precisely because the pool hands out one connection per thread at a
  * time.
  */
final class ReaderPool private (
    private val connections: Vector[Db],
    private val idle: ArrayBlockingQueue[Db]
) extends AutoCloseable:
  @volatile private var closed = false

  /** The fixed number of read-only connections in the pool. */
  def size: Int = connections.length

  private def requireOpen(): Unit =
    if closed then throw IllegalStateException("reader pool is closed")

  /** Borrows a read-only connection, blocking until one is free. */
  def borrow(): Db =
    requireOpen()
    val db = idle.take()
    if closed then throw IllegalStateException("reader pool is closed")
    db

  /** Returns a previously borrowed connection to the pool for reuse. Never
    * blocks (the queue capacity equals the connection count). A no-op once the
    * pool is closed, since close already freed the connection.
    */
  def giveBack(db: Db): Unit =
    if !closed then idle.put(db)

  /** Borrows a connection, runs `body`, and always returns the connection. */
  def withReader[A](body: Db => A): A =
    val db = borrow()
    try body(db)
    finally giveBack(db)

  /** Closes every connection, freeing each native arena. Idempotent. After
    * close a fresh [[borrow]] throws, and any still-borrowed connection is
    * closed too (so post-close use of it throws).
    */
  def close(): Unit =
    if !closed then
      closed = true
      connections.foreach(_.close())

object ReaderPool:
  val DefaultSize = 4

  /** Opens `size` read-only connections to the already-initialized database at
    * `location` (the writer must have created the file and its WAL first).
    */
  def open(location: String, sqlite: Sqlite3, size: Int = DefaultSize): ReaderPool =
    require(size >= 1, s"reader pool size must be >= 1, got $size")
    val conns = Vector.fill(size)(Db.openReadOnlyAt(location, sqlite))
    val idle = new ArrayBlockingQueue[Db](size)
    conns.foreach(idle.put)
    new ReaderPool(conns, idle)
