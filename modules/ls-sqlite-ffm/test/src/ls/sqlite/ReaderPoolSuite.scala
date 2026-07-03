package ls.sqlite

import java.nio.file.Path
import java.util.concurrent.atomic.AtomicReference

/** Reader-connection pool: bounded, read-only, one-connection-per-borrow,
  * reused on return, fully closed on shutdown, and never the writer.
  */
class ReaderPoolSuite extends munit.FunSuite with TempDbFixture:

  /** Opens a MetaStore (creating + schema-migrating the file, so the WAL/-shm
    * exist) plus one committed write, so read-only connections have data.
    */
  private def initStore(dir: Path): MetaStore =
    val store = MetaStore.open(dir.resolve("meta.sqlite"))
    store.upsertTarget("bsp://ws/x", "3.8.4", "c", "o", "/sdb", "/ws", active = true)
    store

  tempDir.test("borrowed connections are distinct while held, and a returned one is reused") { dir =>
    val store = initStore(dir)
    val pool = ReaderPool.open(store.db.path, store.db.sqlite, size = 3)
    try
      val a = pool.borrow()
      val b = pool.borrow()
      val c = pool.borrow()
      assertEquals(Set(a, b, c).size, 3, "three concurrently-held connections must be distinct")
      // return one and re-borrow: the same connection comes back (reuse)
      pool.giveBack(b)
      val d = pool.borrow()
      assertEquals(d, b, "a returned connection is reused")
      assertEquals(Set(a, c, d).size, 3)
      pool.giveBack(a)
      pool.giveBack(c)
      pool.giveBack(d)
    finally
      pool.close()
      store.close()
  }

  tempDir.test("borrow caps at the pool size: an excess borrower queues until a return") { dir =>
    val store = initStore(dir)
    val pool = ReaderPool.open(store.db.path, store.db.sqlite, size = 2)
    try
      val a = pool.borrow()
      val b = pool.borrow()
      assert(a ne b)
      // a third borrow must block while both connections are checked out
      val got = new AtomicReference[Db]()
      val t = new Thread(() => got.set(pool.borrow()))
      t.setDaemon(true)
      t.start()
      t.join(200)
      assert(t.isAlive, "the third borrow must block while the pool is exhausted")
      assertEquals(got.get(), null)
      // returning a connection lets the blocked borrower proceed and reuse it
      pool.giveBack(a)
      t.join(5000)
      assert(!t.isAlive, "the third borrow must proceed once a connection is returned")
      assertEquals(got.get(), a, "the blocked borrower reuses the returned connection")
      pool.giveBack(b)
      pool.giveBack(got.get())
    finally
      pool.close()
      store.close()
  }

  tempDir.test("close frees every connection: post-close borrow and borrowed-connection use throw") { dir =>
    val store = initStore(dir)
    val pool = ReaderPool.open(store.db.path, store.db.sqlite, size = 2)
    val borrowed = pool.borrow()
    pool.close()
    // the still-borrowed connection was closed by the pool
    intercept[IllegalStateException](borrowed.prepare("SELECT 1"))
    // borrowing after close throws rather than handing out a closed connection
    intercept[IllegalStateException](pool.borrow())
    store.close()
  }

  tempDir.test("the pool serves read-only connections and never the writer") { dir =>
    val store = initStore(dir)
    try
      store.readers.withReader { r =>
        assert(r ne store.db, "a pool connection must not be the writer connection")
        // it can read the committed data
        val count = r.prepare("SELECT count(*) FROM targets").queryOne(_.columnLong(0))
        assert(count.exists(_ >= 1L), s"reader should see committed rows, got $count")
        // but it cannot write (read-only), proving it is not the writer connection
        intercept[SqliteException](r.exec("CREATE TABLE zzz_reader_probe(x)"))
      }
    finally store.close()
  }
