package ls.sqlite

import java.util.concurrent.{ArrayBlockingQueue, TimeUnit}

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

  /** Borrows a read-only connection, blocking until one is free. When a
    * connection is available it is returned at once; otherwise the caller polls
    * on a short interval and re-checks shutdown, so [[close]] wakes a parked
    * borrower (it fails fast rather than hanging in a plain blocking take).
    */
  def borrow(): Db =
    requireOpen()
    var result: Db = null
    while result == null do
      if closed then throw IllegalStateException("reader pool is closed")
      val polled = idle.poll(ReaderPool.PollMillis, TimeUnit.MILLISECONDS)
      if polled != null then
        if closed then throw IllegalStateException("reader pool is closed")
        result = polled
    result

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

  /** How often a parked [[borrow]] wakes to re-check shutdown. */
  private val PollMillis = 50L

  /** Opens `size` read-only connections to the already-initialized database at
    * `location` (the writer must have created the file and its WAL first). If
    * opening any connection fails, the ones already opened are closed before
    * the error propagates, so a partial pool never leaks connections.
    */
  def open(location: String, sqlite: Sqlite3, size: Int = DefaultSize): ReaderPool =
    require(size >= 1, s"reader pool size must be >= 1, got $size")
    val conns = Vector.newBuilder[Db]
    try
      var i = 0
      while i < size do
        conns += Db.openReadOnlyAt(location, sqlite)
        i += 1
    catch
      case t: Throwable =>
        conns.result().foreach(c => try c.close() catch { case c2: Throwable => t.addSuppressed(c2) })
        throw t
    val all = conns.result()
    val idle = new ArrayBlockingQueue[Db](size)
    all.foreach(idle.put)
    new ReaderPool(all, idle)
