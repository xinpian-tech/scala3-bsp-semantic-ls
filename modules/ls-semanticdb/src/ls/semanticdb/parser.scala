package ls.semanticdb

import java.nio.file.{Files, Path}

/** Streaming-friendly decoder for the SemanticDB `TextDocuments` root message.
  *
  * Field numbers below were derived from the authoritative
  * `scalameta/semanticdb/semanticdb.proto` (fetched from the scalameta main
  * branch on 2026-07-02), not from memory:
  *
  * {{{
  * message TextDocuments { repeated TextDocument documents = 1; }
  * message TextDocument {
  *   reserved 4, 8, 9;
  *   Schema schema = 1;            string uri = 2;   string text = 3;
  *   string md5 = 11;              Language language = 10;
  *   repeated SymbolInformation symbols = 5;
  *   repeated SymbolOccurrence occurrences = 6;
  *   repeated Diagnostic diagnostics = 7;   // skipped
  *   repeated Synthetic synthetics = 12;    // skipped
  *   string build_target = 13;              // skipped
  * }
  * message SymbolInformation {
  *   string symbol = 1; Kind kind = 3; int32 properties = 4;
  *   string display_name = 5; repeated string overridden_symbols = 19;
  *   // language=16, signature=17, annotations=13, access=18,
  *   // documentation=20 are skipped
  * }
  * message SymbolOccurrence { Range range = 1; string symbol = 2; Role role = 3; }
  * message Range {
  *   int32 start_line = 1; int32 start_character = 2;
  *   int32 end_line = 3;   int32 end_character = 4;
  * }
  * }}}
  *
  * Note: `Range` coordinates are plain `int32` (NOT `sint32`), so plain varint
  * decoding without zigzag is the correct treatment.
  */
object SemanticdbParser:

  private object F:
    // TextDocuments
    final val Documents = 1
    // TextDocument
    final val TdSchema = 1
    final val TdUri = 2
    final val TdText = 3
    final val TdSymbols = 5
    final val TdOccurrences = 6
    final val TdDiagnostics = 7
    final val TdLanguage = 10
    final val TdMd5 = 11
    final val TdSynthetics = 12
    // SymbolInformation
    final val SiSymbol = 1
    final val SiKind = 3
    final val SiProperties = 4
    final val SiDisplayName = 5
    final val SiOverriddenSymbols = 19
    // SymbolOccurrence
    final val SoRange = 1
    final val SoSymbol = 2
    final val SoRole = 3
    // Range
    final val RStartLine = 1
    final val RStartCharacter = 2
    final val REndLine = 3
    final val REndCharacter = 4

  private object Wire:
    final val Varint = 0
    final val LengthDelimited = 2

  /** Parses the whole payload of one `.semanticdb` file. */
  def parseTextDocuments(bytes: Array[Byte]): SdbDocuments =
    val reader = new ProtoReader(bytes)
    val docs = Vector.newBuilder[SdbDocument]
    while reader.hasRemaining do
      val tag = reader.readTag()
      val field = tag >>> 3
      val wire = tag & 7
      field match
        case F.Documents if wire == Wire.LengthDelimited =>
          docs += parseDocument(reader.readMessage())
        case _ => reader.skipField(wire, field)
    SdbDocuments(docs.result())

  def parseFile(path: Path): SdbDocuments =
    parseTextDocuments(Files.readAllBytes(path))

  private def parseDocument(r: ProtoReader): SdbDocument =
    var schema = 0
    var uri = ""
    var text = ""
    var md5 = ""
    var language = 0
    val symbols = Vector.newBuilder[SdbSymbolInfo]
    val occurrences = Vector.newBuilder[SdbOccurrence]
    while r.hasRemaining do
      val tag = r.readTag()
      val field = tag >>> 3
      val wire = tag & 7
      field match
        case F.TdSchema if wire == Wire.Varint => schema = r.readInt32()
        case F.TdUri if wire == Wire.LengthDelimited => uri = r.readString()
        case F.TdText if wire == Wire.LengthDelimited => text = r.readString()
        case F.TdMd5 if wire == Wire.LengthDelimited => md5 = r.readString()
        case F.TdLanguage if wire == Wire.Varint => language = r.readInt32()
        case F.TdSymbols if wire == Wire.LengthDelimited =>
          symbols += parseSymbolInfo(r.readMessage())
        case F.TdOccurrences if wire == Wire.LengthDelimited =>
          occurrences += parseOccurrence(r.readMessage())
        // Diagnostics/synthetics payloads are skipped as opaque bytes: the
        // skip is a single offset bump, no per-message materialization.
        case F.TdDiagnostics | F.TdSynthetics => r.skipField(wire, field)
        case _ => r.skipField(wire, field)
    SdbDocument(schema, uri, text, md5, language, symbols.result(), occurrences.result())

  private def parseSymbolInfo(r: ProtoReader): SdbSymbolInfo =
    var symbol = ""
    var kind = 0
    var properties = 0
    var displayName = ""
    val overridden = Vector.newBuilder[String]
    while r.hasRemaining do
      val tag = r.readTag()
      val field = tag >>> 3
      val wire = tag & 7
      field match
        case F.SiSymbol if wire == Wire.LengthDelimited => symbol = r.readString()
        case F.SiKind if wire == Wire.Varint => kind = r.readInt32()
        case F.SiProperties if wire == Wire.Varint => properties = r.readInt32()
        case F.SiDisplayName if wire == Wire.LengthDelimited => displayName = r.readString()
        case F.SiOverriddenSymbols if wire == Wire.LengthDelimited =>
          overridden += r.readString()
        case _ => r.skipField(wire, field)
    SdbSymbolInfo(symbol, kind, properties, displayName, overridden.result())

  private def parseOccurrence(r: ProtoReader): SdbOccurrence =
    var range: Option[SdbRange] = None
    var symbol = ""
    var role = 0
    while r.hasRemaining do
      val tag = r.readTag()
      val field = tag >>> 3
      val wire = tag & 7
      field match
        case F.SoRange if wire == Wire.LengthDelimited =>
          range = Some(parseRange(r.readMessage()))
        case F.SoSymbol if wire == Wire.LengthDelimited => symbol = r.readString()
        case F.SoRole if wire == Wire.Varint => role = r.readInt32()
        case _ => r.skipField(wire, field)
    SdbOccurrence(range, symbol, role)

  private def parseRange(r: ProtoReader): SdbRange =
    var startLine = 0
    var startCharacter = 0
    var endLine = 0
    var endCharacter = 0
    while r.hasRemaining do
      val tag = r.readTag()
      val field = tag >>> 3
      val wire = tag & 7
      field match
        case F.RStartLine if wire == Wire.Varint => startLine = r.readInt32()
        case F.RStartCharacter if wire == Wire.Varint => startCharacter = r.readInt32()
        case F.REndLine if wire == Wire.Varint => endLine = r.readInt32()
        case F.REndCharacter if wire == Wire.Varint => endCharacter = r.readInt32()
        case _ => r.skipField(wire, field)
    SdbRange(startLine, startCharacter, endLine, endCharacter)
