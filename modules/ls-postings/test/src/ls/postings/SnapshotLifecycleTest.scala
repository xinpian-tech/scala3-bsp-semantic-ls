package ls.postings

import java.nio.file.Files
import java.util.concurrent.atomic.AtomicInteger
import java.util.concurrent.{CountDownLatch, TimeUnit}

import ls.index.*
import TestSupport.*

/** Retain/release lifecycle: readers keep superseded snapshots alive and
  * correct until they release; arenas close exactly on drain; the manager
  * deletes superseded segment directories only once fully closed.
  */
class SnapshotLifecycleTest extends munit.FunSuite:

  private def generationData(gen: Int): SegmentData =
    SegmentData(
      docs = Vector(SegmentDoc(s"file:///gen/Doc.scala", docId = 1, epoch = gen, targetOrd = 0)),
      targets = Vector(77L),
      symbols = Vector(SegmentSymbol(s"gen/Sym$gen.", symbolId = gen.toLong, refGroupOrd = 0)),
      refOccurrences = Vector(
        Vector(GroupOcc(0, gen, 0, Span(1, 0, 1, 3 + gen), 0))
      ),
      defOccurrences = Vector(Vector.empty),
      renameOccurrences = Vector.empty,
      renameProfiles = Vector.empty,
      docOccurrences = Vector(Vector(DocOcc(0, Span(1, 0, 1, 3 + gen), 0)))
    )

  private def publishGeneration(manager: SnapshotManager, gen: Int): PostingsSnapshot =
    val dir = SegmentWriter.write(manager.root, manager.nextSegmentId(), generationData(gen))
    manager.publish(SegmentReader.open(dir))

  test("segment id allocation counts up from existing directories"):
    val manager = SnapshotManager(tempRoot("alloc"))
    assertEquals(manager.nextSegmentId(), 1L)
    publishGeneration(manager, 1)
    assertEquals(manager.nextSegmentId(), 2L)
    Files.createDirectories(manager.segmentsDir.resolve("segment-junk"))
    Files.createDirectories(manager.segmentsDir.resolve("not-a-segment"))
    assertEquals(manager.nextSegmentId(), 2L)
    publishGeneration(manager, 2)
    assertEquals(manager.nextSegmentId(), 3L)
    manager.close()

  test("current is None before any publish; withCurrent is None too"):
    val manager = SnapshotManager(tempRoot("none"))
    assertEquals(manager.current(), None)
    assertEquals(manager.withCurrent(_.docCount), None)

  test("a reader holding the old snapshot survives a publish; arena closes only on release"):
    val manager = SnapshotManager(tempRoot("hold"))
    publishGeneration(manager, 1)

    val held = manager.current().get // retained by us
    assertEquals(held.snapshotId, 1L)
    val heldDir = held.segmentDir

    publishGeneration(manager, 2)

    // close was initiated for the old snapshot: no NEW retains succeed ...
    assertEquals(held.retain(), false)
    assert(!held.isClosed, "held snapshot must stay open while retained")

    // ... but our existing retention still scans correctly against gen 1
    val sink = CollectSink()
    held.scanReferences(RefGroupOrd(0), TargetBitset.all(1), sink)
    assertEquals(sink.out.toVector, Vector(SunkOcc(0, 0, 1, Span.pack(1, 0), Span.pack(1, 4), 0)))
    assertEquals(held.symbolAt(DocOrd(0), 1, 2).map(_.span), Some(Span(1, 0, 1, 4)))
    assertEquals(held.semanticSymbolOf(SymbolOrd(0)), "gen/Sym1.")

    // the new current serves gen 2
    manager.withCurrent { s =>
      assertEquals(s.snapshotId, 2L)
      val sink2 = CollectSink()
      s.scanReferences(RefGroupOrd(0), TargetBitset.all(1), sink2)
      assertEquals(sink2.out.toVector, Vector(SunkOcc(0, 0, 2, Span.pack(1, 0), Span.pack(1, 5), 0)))
    }

    // not deletable while we still hold it
    assertEquals(manager.deleteSuperseded(), Nil)
    assert(Files.isDirectory(heldDir))

    held.release()
    assert(held.isClosed, "last release must close the arena")

    // any mmap access now hits the closed arena
    intercept[IllegalStateException](held.symbolAt(DocOrd(0), 1, 2))
    intercept[IllegalStateException] {
      val s = CollectSink()
      held.scanReferences(RefGroupOrd(0), TargetBitset.all(1), s)
    }
    assertEquals(held.retain(), false)

    // now the compactor hook may delete the superseded directory
    assertEquals(manager.deleteSuperseded(), List(heldDir))
    assert(!Files.exists(heldDir))
    manager.close()

  test("release below zero is rejected"):
    val manager = SnapshotManager(tempRoot("neg"))
    publishGeneration(manager, 1)
    val s = manager.current().get // refs: manager + us
    publishGeneration(manager, 2) // supersedes s, manager drops its ref
    s.release() // drains to zero, closes
    assert(s.isClosed)
    intercept[IllegalStateException](s.release())
    manager.close()

  test("manager close drains the current snapshot"):
    val manager = SnapshotManager(tempRoot("close"))
    publishGeneration(manager, 1)
    val s = manager.current().get
    manager.close()
    assertEquals(manager.current(), None)
    assert(!s.isClosed, "still retained by the test")
    s.release()
    assert(s.isClosed)

  test("concurrent readers across publishes never observe a closed snapshot"):
    val manager = SnapshotManager(tempRoot("stress"))
    publishGeneration(manager, 1)

    val threads = 4
    val errors = new java.util.concurrent.ConcurrentLinkedQueue[Throwable]()
    val stop = new java.util.concurrent.atomic.AtomicBoolean(false)
    val scans = new AtomicInteger(0)
    val started = new CountDownLatch(threads)
    val body: Runnable = () =>
      started.countDown()
      try
        while !stop.get() do
          manager.withCurrent { snap =>
            val gen = snap.epochOf(DocOrd(0))
            val sink = CollectSink()
            snap.scanReferences(RefGroupOrd(0), TargetBitset.all(1), sink)
            // one fresh occurrence per generation, span width encodes the generation
            if sink.out.length != 1 || sink.out(0).packedEnd != Span.pack(1, 3 + gen) then
              throw AssertionError(s"inconsistent scan for generation $gen: ${sink.out}")
            scans.incrementAndGet()
          }
      catch case e: Throwable => errors.add(e)
    val workers = (1 to threads).map { _ =>
      val t = new Thread(body)
      t.start()
      t
    }
    assert(started.await(5, TimeUnit.SECONDS))
    for gen <- 2 to 10 do
      publishGeneration(manager, gen)
      Thread.sleep(20)
    stop.set(true)
    workers.foreach(_.join(5000))
    assert(errors.isEmpty, s"concurrent readers failed: ${errors.peek()}")
    assert(scans.get() > 0, "stress test performed no scans")

    manager.close()
    // all retired snapshots have drained: everything but the last segment is deletable
    val deleted = manager.deleteSuperseded()
    assertEquals(deleted.size, 9)
    val remaining =
      import scala.jdk.CollectionConverters.*
      val stream = Files.list(manager.segmentsDir)
      try stream.iterator().asScala.map(_.getFileName.toString).toList.sorted
      finally stream.close()
    assertEquals(remaining, List("segment-000010"))
