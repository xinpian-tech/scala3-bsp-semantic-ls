package ls.pc.host.codec

import ls.pc.host.codec.Codec.{Reader, Writer}

/** Island-side owned mirrors of every op payload, with lossless flat
  * encode/decode over the [[Codec]] buffer format — the byte-for-byte
  * counterpart of the Rust `ls-pc-abi` `payloads` module. Covers the requests
  * the island consumes (`register_target`/`did_open`/`did_change`/position/
  * resolve), the carrier-free responses it produces (`hover`/`definition`/
  * `type_definition`/`prepare_rename`/`plugin_status` and the
  * `symbol_definition`/`search_methods`/`definition_source_toplevels`
  * callbacks), the LSP4J-carrier responses
  * (`completion`/`completion_resolve`/`signature_help`) at the resolved LSP4J
  * 1.0.0 field surface, and the ABI v2 payload-query carriers (`inlay_hints`/
  * `semantic_tokens`/`selection_range`/`code_action`/`auto_imports`/
  * `pc_diagnostics`/`folding_range`). All 27 payload kinds are implemented
  * below.
  */
object Payloads:
  // Payload kinds (the envelope tag; a decode against the wrong kind is rejected).
  val KindTargetConfig: Int = 1
  val KindDidOpen: Int = 2
  val KindDidChange: Int = 3
  val KindPosition: Int = 4
  val KindResolveParams: Int = 5
  val KindCompletionList: Int = 6
  val KindCompletionItem: Int = 7
  val KindHover: Int = 8
  val KindSignatureHelp: Int = 9
  val KindDefinition: Int = 10
  val KindPrepareRename: Int = 11
  val KindPluginStatus: Int = 12
  val KindLocations: Int = 13
  val KindMethodHits: Int = 14
  val KindInlayHintParams: Int = 15
  val KindInlayHints: Int = 16
  val KindUriParams: Int = 17
  val KindSemanticTokens: Int = 18
  val KindSelectionRangeParams: Int = 19
  val KindSelectionRanges: Int = 20
  val KindCodeActionParams: Int = 21
  val KindCodeActionEdits: Int = 22
  val KindAutoImportParams: Int = 23
  val KindAutoImports: Int = 24
  val KindPcDiagnostics: Int = 25
  val KindFoldingRanges: Int = 26
  val KindToplevels: Int = 27

  /** `DefinitionOrigin` ordinals (mirror the Scala `enum DefinitionOrigin`). */
  object Origin:
    val Workspace: Int = 0
    val Synthetic: Int = 1
    val Plugin: Int = 2

  /** `code_action` action ids (mirror the Rust `code_action_id` constants). */
  object CodeActionId:
    val ConvertToNamedArguments: Int = 0
    val ImplementAbstractMembers: Int = 1
    val ExtractMethod: Int = 2
    val InlineValue: Int = 3
    val InsertInferredType: Int = 4
    val InsertInferredMethod: Int = 5
    val ConvertToNamedLambdaParameters: Int = 6

  /** `folding_range` kind ordinals (mirror the Rust `folding_kind` constants;
    * `None` means the range carries no LSP folding kind).
    */
  object FoldingKind:
    val None: Int = 0
    val Comment: Int = 1
    val Imports: Int = 2
    val Region: Int = 3

  // -------------------------------------------------------------------------
  // Shared value types.
  // -------------------------------------------------------------------------

  /** A zero-based `[start, end)` range (UTF-16 positions, as LSP). */
  final case class Rng(startLine: Int, startCharacter: Int, endLine: Int, endCharacter: Int)

  object Rng:
    def write(w: Writer, r: Rng): Unit =
      w.range(r.startLine, r.startCharacter, r.endLine, r.endCharacter)

    def read(r: Reader): Rng =
      val (sl, sc, el, ec) = r.range()
      Rng(sl, sc, el, ec)

    def writeOpt(w: Writer, range: Option[Rng]): Unit = range match
      case Some(rng) =>
        w.u32(1)
        write(w, rng)
      case None =>
        w.u32(0)
        w.range(0, 0, 0, 0)

    def readOpt(r: Reader): Option[Rng] =
      val present = r.u32()
      val rng = read(r)
      if present == 0 then None else Some(rng)

  /** A zero-based `line`/`character` position (UTF-16 code units, as LSP). */
  final case class Pos(line: Int, character: Int)

  object Pos:
    def write(w: Writer, p: Pos): Unit =
      w.u32(p.line)
      w.u32(p.character)

    def read(r: Reader): Pos =
      val line = r.u32()
      val character = r.u32()
      Pos(line, character)

    /** Fixed-shape optional (mirrors [[Rng.writeOpt]]): a presence flag then
      * the two words, zeroed when absent.
      */
    def writeOpt(w: Writer, pos: Option[Pos]): Unit = pos match
      case Some(p) =>
        w.u32(1)
        write(w, p)
      case None =>
        w.u32(0)
        w.u32(0)
        w.u32(0)

    def readOpt(r: Reader): Option[Pos] =
      val present = r.u32()
      val pos = read(r)
      if present == 0 then None else Some(pos)

  /** A definition/reference location plus its `DefinitionOrigin` ordinal. */
  final case class Location(uri: String, range: Rng, origin: Int)

  object Location:
    def write(w: Writer, l: Location): Unit =
      w.str(l.uri)
      Rng.write(w, l.range)
      w.u32(l.origin)

    def read(r: Reader): Location =
      val uri = r.str()
      val range = Rng.read(r)
      val origin = r.u32()
      Location(uri, range, origin)

  /** Markup content (LSP4J `MarkupContent`). */
  final case class MarkupContent(kind: String, value: String)

  object MarkupContent:
    def write(w: Writer, m: MarkupContent): Unit =
      w.str(m.kind)
      w.str(m.value)

    def read(r: Reader): MarkupContent =
      val kind = r.str()
      val value = r.str()
      MarkupContent(kind, value)

  /** A `textDocument/hover` marked-string element (LSP4J `Either<String, MarkedString>`). */
  enum MarkedStringItem:
    case Plain(value: String)
    case Marked(language: String, value: String)

  /** Hover contents (LSP4J `Either<MarkupContent, List<MarkedString>>`). */
  enum HoverContents:
    case Markup(content: MarkupContent)
    case Marked(items: Seq[MarkedStringItem])

  object HoverContents:
    def write(w: Writer, c: HoverContents): Unit = c match
      case HoverContents.Markup(m) =>
        w.u32(0)
        MarkupContent.write(w, m)
      case HoverContents.Marked(items) =>
        w.u32(1)
        w.u32(items.length)
        items.foreach {
          case MarkedStringItem.Plain(s) =>
            w.u32(0)
            w.str(s)
          case MarkedStringItem.Marked(language, value) =>
            w.u32(1)
            w.str(language)
            w.str(value)
        }

    def read(r: Reader): HoverContents = r.u32() match
      case 0 => HoverContents.Markup(MarkupContent.read(r))
      case 1 =>
        val n = r.count()
        val items = Vector.newBuilder[MarkedStringItem]
        var i = 0
        while i < n do
          items += (r.u32() match
            case 0 => MarkedStringItem.Plain(r.str())
            case 1 =>
              val language = r.str()
              val value = r.str()
              MarkedStringItem.Marked(language, value)
            case tag => throw CodecException(s"invalid marked string variant tag $tag"))
          i += 1
        HoverContents.Marked(items.result())
      case tag => throw CodecException(s"invalid hover contents variant tag $tag")

  /** Hover contents plus an optional range (LSP4J `Hover`). */
  final case class Hover(contents: HoverContents, range: Option[Rng])

  // -------------------------------------------------------------------------
  // Requests (the island decodes these).
  // -------------------------------------------------------------------------

  /** `register_target` payload (mirrors `PcWorkerTargetParams`). */
  final case class TargetConfig(
      bspId: String,
      scalaVersion: String,
      classpath: Seq[String],
      scalacOptions: Seq[String],
      sourceDirs: Seq[String]
  ):
    def encode(): Array[Byte] =
      val w = Writer()
      w.str(bspId)
      w.str(scalaVersion)
      w.strList(classpath)
      w.strList(scalacOptions)
      w.strList(sourceDirs)
      w.finish(KindTargetConfig)

  object TargetConfig:
    def decode(buf: Array[Byte]): TargetConfig =
      val r = Reader(buf, KindTargetConfig)
      val bspId = r.str()
      val scalaVersion = r.str()
      val classpath = r.strList()
      val scalacOptions = r.strList()
      val sourceDirs = r.strList()
      r.finish()
      TargetConfig(bspId, scalaVersion, classpath, scalacOptions, sourceDirs)

  /** `did_open` payload. */
  final case class DidOpenParams(targetId: String, uri: String, text: String):
    def encode(): Array[Byte] =
      val w = Writer()
      w.str(targetId)
      w.str(uri)
      w.str(text)
      w.finish(KindDidOpen)

  object DidOpenParams:
    def decode(buf: Array[Byte]): DidOpenParams =
      val r = Reader(buf, KindDidOpen)
      val targetId = r.str()
      val uri = r.str()
      val text = r.str()
      r.finish()
      DidOpenParams(targetId, uri, text)

  /** `did_change` payload. */
  final case class DidChangeParams(uri: String, text: String):
    def encode(): Array[Byte] =
      val w = Writer()
      w.str(uri)
      w.str(text)
      w.finish(KindDidChange)

  object DidChangeParams:
    def decode(buf: Array[Byte]): DidChangeParams =
      val r = Reader(buf, KindDidChange)
      val uri = r.str()
      val text = r.str()
      r.finish()
      DidChangeParams(uri, text)

  /** A position query's params (uri + line/character). */
  final case class PositionParams(uri: String, line: Int, character: Int):
    def encode(): Array[Byte] =
      val w = Writer()
      w.str(uri)
      w.u32(line)
      w.u32(character)
      w.finish(KindPosition)

  object PositionParams:
    def decode(buf: Array[Byte]): PositionParams =
      val r = Reader(buf, KindPosition)
      val uri = r.str()
      val line = r.u32()
      val character = r.u32()
      r.finish()
      PositionParams(uri, line, character)

  // -------------------------------------------------------------------------
  // Responses (the island encodes these).
  // -------------------------------------------------------------------------

  /** A hover response: `None` is a null hover (the PC has nothing at the
    * point), distinct from a present hover with empty contents.
    */
  final case class HoverResult(hover: Option[Hover]):
    def encode(): Array[Byte] =
      val w = Writer()
      hover match
        case Some(h) =>
          w.u32(1)
          HoverContents.write(w, h.contents)
          Rng.writeOpt(w, h.range)
        case None => w.u32(0)
      w.finish(KindHover)

  object HoverResult:
    def decode(buf: Array[Byte]): HoverResult =
      val r = Reader(buf, KindHover)
      val hover =
        if r.u32() != 0 then
          val contents = HoverContents.read(r)
          val range = Rng.readOpt(r)
          Some(Hover(contents, range))
        else None
      r.finish()
      HoverResult(hover)

  /** A `definition`/`type_definition` response: the queried symbol + locations. */
  final case class DefinitionResult(symbol: String, locations: Seq[Location]):
    def encode(): Array[Byte] =
      val w = Writer()
      w.str(symbol)
      w.u32(locations.length)
      locations.foreach(Location.write(w, _))
      w.finish(KindDefinition)

  object DefinitionResult:
    def decode(buf: Array[Byte]): DefinitionResult =
      val r = Reader(buf, KindDefinition)
      val symbol = r.str()
      val n = r.count()
      val locations = Vector.newBuilder[Location]
      var i = 0
      while i < n do
        locations += Location.read(r)
        i += 1
      r.finish()
      DefinitionResult(symbol, locations.result())

  /** The `symbol_definition` callback response: locations only. */
  final case class LocationsResult(locations: Seq[Location]):
    def encode(): Array[Byte] =
      val w = Writer()
      w.u32(locations.length)
      locations.foreach(Location.write(w, _))
      w.finish(KindLocations)

  object LocationsResult:
    def decode(buf: Array[Byte]): LocationsResult =
      val r = Reader(buf, KindLocations)
      val n = r.count()
      val locations = Vector.newBuilder[Location]
      var i = 0
      while i < n do
        locations += Location.read(r)
        i += 1
      r.finish()
      LocationsResult(locations.result())

  /** One workspace method hit of the `search_methods` callback: the defining
    * `file://` uri, the SemanticDB symbol string, the SemanticDB kind code, and
    * the definition span.
    */
  final case class MethodHit(uri: String, symbol: String, kind: Int, range: Rng)

  object MethodHit:
    def write(w: Writer, h: MethodHit): Unit =
      w.str(h.uri)
      w.str(h.symbol)
      w.i32(h.kind)
      Rng.write(w, h.range)

    def read(r: Reader): MethodHit =
      val uri = r.str()
      val symbol = r.str()
      val kind = r.i32()
      val range = Rng.read(r)
      MethodHit(uri, symbol, kind, range)

  /** The `search_methods` callback response: workspace method hits only
    * (mirrors [[LocationsResult]]'s shape for the definition callback).
    */
  final case class MethodHitsResult(hits: Seq[MethodHit]):
    def encode(): Array[Byte] =
      val w = Writer()
      w.u32(hits.length)
      hits.foreach(MethodHit.write(w, _))
      w.finish(KindMethodHits)

  object MethodHitsResult:
    def decode(buf: Array[Byte]): MethodHitsResult =
      val r = Reader(buf, KindMethodHits)
      val n = r.count()
      val hits = Vector.newBuilder[MethodHit]
      var i = 0
      while i < n do
        hits += MethodHit.read(r)
        i += 1
      r.finish()
      MethodHitsResult(hits.result())

  /** A prepare-rename response: `None` when the symbol is not PC-renameable. */
  final case class PrepareRenameResult(range: Option[Rng]):
    def encode(): Array[Byte] =
      val w = Writer()
      Rng.writeOpt(w, range)
      w.finish(KindPrepareRename)

  object PrepareRenameResult:
    def decode(buf: Array[Byte]): PrepareRenameResult =
      val r = Reader(buf, KindPrepareRename)
      val range = Rng.readOpt(r)
      r.finish()
      PrepareRenameResult(range)

  // -------------------------------------------------------------------------
  // Plugin status.
  // -------------------------------------------------------------------------

  /** One compiler plugin's status (mirrors `PcWorkerCompilerPlugin`). */
  final case class CompilerPlugin(
      jars: Seq[String],
      options: Seq[String],
      loaded: Boolean,
      detail: String
  )

  /** One service plugin's status (mirrors `PcWorkerServicePlugin`). */
  final case class ServicePlugin(
      id: String,
      source: String,
      enabled: Boolean,
      selfTestOk: Boolean,
      selfTestDetail: String
  )

  /** A disabled plugin (mirrors `PcWorkerDisabledPlugin`). */
  final case class DisabledPlugin(id: String, reason: String)

  /** The full plugin-status report (mirrors `PcWorkerPluginStatus`). */
  final case class PluginStatus(
      compilerPlugins: Seq[CompilerPlugin],
      servicePlugins: Seq[ServicePlugin],
      disabled: Seq[DisabledPlugin]
  ):
    def encode(): Array[Byte] =
      val w = Writer()
      w.u32(compilerPlugins.length)
      compilerPlugins.foreach { p =>
        w.strList(p.jars)
        w.strList(p.options)
        w.bool32(p.loaded)
        w.str(p.detail)
      }
      w.u32(servicePlugins.length)
      servicePlugins.foreach { p =>
        w.str(p.id)
        w.str(p.source)
        w.bool32(p.enabled)
        w.bool32(p.selfTestOk)
        w.str(p.selfTestDetail)
      }
      w.u32(disabled.length)
      disabled.foreach { p =>
        w.str(p.id)
        w.str(p.reason)
      }
      w.finish(KindPluginStatus)

  object PluginStatus:
    def decode(buf: Array[Byte]): PluginStatus =
      val r = Reader(buf, KindPluginStatus)
      val compilerCount = r.count()
      val compilerPlugins = Vector.newBuilder[CompilerPlugin]
      var i = 0
      while i < compilerCount do
        val jars = r.strList()
        val options = r.strList()
        val loaded = r.bool32()
        val detail = r.str()
        compilerPlugins += CompilerPlugin(jars, options, loaded, detail)
        i += 1
      val serviceCount = r.count()
      val servicePlugins = Vector.newBuilder[ServicePlugin]
      i = 0
      while i < serviceCount do
        val id = r.str()
        val source = r.str()
        val enabled = r.bool32()
        val selfTestOk = r.bool32()
        val selfTestDetail = r.str()
        servicePlugins += ServicePlugin(id, source, enabled, selfTestOk, selfTestDetail)
        i += 1
      val disabledCount = r.count()
      val disabled = Vector.newBuilder[DisabledPlugin]
      i = 0
      while i < disabledCount do
        val id = r.str()
        val reason = r.str()
        disabled += DisabledPlugin(id, reason)
        i += 1
      r.finish()
      PluginStatus(compilerPlugins.result(), servicePlugins.result(), disabled.result())

  // -------------------------------------------------------------------------
  // Completion / resolve / signature-help carriers (the resolved LSP4J 1.0.0
  // field surface: e.g. CompletionItem.textEditText, Command.tooltip,
  // CompletionList.applyKind). Opaque JSON fields (CompletionItem.data,
  // Command.arguments, itemDefaults.data) are carried verbatim as bytes.
  // -------------------------------------------------------------------------

  private def writeOptStrList(w: Writer, list: Option[Seq[String]]): Unit = list match
    case Some(items) =>
      w.u32(1)
      w.strList(items)
    case None => w.u32(0)

  private def readOptStrList(r: Reader): Option[Seq[String]] =
    if r.u32() == 0 then None else Some(r.strList())

  /** A documentation body (LSP4J `Either<String, MarkupContent>`). */
  enum Documentation:
    case Plain(value: String)
    case Markup(content: MarkupContent)

  object Documentation:
    def writeOpt(w: Writer, doc: Option[Documentation]): Unit = doc match
      case None => w.u32(0)
      case Some(Documentation.Plain(s)) =>
        w.u32(1)
        w.str(s)
      case Some(Documentation.Markup(m)) =>
        w.u32(2)
        MarkupContent.write(w, m)

    def readOpt(r: Reader): Option[Documentation] = r.u32() match
      case 0 => None
      case 1 => Some(Documentation.Plain(r.str()))
      case 2 => Some(Documentation.Markup(MarkupContent.read(r)))
      case tag => throw CodecException(s"invalid documentation variant tag $tag")

  /** A text edit (a range plus its replacement text). */
  final case class TextEdit(range: Rng, newText: String)

  object TextEdit:
    def write(w: Writer, e: TextEdit): Unit =
      Rng.write(w, e.range)
      w.str(e.newText)

    def read(r: Reader): TextEdit =
      val range = Rng.read(r)
      val newText = r.str()
      TextEdit(range, newText)

  /** An insert/replace completion edit (LSP4J `InsertReplaceEdit`). */
  final case class InsertReplaceEdit(newText: String, insert: Rng, replace: Rng)

  /** A completion item's edit (LSP4J `Either<TextEdit, InsertReplaceEdit>`). */
  enum CompletionEdit:
    case Plain(edit: TextEdit)
    case InsertReplace(edit: InsertReplaceEdit)

  object CompletionEdit:
    def write(w: Writer, e: CompletionEdit): Unit = e match
      case CompletionEdit.Plain(edit) =>
        w.u32(0)
        TextEdit.write(w, edit)
      case CompletionEdit.InsertReplace(edit) =>
        w.u32(1)
        w.str(edit.newText)
        Rng.write(w, edit.insert)
        Rng.write(w, edit.replace)

    def read(r: Reader): CompletionEdit = r.u32() match
      case 0 => CompletionEdit.Plain(TextEdit.read(r))
      case 1 =>
        val newText = r.str()
        val insert = Rng.read(r)
        val replace = Rng.read(r)
        CompletionEdit.InsertReplace(InsertReplaceEdit(newText, insert, replace))
      case tag => throw CodecException(s"invalid completion edit variant tag $tag")

  /** Additional label details (LSP4J `CompletionItemLabelDetails`). */
  final case class LabelDetails(detail: Option[String], description: Option[String])

  /** A completion item's `command` (LSP4J `Command`); `arguments` is the opaque
    * serialized JSON argument array, carried verbatim.
    */
  final case class Command(
      title: String,
      tooltip: Option[String],
      command: String,
      arguments: Option[Seq[Byte]]
  )

  /** One completion item — the full resolved LSP4J 1.0.0 `CompletionItem` surface. */
  final case class CompletionItem(
      label: String,
      labelDetails: Option[LabelDetails],
      kind: Option[Int],
      tags: Option[Seq[Int]],
      detail: Option[String],
      documentation: Option[Documentation],
      deprecated: Option[Boolean],
      preselect: Option[Boolean],
      sortText: Option[String],
      filterText: Option[String],
      insertText: Option[String],
      insertTextFormat: Option[Int],
      insertTextMode: Option[Int],
      textEdit: Option[CompletionEdit],
      textEditText: Option[String],
      additionalTextEdits: Option[Seq[TextEdit]],
      commitCharacters: Option[Seq[String]],
      command: Option[Command],
      data: Option[Seq[Byte]]
  ):
    /** `completion_resolve` response (a single enriched item). */
    def encode(): Array[Byte] =
      val w = Writer()
      CompletionItem.write(w, this)
      w.finish(KindCompletionItem)

  object CompletionItem:
    def write(w: Writer, item: CompletionItem): Unit =
      w.str(item.label)
      item.labelDetails match
        case Some(d) =>
          w.u32(1)
          w.optStr(d.detail)
          w.optStr(d.description)
        case None => w.u32(0)
      w.optI32(item.kind)
      item.tags match
        case Some(tags) =>
          w.u32(1)
          w.u32(tags.length)
          tags.foreach(w.i32)
        case None => w.u32(0)
      w.optStr(item.detail)
      Documentation.writeOpt(w, item.documentation)
      w.optBool(item.deprecated)
      w.optBool(item.preselect)
      w.optStr(item.sortText)
      w.optStr(item.filterText)
      w.optStr(item.insertText)
      w.optI32(item.insertTextFormat)
      w.optI32(item.insertTextMode)
      item.textEdit match
        case Some(edit) =>
          w.u32(1)
          CompletionEdit.write(w, edit)
        case None => w.u32(0)
      w.optStr(item.textEditText)
      item.additionalTextEdits match
        case Some(edits) =>
          w.u32(1)
          w.u32(edits.length)
          edits.foreach(TextEdit.write(w, _))
        case None => w.u32(0)
      writeOptStrList(w, item.commitCharacters)
      item.command match
        case Some(c) =>
          w.u32(1)
          w.str(c.title)
          w.optStr(c.tooltip)
          w.str(c.command)
          w.optBytes(c.arguments.map(_.toArray))
        case None => w.u32(0)
      w.optBytes(item.data.map(_.toArray))

    def read(r: Reader): CompletionItem =
      val label = r.str()
      val labelDetails =
        if r.u32() != 0 then
          val detail = r.optStr()
          val description = r.optStr()
          Some(LabelDetails(detail, description))
        else None
      val kind = r.optI32()
      val tags =
        if r.u32() != 0 then
          val n = r.count()
          val b = Vector.newBuilder[Int]
          var i = 0
          while i < n do
            b += r.i32()
            i += 1
          Some(b.result())
        else None
      val detail = r.optStr()
      val documentation = Documentation.readOpt(r)
      val deprecated = r.optBool()
      val preselect = r.optBool()
      val sortText = r.optStr()
      val filterText = r.optStr()
      val insertText = r.optStr()
      val insertTextFormat = r.optI32()
      val insertTextMode = r.optI32()
      val textEdit = if r.u32() != 0 then Some(CompletionEdit.read(r)) else None
      val textEditText = r.optStr()
      val additionalTextEdits =
        if r.u32() != 0 then
          val n = r.count()
          val b = Vector.newBuilder[TextEdit]
          var i = 0
          while i < n do
            b += TextEdit.read(r)
            i += 1
          Some(b.result())
        else None
      val commitCharacters = readOptStrList(r)
      val command =
        if r.u32() != 0 then
          val title = r.str()
          val tooltip = r.optStr()
          val cmd = r.str()
          val arguments = r.optBytes().map(_.toSeq)
          Some(Command(title, tooltip, cmd, arguments))
        else None
      val data = r.optBytes().map(_.toSeq)
      CompletionItem(
        label,
        labelDetails,
        kind,
        tags,
        detail,
        documentation,
        deprecated,
        preselect,
        sortText,
        filterText,
        insertText,
        insertTextFormat,
        insertTextMode,
        textEdit,
        textEditText,
        additionalTextEdits,
        commitCharacters,
        command,
        data
      )

    def decode(buf: Array[Byte]): CompletionItem =
      val r = Reader(buf, KindCompletionItem)
      val item = read(r)
      r.finish()
      item

  /** A range or an insert/replace range pair (LSP4J completion-list
    * `itemDefaults.editRange`).
    */
  enum EditRange:
    case Range(range: Rng)
    case InsertReplace(insert: Rng, replace: Rng)

  object EditRange:
    def writeOpt(w: Writer, editRange: Option[EditRange]): Unit = editRange match
      case None => w.u32(0)
      case Some(EditRange.Range(range)) =>
        w.u32(1)
        Rng.write(w, range)
      case Some(EditRange.InsertReplace(insert, replace)) =>
        w.u32(2)
        Rng.write(w, insert)
        Rng.write(w, replace)

    def readOpt(r: Reader): Option[EditRange] = r.u32() match
      case 0 => None
      case 1 => Some(EditRange.Range(Rng.read(r)))
      case 2 =>
        val insert = Rng.read(r)
        val replace = Rng.read(r)
        Some(EditRange.InsertReplace(insert, replace))
      case tag => throw CodecException(s"invalid edit range variant tag $tag")

  /** Completion-list defaults (LSP4J `CompletionItemDefaults`). */
  final case class CompletionItemDefaults(
      commitCharacters: Option[Seq[String]],
      editRange: Option[EditRange],
      insertTextFormat: Option[Int],
      insertTextMode: Option[Int],
      data: Option[Seq[Byte]]
  )

  object CompletionItemDefaults:
    def write(w: Writer, d: CompletionItemDefaults): Unit =
      writeOptStrList(w, d.commitCharacters)
      EditRange.writeOpt(w, d.editRange)
      w.optI32(d.insertTextFormat)
      w.optI32(d.insertTextMode)
      w.optBytes(d.data.map(_.toArray))

    def read(r: Reader): CompletionItemDefaults =
      val commitCharacters = readOptStrList(r)
      val editRange = EditRange.readOpt(r)
      val insertTextFormat = r.optI32()
      val insertTextMode = r.optI32()
      val data = r.optBytes().map(_.toSeq)
      CompletionItemDefaults(commitCharacters, editRange, insertTextFormat, insertTextMode, data)

  /** How a `CompletionList` applies its `itemDefaults` (LSP4J `CompletionApplyKind`). */
  final case class CompletionApplyKind(commitCharacters: Option[Int], data: Option[Int])

  object CompletionApplyKind:
    def writeOpt(w: Writer, applyKind: Option[CompletionApplyKind]): Unit = applyKind match
      case Some(k) =>
        w.u32(1)
        w.optI32(k.commitCharacters)
        w.optI32(k.data)
      case None => w.u32(0)

    def readOpt(r: Reader): Option[CompletionApplyKind] =
      if r.u32() == 0 then None
      else
        val commitCharacters = r.optI32()
        val data = r.optI32()
        Some(CompletionApplyKind(commitCharacters, data))

  /** A completion response list (LSP4J `CompletionList`). An empty `items` is a
    * real empty list (distinct from a null hover / prepare-rename).
    */
  final case class CompletionList(
      isIncomplete: Boolean,
      itemDefaults: Option[CompletionItemDefaults],
      applyKind: Option[CompletionApplyKind],
      items: Seq[CompletionItem]
  ):
    def encode(): Array[Byte] =
      val w = Writer()
      w.bool32(isIncomplete)
      itemDefaults match
        case Some(d) =>
          w.u32(1)
          CompletionItemDefaults.write(w, d)
        case None => w.u32(0)
      CompletionApplyKind.writeOpt(w, applyKind)
      w.u32(items.length)
      items.foreach(CompletionItem.write(w, _))
      w.finish(KindCompletionList)

  object CompletionList:
    def decode(buf: Array[Byte]): CompletionList =
      val r = Reader(buf, KindCompletionList)
      val isIncomplete = r.bool32()
      val itemDefaults = if r.u32() != 0 then Some(CompletionItemDefaults.read(r)) else None
      val applyKind = CompletionApplyKind.readOpt(r)
      val n = r.count()
      val items = Vector.newBuilder[CompletionItem]
      var i = 0
      while i < n do
        items += CompletionItem.read(r)
        i += 1
      r.finish()
      CompletionList(isIncomplete, itemDefaults, applyKind, items.result())

  /** `completion_resolve` params (mirrors `PcWorkerResolveParams`). */
  final case class ResolveParams(targetId: String, symbol: String, item: CompletionItem):
    def encode(): Array[Byte] =
      val w = Writer()
      w.str(targetId)
      w.str(symbol)
      CompletionItem.write(w, item)
      w.finish(KindResolveParams)

  object ResolveParams:
    def decode(buf: Array[Byte]): ResolveParams =
      val r = Reader(buf, KindResolveParams)
      val targetId = r.str()
      val symbol = r.str()
      val item = CompletionItem.read(r)
      r.finish()
      ResolveParams(targetId, symbol, item)

  /** A parameter label (LSP4J `Either<String, Tuple.Two<Integer, Integer>>`). */
  enum ParameterLabel:
    case Str(value: String)
    case Offsets(start: Int, end: Int)

  object ParameterLabel:
    def write(w: Writer, l: ParameterLabel): Unit = l match
      case ParameterLabel.Str(s) =>
        w.u32(0)
        w.str(s)
      case ParameterLabel.Offsets(start, end) =>
        w.u32(1)
        w.u32(start)
        w.u32(end)

    def read(r: Reader): ParameterLabel = r.u32() match
      case 0 => ParameterLabel.Str(r.str())
      case 1 =>
        val start = r.u32()
        val end = r.u32()
        ParameterLabel.Offsets(start, end)
      case tag => throw CodecException(s"invalid parameter label variant tag $tag")

  /** One parameter of a signature (LSP4J `ParameterInformation`). */
  final case class ParameterInfo(label: ParameterLabel, documentation: Option[Documentation])

  /** One signature (LSP4J `SignatureInformation`). */
  final case class SignatureInfo(
      label: String,
      documentation: Option[Documentation],
      parameters: Option[Seq[ParameterInfo]],
      activeParameter: Option[Int]
  )

  /** A signature-help response (LSP4J `SignatureHelp`). */
  final case class SignatureHelp(
      signatures: Seq[SignatureInfo],
      activeSignature: Option[Int],
      activeParameter: Option[Int]
  ):
    def encode(): Array[Byte] =
      val w = Writer()
      w.u32(signatures.length)
      signatures.foreach { sig =>
        w.str(sig.label)
        Documentation.writeOpt(w, sig.documentation)
        sig.parameters match
          case Some(params) =>
            w.u32(1)
            w.u32(params.length)
            params.foreach { p =>
              ParameterLabel.write(w, p.label)
              Documentation.writeOpt(w, p.documentation)
            }
          case None => w.u32(0)
        w.optI32(sig.activeParameter)
      }
      w.optI32(activeSignature)
      w.optI32(activeParameter)
      w.finish(KindSignatureHelp)

  object SignatureHelp:
    def decode(buf: Array[Byte]): SignatureHelp =
      val r = Reader(buf, KindSignatureHelp)
      val sigCount = r.count()
      val signatures = Vector.newBuilder[SignatureInfo]
      var i = 0
      while i < sigCount do
        val label = r.str()
        val documentation = Documentation.readOpt(r)
        val parameters =
          if r.u32() != 0 then
            val paramCount = r.count()
            val params = Vector.newBuilder[ParameterInfo]
            var j = 0
            while j < paramCount do
              val paramLabel = ParameterLabel.read(r)
              val paramDoc = Documentation.readOpt(r)
              params += ParameterInfo(paramLabel, paramDoc)
              j += 1
            Some(params.result())
          else None
        val activeParameter = r.optI32()
        signatures += SignatureInfo(label, documentation, parameters, activeParameter)
        i += 1
      val activeSignature = r.optI32()
      val activeParameter = r.optI32()
      r.finish()
      SignatureHelp(signatures.result(), activeSignature, activeParameter)

  // -------------------------------------------------------------------------
  // Payload-query ops (ABI v2): inlay hints, semantic tokens, selection
  // ranges, code actions, auto-imports, PC diagnostics, folding ranges — plus
  // the `definition_source_toplevels` callback response. Byte-for-byte mirrors
  // of the Rust `payloads` additions.
  // -------------------------------------------------------------------------

  private def writeTextEditList(w: Writer, edits: Seq[TextEdit]): Unit =
    w.u32(edits.length)
    edits.foreach(TextEdit.write(w, _))

  private def readTextEditList(r: Reader): Seq[TextEdit] =
    val n = r.count()
    val out = Vector.newBuilder[TextEdit]
    var i = 0
    while i < n do
      out += TextEdit.read(r)
      i += 1
    out.result()

  /** `inlay_hints` params: the buffer, the requested range, and a flags bitset
    * (the provider's hint-category toggles; opaque to the transport).
    */
  final case class InlayHintParams(uri: String, range: Rng, flags: Int):
    def encode(): Array[Byte] =
      val w = Writer()
      w.str(uri)
      Rng.write(w, range)
      w.u32(flags)
      w.finish(KindInlayHintParams)

  object InlayHintParams:
    def decode(buf: Array[Byte]): InlayHintParams =
      val r = Reader(buf, KindInlayHintParams)
      val uri = r.str()
      val range = Rng.read(r)
      val flags = r.u32()
      r.finish()
      InlayHintParams(uri, range, flags)

  /** One part of an inlay hint's label: the text, an optional target location
    * (`(uri, range)` — no origin), and an optional tooltip string.
    */
  final case class InlayLabelPart(
      text: String,
      location: Option[(String, Rng)],
      tooltip: Option[String]
  )

  object InlayLabelPart:
    def write(w: Writer, p: InlayLabelPart): Unit =
      w.str(p.text)
      p.location match
        case Some((uri, range)) =>
          w.u32(1)
          w.str(uri)
          Rng.write(w, range)
        case None => w.u32(0)
      w.optStr(p.tooltip)

    def read(r: Reader): InlayLabelPart =
      val text = r.str()
      val location =
        if r.u32() != 0 then
          val uri = r.str()
          val range = Rng.read(r)
          Some((uri, range))
        else None
      val tooltip = r.optStr()
      InlayLabelPart(text, location, tooltip)

  /** One inlay hint: position, label parts, the LSP `InlayHintKind` int, the
    * padding flags, optional text edits, and opaque `data` bytes (the
    * `CompletionItem.data` idiom — carried verbatim, never interpreted).
    */
  final case class InlayHint(
      position: Pos,
      labelParts: Seq[InlayLabelPart],
      kind: Int,
      paddingLeft: Boolean,
      paddingRight: Boolean,
      textEdits: Option[Seq[TextEdit]],
      data: Option[Seq[Byte]]
  )

  object InlayHint:
    def write(w: Writer, h: InlayHint): Unit =
      Pos.write(w, h.position)
      w.u32(h.labelParts.length)
      h.labelParts.foreach(InlayLabelPart.write(w, _))
      w.i32(h.kind)
      w.bool32(h.paddingLeft)
      w.bool32(h.paddingRight)
      h.textEdits match
        case Some(edits) =>
          w.u32(1)
          writeTextEditList(w, edits)
        case None => w.u32(0)
      w.optBytes(h.data.map(_.toArray))

    def read(r: Reader): InlayHint =
      val position = Pos.read(r)
      val n = r.count()
      val parts = Vector.newBuilder[InlayLabelPart]
      var i = 0
      while i < n do
        parts += InlayLabelPart.read(r)
        i += 1
      val kind = r.i32()
      val paddingLeft = r.bool32()
      val paddingRight = r.bool32()
      val textEdits = if r.u32() != 0 then Some(readTextEditList(r)) else None
      val data = r.optBytes().map(_.toSeq)
      InlayHint(position, parts.result(), kind, paddingLeft, paddingRight, textEdits, data)

  /** The `inlay_hints` response: the hints for the requested range. */
  final case class InlayHintsResult(hints: Seq[InlayHint]):
    def encode(): Array[Byte] =
      val w = Writer()
      w.u32(hints.length)
      hints.foreach(InlayHint.write(w, _))
      w.finish(KindInlayHints)

  object InlayHintsResult:
    def decode(buf: Array[Byte]): InlayHintsResult =
      val r = Reader(buf, KindInlayHints)
      val n = r.count()
      val hints = Vector.newBuilder[InlayHint]
      var i = 0
      while i < n do
        hints += InlayHint.read(r)
        i += 1
      r.finish()
      InlayHintsResult(hints.result())

  /** A single-uri params payload (`semantic_tokens`/`pc_diagnostics`/
    * `folding_range`).
    */
  final case class UriParams(uri: String):
    def encode(): Array[Byte] =
      val w = Writer()
      w.str(uri)
      w.finish(KindUriParams)

  object UriParams:
    def decode(buf: Array[Byte]): UriParams =
      val r = Reader(buf, KindUriParams)
      val uri = r.str()
      r.finish()
      UriParams(uri)

  /** One semantic-tokens node: `[start, end)` UTF-16 offsets into the buffer
    * text (offsets, not line/character — the Rust host converts), plus the
    * token type and modifier bitset ints.
    */
  final case class SemanticNode(start: Int, end: Int, tokenType: Int, tokenModifier: Int)

  /** The `semantic_tokens` response: the offset-based token nodes in buffer
    * order.
    */
  final case class SemanticTokensResult(nodes: Seq[SemanticNode]):
    def encode(): Array[Byte] =
      val w = Writer()
      w.u32(nodes.length)
      nodes.foreach { n =>
        w.u32(n.start)
        w.u32(n.end)
        w.i32(n.tokenType)
        w.i32(n.tokenModifier)
      }
      w.finish(KindSemanticTokens)

  object SemanticTokensResult:
    def decode(buf: Array[Byte]): SemanticTokensResult =
      val r = Reader(buf, KindSemanticTokens)
      val n = r.count()
      val nodes = Vector.newBuilder[SemanticNode]
      var i = 0
      while i < n do
        val start = r.u32()
        val end = r.u32()
        val tokenType = r.i32()
        val tokenModifier = r.i32()
        nodes += SemanticNode(start, end, tokenType, tokenModifier)
        i += 1
      r.finish()
      SemanticTokensResult(nodes.result())

  /** `selection_range` params: the buffer and the query positions. */
  final case class SelectionRangeParams(uri: String, positions: Seq[Pos]):
    def encode(): Array[Byte] =
      val w = Writer()
      w.str(uri)
      w.u32(positions.length)
      positions.foreach(Pos.write(w, _))
      w.finish(KindSelectionRangeParams)

  object SelectionRangeParams:
    def decode(buf: Array[Byte]): SelectionRangeParams =
      val r = Reader(buf, KindSelectionRangeParams)
      val uri = r.str()
      val n = r.count()
      val positions = Vector.newBuilder[Pos]
      var i = 0
      while i < n do
        positions += Pos.read(r)
        i += 1
      r.finish()
      SelectionRangeParams(uri, positions.result())

  /** The `selection_range` response: per query position, the chain of
    * enclosing ranges, innermost first.
    */
  final case class SelectionRangesResult(chains: Seq[Seq[Rng]]):
    def encode(): Array[Byte] =
      val w = Writer()
      w.u32(chains.length)
      chains.foreach { chain =>
        w.u32(chain.length)
        chain.foreach(Rng.write(w, _))
      }
      w.finish(KindSelectionRanges)

  object SelectionRangesResult:
    def decode(buf: Array[Byte]): SelectionRangesResult =
      val r = Reader(buf, KindSelectionRanges)
      val chainCount = r.count()
      val chains = Vector.newBuilder[Seq[Rng]]
      var i = 0
      while i < chainCount do
        val n = r.count()
        val chain = Vector.newBuilder[Rng]
        var j = 0
        while j < n do
          chain += Rng.read(r)
          j += 1
        chains += chain.result()
        i += 1
      r.finish()
      SelectionRangesResult(chains.result())

  /** `code_action` params: the buffer, the [[CodeActionId]] action, the cursor
    * position, an optional extraction end (extract-method's selection end),
    * and optional argument indices (convert-to-named-arguments).
    */
  final case class CodeActionParams(
      uri: String,
      action: Int,
      position: Pos,
      extractionEnd: Option[Pos],
      argIndices: Option[Seq[Int]]
  ):
    def encode(): Array[Byte] =
      val w = Writer()
      w.str(uri)
      w.i32(action)
      Pos.write(w, position)
      Pos.writeOpt(w, extractionEnd)
      argIndices match
        case Some(indices) =>
          w.u32(1)
          w.u32(indices.length)
          indices.foreach(w.i32)
        case None => w.u32(0)
      w.finish(KindCodeActionParams)

  object CodeActionParams:
    def decode(buf: Array[Byte]): CodeActionParams =
      val r = Reader(buf, KindCodeActionParams)
      val uri = r.str()
      val action = r.i32()
      val position = Pos.read(r)
      val extractionEnd = Pos.readOpt(r)
      val argIndices =
        if r.u32() != 0 then
          val n = r.count()
          val indices = Vector.newBuilder[Int]
          var i = 0
          while i < n do
            indices += r.i32()
            i += 1
          Some(indices.result())
        else None
      r.finish()
      CodeActionParams(uri, action, position, extractionEnd, argIndices)

  /** The `code_action` response: the edits plus an optional typed refusal
    * message (the `DisplayableException` carrier — a refusal is DATA on a
    * `STATUS_OK` reply, not an error status).
    */
  final case class CodeActionResult(edits: Seq[TextEdit], refusal: Option[String]):
    def encode(): Array[Byte] =
      val w = Writer()
      writeTextEditList(w, edits)
      w.optStr(refusal)
      w.finish(KindCodeActionEdits)

  object CodeActionResult:
    def decode(buf: Array[Byte]): CodeActionResult =
      val r = Reader(buf, KindCodeActionEdits)
      val edits = readTextEditList(r)
      val refusal = r.optStr()
      r.finish()
      CodeActionResult(edits, refusal)

  /** `auto_imports` params: the buffer, the cursor position, the name to
    * import, and whether an extension-method import is requested.
    */
  final case class AutoImportParams(uri: String, position: Pos, name: String, isExtension: Boolean):
    def encode(): Array[Byte] =
      val w = Writer()
      w.str(uri)
      Pos.write(w, position)
      w.str(name)
      w.bool32(isExtension)
      w.finish(KindAutoImportParams)

  object AutoImportParams:
    def decode(buf: Array[Byte]): AutoImportParams =
      val r = Reader(buf, KindAutoImportParams)
      val uri = r.str()
      val position = Pos.read(r)
      val name = r.str()
      val isExtension = r.bool32()
      r.finish()
      AutoImportParams(uri, position, name, isExtension)

  /** One auto-import candidate: the providing package, the edits that apply
    * it, and optionally the imported SemanticDB symbol.
    */
  final case class AutoImport(packageName: String, edits: Seq[TextEdit], symbol: Option[String])

  /** The `auto_imports` response: the candidates, best first. */
  final case class AutoImportsResult(imports: Seq[AutoImport]):
    def encode(): Array[Byte] =
      val w = Writer()
      w.u32(imports.length)
      imports.foreach { imp =>
        w.str(imp.packageName)
        writeTextEditList(w, imp.edits)
        w.optStr(imp.symbol)
      }
      w.finish(KindAutoImports)

  object AutoImportsResult:
    def decode(buf: Array[Byte]): AutoImportsResult =
      val r = Reader(buf, KindAutoImports)
      val n = r.count()
      val imports = Vector.newBuilder[AutoImport]
      var i = 0
      while i < n do
        val packageName = r.str()
        val edits = readTextEditList(r)
        val symbol = r.optStr()
        imports += AutoImport(packageName, edits, symbol)
        i += 1
      r.finish()
      AutoImportsResult(imports.result())

  /** One presentation-compiler diagnostic (the reduced record the boundary
    * carries: span, LSP severity int, code string, message).
    */
  final case class PcDiagnostic(range: Rng, severity: Int, code: String, message: String)

  /** The `pc_diagnostics` response: the buffer's PC diagnostics in report
    * order.
    */
  final case class PcDiagnosticsResult(diagnostics: Seq[PcDiagnostic]):
    def encode(): Array[Byte] =
      val w = Writer()
      w.u32(diagnostics.length)
      diagnostics.foreach { d =>
        Rng.write(w, d.range)
        w.i32(d.severity)
        w.str(d.code)
        w.str(d.message)
      }
      w.finish(KindPcDiagnostics)

  object PcDiagnosticsResult:
    def decode(buf: Array[Byte]): PcDiagnosticsResult =
      val r = Reader(buf, KindPcDiagnostics)
      val n = r.count()
      val diagnostics = Vector.newBuilder[PcDiagnostic]
      var i = 0
      while i < n do
        val range = Rng.read(r)
        val severity = r.i32()
        val code = r.str()
        val message = r.str()
        diagnostics += PcDiagnostic(range, severity, code, message)
        i += 1
      r.finish()
      PcDiagnosticsResult(diagnostics.result())

  /** One folding range: the span plus its [[FoldingKind]] ordinal. */
  final case class FoldingRange(range: Rng, kind: Int)

  /** The `folding_range` response: the buffer's folding ranges. */
  final case class FoldingRangesResult(ranges: Seq[FoldingRange]):
    def encode(): Array[Byte] =
      val w = Writer()
      w.u32(ranges.length)
      ranges.foreach { f =>
        Rng.write(w, f.range)
        w.i32(f.kind)
      }
      w.finish(KindFoldingRanges)

  object FoldingRangesResult:
    def decode(buf: Array[Byte]): FoldingRangesResult =
      val r = Reader(buf, KindFoldingRanges)
      val n = r.count()
      val ranges = Vector.newBuilder[FoldingRange]
      var i = 0
      while i < n do
        val range = Rng.read(r)
        val kind = r.i32()
        ranges += FoldingRange(range, kind)
        i += 1
      r.finish()
      FoldingRangesResult(ranges.result())

  /** The `definition_source_toplevels` callback response: the toplevel
    * SemanticDB symbols of the resolved definition source (mirrors
    * [[LocationsResult]]'s shape for the definition callback).
    */
  final case class ToplevelsResult(symbols: Seq[String]):
    def encode(): Array[Byte] =
      val w = Writer()
      w.strList(symbols)
      w.finish(KindToplevels)

  object ToplevelsResult:
    def decode(buf: Array[Byte]): ToplevelsResult =
      val r = Reader(buf, KindToplevels)
      val symbols = r.strList()
      r.finish()
      ToplevelsResult(symbols)
