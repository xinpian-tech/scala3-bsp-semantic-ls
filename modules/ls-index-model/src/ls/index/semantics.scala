package ls.index

/** Occurrence role, mirroring SemanticDB SymbolOccurrence.Role. */
enum Role:
  case Reference, Definition

/** Bit flags stored per occurrence in postings. Exact facts, never guesses. */
object OccFlags:
  inline val Definition = 1 << 0
  inline val Editable = 1 << 1
  inline val Generated = 1 << 2
  inline val Readonly = 1 << 3
  inline val Synthetic = 1 << 4

  def has(flags: Int, bit: Int): Boolean = (flags & bit) != 0

/** Identity of a SemanticDB symbol. Global symbols are unique per universe;
  * local symbols are only meaningful together with their document.
  */
final case class SymbolKey(semanticSymbol: String, localDoc: Option[DocId]):
  def isLocal: Boolean = localDoc.isDefined

object SymbolKey:
  def global(sym: String): SymbolKey = SymbolKey(sym, None)
  def local(sym: String, doc: DocId): SymbolKey = SymbolKey(sym, Some(doc))

/** SemanticDB SymbolInformation.Kind subset we materialize. Values follow the
  * SemanticDB spec numbering so SQLite rows stay debuggable.
  */
enum SymKind(val code: Int):
  case UnknownKind extends SymKind(0)
  case LocalValue extends SymKind(19)
  case LocalVariable extends SymKind(20)
  case Method extends SymKind(3)
  case Constructor extends SymKind(21)
  case Macro extends SymKind(6)
  case Type extends SymKind(7)
  case Parameter extends SymKind(8)
  case SelfParameter extends SymKind(17)
  case TypeParameter extends SymKind(9)
  case Object extends SymKind(10)
  case Package extends SymKind(11)
  case PackageObject extends SymKind(12)
  case Class extends SymKind(13)
  case Trait extends SymKind(14)
  case Interface extends SymKind(18)
  case Field extends SymKind(15)

object SymKind:
  def fromCode(code: Int): SymKind =
    values.find(_.code == code).getOrElse(UnknownKind)

/** SemanticDB SymbolInformation.Property bit mask (spec numbering). */
object SymProps:
  inline val Abstract = 0x4
  inline val Final = 0x8
  inline val Sealed = 0x10
  inline val Implicit = 0x20
  inline val Lazy = 0x40
  inline val Case = 0x80
  inline val Covariant = 0x100
  inline val Contravariant = 0x200
  inline val Val = 0x400
  inline val Var = 0x800
  inline val Static = 0x1000
  inline val Primary = 0x2000
  inline val Enum = 0x4000
  inline val Default = 0x8000
  inline val Given = 0x10000
  inline val Inline = 0x20000
  inline val Open = 0x40000
  inline val Transparent = 0x80000
  inline val Infix = 0x100000
  inline val Opaque = 0x200000

/** Normalized SymbolInformation extracted from SemanticDB. */
final case class SymbolInfo(
    key: SymbolKey,
    displayName: String,
    ownerName: Option[String],
    packageName: Option[String],
    kind: SymKind,
    properties: Int,
    overriddenSymbols: List[String]
)

/** Normalized SymbolOccurrence extracted from SemanticDB. */
final case class Occurrence(
    key: SymbolKey,
    span: Span,
    role: Role,
    synthetic: Boolean = false
)

/** One normalized SemanticDB TextDocument, the unit of ingest. */
final case class NormalizedDocument(
    uri: String,
    md5: String,
    schemaVersion: Int,
    language: String,
    symbols: Vector[SymbolInfo],
    occurrences: Vector[Occurrence]
)
