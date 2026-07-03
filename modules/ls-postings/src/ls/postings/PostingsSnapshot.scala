package ls.postings

import java.nio.file.Path
import java.util.concurrent.atomic.{AtomicBoolean, AtomicInteger}

import ls.index.*

/** Complete [[IndexSnapshot]] implementation over one mapped v1 segment
  * ([[SegmentReader]]) plus heap-side dictionaries built once at open.
  *
  * ==Reference-counting state machine==
  *
  * State lives in one [[AtomicInteger]] `refs` plus a monotonic `superseded`
  * flag:
  *
  *   - `refs` starts at 1: the creator (normally [[SnapshotManager]]) owns
  *     the initial reference.
  *   - `retain()`: CAS loop `c -> c + 1`, refusing when `superseded` is set
  *     or `c == 0`. A successful CAS from `c >= 1` proves the arena was open
  *     at that instant and stays open until the matching `release()`,
  *     because the count can only reach 0 after every hand-out is released
  *     and can never leave 0 again (retain refuses `c == 0`).
  *   - `markSuperseded()` is called exactly once, before the creator drops
  *     its initial reference; it only flips the flag, it never closes.
  *   - `release()`: `decrementAndGet`; the thread that observes 0 with
  *     `superseded` set closes the reader arena. Since `superseded` is set
  *     before the creator's release, the drain-to-zero thread can never miss
  *     the flag, and since no retain succeeds after 0, close happens exactly
  *     once.
  *
  * Consequence for readers: a snapshot retained before a publish stays fully
  * usable across the publish; new `retain()` calls fail once close has been
  * initiated (superseded), which makes [[SnapshotManager.current]] retry on
  * the freshly published snapshot.
  */
final class PostingsSnapshot private[postings] (val reader: SegmentReader) extends IndexSnapshot:
  private val refs = new AtomicInteger(1)
  private val superseded = new AtomicBoolean(false)
  @volatile private var closed = false

  // --- heap dictionaries built once at open (index-model dense-ordinal contract) ---
  private val uris: Array[String] =
    Array.tabulate(reader.docCount)(reader.uriOfDoc)
  private val uriToOrd: java.util.HashMap[String, Integer] =
    val m = new java.util.HashMap[String, Integer](reader.docCount * 2)
    var d = 0
    while d < reader.docCount do
      m.put(uris(d), d)
      d += 1
    m
  private val docTargetOrds: Array[Int] =
    Array.tabulate(reader.docCount)(reader.targetOrdOfDoc)
  private val docFlags: Array[Int] =
    Array.tabulate(reader.docCount)(reader.docFlagsOf)
  private val docEpochs: Array[Int] =
    Array.tabulate(reader.docCount)(reader.epochOf)
  private val targetIds: Array[Long] =
    Array.tabulate(reader.targetCount)(reader.targetIdOf)
  private val targetIdToOrd: java.util.HashMap[java.lang.Long, Integer] =
    val m = new java.util.HashMap[java.lang.Long, Integer](reader.targetCount * 2)
    var t = 0
    while t < reader.targetCount do
      m.put(targetIds(t), t)
      t += 1
    m

  def segmentDir: Path = reader.segmentDir

  override def snapshotId: Long = reader.segmentId

  // --- lifecycle ---

  override def retain(): Boolean =
    if superseded.get() then false
    else
      var ok = false
      var done = false
      while !done do
        val c = refs.get()
        if c <= 0 then
          done = true // drained: arena is (being) closed
        else if refs.compareAndSet(c, c + 1) then
          ok = true
          done = true
      ok

  override def release(): Unit =
    val c = refs.decrementAndGet()
    if c < 0 then
      refs.incrementAndGet()
      throw IllegalStateException(s"snapshot $snapshotId released more times than retained")
    if c == 0 then
      // Reaching 0 requires the creator reference to be gone; the manager
      // protocol sets `superseded` before dropping it, so close-on-drain
      // happens here exactly once.
      closed = true
      reader.close()

  /** Initiates close-on-drain. Called exactly once by the publisher before it
    * drops its creator reference.
    */
  private[postings] def markSuperseded(): Unit = superseded.set(true)

  /** True once the backing arena has been closed (all references drained). */
  def isClosed: Boolean = closed

  // --- dictionaries ---

  override def docCount: Int = reader.docCount
  override def targetCount: Int = reader.targetCount

  override def uriOf(doc: DocOrd): String = uris(doc.ord)

  override def docOrdOf(uri: String): Option[DocOrd] =
    val v = uriToOrd.get(uri)
    if v eq null then None else Some(DocOrd(v.intValue()))

  override def epochOf(doc: DocOrd): Int = docEpochs(doc.ord)
  override def targetOrdOf(doc: DocOrd): TargetOrd = TargetOrd(docTargetOrds(doc.ord))
  override def targetIdOf(target: TargetOrd): TargetId = TargetId(targetIds(target.ord))

  override def targetOrdOfId(id: TargetId): Option[TargetOrd] =
    val v = targetIdToOrd.get(id.value)
    if v eq null then None else Some(TargetOrd(v.intValue()))

  override def isGenerated(doc: DocOrd): Boolean =
    (docFlags(doc.ord) & SegmentFormat.DocFlagGenerated) != 0

  override def isReadonly(doc: DocOrd): Boolean =
    (docFlags(doc.ord) & SegmentFormat.DocFlagReadonly) != 0

  // --- symbol resolution ---

  override def symbolAt(doc: DocOrd, line: Int, character: Int): Option[OccurrenceHit] =
    reader.symbolAt(doc.ord, line, character)

  override def semanticSymbolOf(sym: SymbolOrd): String =
    reader.semanticSymbolOf(sym.ord)

  override def symbolOrdOf(semanticSymbol: String): Option[SymbolOrd] =
    val ord = reader.findSymbolOrd(semanticSymbol)
    if ord < 0 then None else Some(SymbolOrd(ord))

  override def refGroupOf(sym: SymbolOrd): Option[RefGroupOrd] =
    val g = reader.refGroupOrdOf(sym.ord)
    if g < 0 then None else Some(RefGroupOrd(g))

  override def refGroupSymbols(group: RefGroupOrd): Vector[String] =
    reader.refGroupSymbols(group.ord)

  override def renameGroupOf(sym: SymbolOrd): Option[RenameGroupOrd] =
    val g = reader.renameGroupOrdOf(sym.ord)
    if g < 0 then None else Some(RenameGroupOrd(g))

  override def definitionTargetOf(sym: SymbolOrd): Option[TargetOrd] =
    val t = reader.defTargetOrdOf(sym.ord)
    if t < 0 then None else Some(TargetOrd(t))

  // --- postings scans ---

  override def scanReferences(group: RefGroupOrd, allowed: TargetBitset, sink: OccurrenceSink): Unit =
    reader.scanRefGroup(group.ord, allowed, sink)

  override def scanDefinitions(group: RefGroupOrd, sink: OccurrenceSink): Unit =
    reader.scanDefGroup(group.ord, sink)

  override def scanRenameEdits(group: RenameGroupOrd, sink: OccurrenceSink): Unit =
    reader.scanRenameGroup(group.ord, sink)

  override def scanDocOccurrences(doc: DocOrd, sink: OccurrenceSink): Unit =
    reader.scanDoc(doc.ord, sink)

  override def scanDocEditable(doc: DocOrd, sink: OccurrenceSink): Unit =
    reader.scanDoc(doc.ord, sink, requireEditable = true)

  override def renameProfileOf(group: RenameGroupOrd): RenameProfile =
    reader.renameProfileOf(group.ord)
