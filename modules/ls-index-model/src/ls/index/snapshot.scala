package ls.index

/** Consistency level a query path demands (plan section 10). */
enum ConsistencyLevel:
  case BestEffort // workspace symbol
  case FreshPreferred // references
  case FreshRequired // rename

/** Primitive-argument callback for postings scans; avoids per-occurrence
  * allocation on the hot path.
  */
trait OccurrenceSink:
  def accept(
      docOrd: Int,
      targetOrd: Int,
      docEpoch: Int,
      packedStart: Int,
      packedEnd: Int,
      flags: Int
  ): Unit

/** Hit from symbol-at-cursor resolution over doc postings. */
final case class OccurrenceHit(
    symbolOrd: SymbolOrd,
    docOrd: DocOrd,
    span: Span,
    role: Role,
    flags: Int
)

/** An immutable, reference-counted view over one postings generation plus its
  * snapshot dictionaries. Readers retain before use and release after; the
  * backing mmap arena closes only when the count drops to zero after the
  * snapshot has been superseded.
  */
trait IndexSnapshot:
  def snapshotId: Long

  /** Returns false when the snapshot is already closed and must not be used. */
  def retain(): Boolean
  def release(): Unit

  // --- dictionaries ---
  def docCount: Int
  def targetCount: Int
  def uriOf(doc: DocOrd): String
  def docOrdOf(uri: String): Option[DocOrd]
  def epochOf(doc: DocOrd): Int
  def targetOrdOf(doc: DocOrd): TargetOrd
  def targetIdOf(target: TargetOrd): TargetId
  def targetOrdOfId(id: TargetId): Option[TargetOrd]
  def isGenerated(doc: DocOrd): Boolean
  def isReadonly(doc: DocOrd): Boolean

  // --- symbol resolution ---
  /** Exact occurrence covering the position, from doc postings. */
  def symbolAt(doc: DocOrd, line: Int, character: Int): Option[OccurrenceHit]
  def semanticSymbolOf(sym: SymbolOrd): String
  def symbolOrdOf(semanticSymbol: String): Option[SymbolOrd]
  def refGroupOf(sym: SymbolOrd): Option[RefGroupOrd]
  def renameGroupOf(sym: SymbolOrd): Option[RenameGroupOrd]
  /** Target that defines the symbol, when known in this snapshot. */
  def definitionTargetOf(sym: SymbolOrd): Option[TargetOrd]

  // --- postings scans (exact, epoch-filtered by the caller via sink) ---
  def scanReferences(group: RefGroupOrd, allowed: TargetBitset, sink: OccurrenceSink): Unit
  def scanDefinitions(group: RefGroupOrd, sink: OccurrenceSink): Unit
  def scanRenameEdits(group: RenameGroupOrd, sink: OccurrenceSink): Unit
  def scanDocOccurrences(doc: DocOrd, sink: OccurrenceSink): Unit
  /** The editable subset of [[scanDocOccurrences]]: only occurrences carrying
    * the [[OccFlags.Editable]] bit, for rename-scoped per-document work.
    * Occurrences without that bit (generated, readonly, and dependency doc
    * occurrences) are excluded. The filter is purely on the Editable bit: it
    * does not additionally drop a synthetic occurrence that is itself editable.
    */
  def scanDocEditable(doc: DocOrd, sink: OccurrenceSink): Unit

  def renameProfileOf(group: RenameGroupOrd): RenameProfile

/** Handle used by request code: loan-pattern over retain/release. */
object IndexSnapshot:
  inline def using[A](snapshot: IndexSnapshot)(inline body: IndexSnapshot => A): A =
    if !snapshot.retain() then
      throw IllegalStateException(s"snapshot ${snapshot.snapshotId} already closed")
    try body(snapshot)
    finally snapshot.release()
