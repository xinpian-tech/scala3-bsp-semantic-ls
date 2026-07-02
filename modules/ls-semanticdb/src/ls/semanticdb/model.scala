package ls.semanticdb

/** Raw SemanticDB messages exactly as decoded from the protobuf wire format,
  * before any normalization. Field subset per plan section 20 Phase 3: only
  * what workspace symbol / references / rename need.
  */

/** SemanticDB `SymbolOccurrence.Role` codes (semanticdb.proto). */
object SdbRole:
  final val UnknownRole = 0
  final val Reference = 1
  final val Definition = 2

/** SemanticDB `Language` codes (semanticdb.proto). */
object SdbLanguage:
  final val Unknown = 0
  final val Scala = 1
  final val Java = 2
  final val Protobuf = 3

  def name(code: Int): String = code match
    case Scala => "scala"
    case Java => "java"
    case Protobuf => "protobuf"
    case _ => "unknown"

/** SemanticDB `Range`: zero-based, end-exclusive character, same convention
  * as LSP. All four fields are plain `int32` on the wire (checked against
  * scalameta semanticdb.proto: they are NOT `sint32`, so no zigzag).
  */
final case class SdbRange(
    startLine: Int,
    startCharacter: Int,
    endLine: Int,
    endCharacter: Int
)

/** SemanticDB `SymbolInformation` subset. */
final case class SdbSymbolInfo(
    symbol: String,
    kindCode: Int,
    properties: Int,
    displayName: String,
    overriddenSymbols: Vector[String]
)

/** SemanticDB `SymbolOccurrence`. `range` is optional on the wire. */
final case class SdbOccurrence(
    range: Option[SdbRange],
    symbol: String,
    roleCode: Int
)

/** SemanticDB `TextDocument` subset. Diagnostics and synthetics payloads are
  * skipped during decoding without materialization.
  */
final case class SdbDocument(
    schema: Int,
    uri: String,
    text: String,
    md5: String,
    languageCode: Int,
    symbols: Vector[SdbSymbolInfo],
    occurrences: Vector[SdbOccurrence]
)

/** SemanticDB `TextDocuments`, the root message of a `.semanticdb` file. */
final case class SdbDocuments(documents: Vector[SdbDocument])
