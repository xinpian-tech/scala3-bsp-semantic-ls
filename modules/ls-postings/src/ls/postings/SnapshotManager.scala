package ls.postings

import java.nio.file.{Files, Path}
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.atomic.AtomicReference

import scala.jdk.CollectionConverters.*

/** Owns the single published [[PostingsSnapshot]] (plan 9.1) and the postings
  * directory layout under `root`:
  *
  * {{{
  * root/
  *   tmp-<id>/                (writer scratch, crash debris only)
  *   segments/segment-NNNNNN/ (immutable published segments)
  * }}}
  *
  * `publish` swaps the [[AtomicReference]] and schedules the previous snapshot
  * for close-on-drain: it is marked superseded (so no new retains succeed)
  * and the manager's creator reference is released; the mmap arena closes
  * when the last in-flight reader releases. Superseded snapshots are queued
  * for [[deleteSuperseded]], the v1 compactor job, which physically removes
  * segment directories whose arena has fully closed.
  */
final class SnapshotManager(val root: Path):
  private val currentRef = new AtomicReference[PostingsSnapshot | Null](null)
  private val retired = new ConcurrentLinkedQueue[PostingsSnapshot]()

  def segmentsDir: Path = root.resolve("segments")

  /** Next segment id: 1 + the highest id of any existing segment directory
    * (including crash debris considered published), starting from 1.
    */
  def nextSegmentId(): Long =
    val dir = segmentsDir
    if !Files.isDirectory(dir) then 1L
    else
      val stream = Files.list(dir)
      try
        val ids = stream
          .iterator()
          .asScala
          .flatMap { p =>
            val name = p.getFileName.toString
            if name.startsWith("segment-") then name.stripPrefix("segment-").toLongOption
            else None
          }
          .toList
        if ids.isEmpty then 1L else ids.max + 1L
      finally stream.close()

  /** Wraps the reader in a snapshot and publishes it as current. The previous
    * snapshot (if any) is superseded and its creator reference dropped.
    * Returns the published snapshot; the manager keeps the creator reference,
    * callers who need to hold it must [[PostingsSnapshot.retain]] via
    * [[current]].
    */
  def publish(reader: SegmentReader): PostingsSnapshot =
    val snapshot = new PostingsSnapshot(reader)
    val old = currentRef.getAndSet(snapshot)
    if old ne null then
      old.markSuperseded()
      retired.add(old)
      old.release()
    snapshot

  /** Returns the current snapshot already retained (caller MUST release), or
    * None when nothing is published. Retries when it races with a publish
    * superseding the snapshot it just read.
    */
  def current(): Option[PostingsSnapshot] =
    var result: Option[PostingsSnapshot] = None
    var done = false
    while !done do
      val s = currentRef.get()
      if s eq null then done = true
      else if s.retain() then
        result = Some(s)
        done = true
      // else: s was superseded between get() and retain(); currentRef has
      // moved on (supersede happens only after the swap), so retry.
    result

  /** Loan pattern: runs `f` over a retained current snapshot, releasing it
    * afterwards. None when no snapshot is published.
    */
  def withCurrent[A](f: PostingsSnapshot => A): Option[A] =
    current().map { s =>
      try f(s)
      finally s.release()
    }

  /** v1 compactor job: deletes segment directories of superseded snapshots
    * whose arenas have fully closed (all readers drained). Snapshots still
    * held by readers stay queued for a later pass. Returns deleted dirs.
    */
  def deleteSuperseded(): List[Path] =
    val deleted = List.newBuilder[Path]
    val requeue = List.newBuilder[PostingsSnapshot]
    var s = retired.poll()
    while s ne null do
      if s.isClosed then
        SegmentWriter.deleteRecursively(s.segmentDir)
        deleted += s.segmentDir
      else requeue += s
      s = retired.poll()
    requeue.result().foreach(retired.add)
    deleted.result()

  /** Startup janitor: removes writer `tmp-*` debris directly under `root` and
    * any `segment-*` directory that is not `keep` (the manifest-active segment
    * as recorded in SQLite). The kept path is never deleted, even when it could
    * not be opened or is absent (a diverged manifest). When `keep` is None
    * every segment directory is treated as orphan debris. Returns the deleted
    * directories.
    *
    * This complements [[deleteSuperseded]]: that one reclaims in-memory retired
    * snapshots whose arenas have drained; this one reclaims on-disk leftovers
    * from a previous process that have no live snapshot.
    */
  def cleanupOrphans(keep: Option[Path]): List[Path] =
    val keepNorm = keep.map(_.toAbsolutePath.normalize)
    val deleted = List.newBuilder[Path]
    def listChildren(dir: Path): List[Path] =
      if !Files.isDirectory(dir) then Nil
      else
        val stream = Files.list(dir)
        try stream.iterator().asScala.toList
        finally stream.close()
    // writer scratch debris under root
    for p <- listChildren(root) if p.getFileName.toString.startsWith("tmp-") do
      SegmentWriter.deleteRecursively(p)
      deleted += p
    // orphan published segment directories
    for
      p <- listChildren(segmentsDir)
      if p.getFileName.toString.startsWith("segment-")
      if !keepNorm.contains(p.toAbsolutePath.normalize)
    do
      SegmentWriter.deleteRecursively(p)
      deleted += p
    deleted.result()

  /** Unpublishes and schedules the current snapshot for close-on-drain. The
    * segment directory is not deleted (it is the durable index).
    */
  def close(): Unit =
    val old = currentRef.getAndSet(null)
    if old ne null then
      old.markSuperseded()
      old.release()
