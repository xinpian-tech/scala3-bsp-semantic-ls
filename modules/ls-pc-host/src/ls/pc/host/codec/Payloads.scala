package ls.pc.host.codec

import ls.pc.host.codec.Codec.{Reader, Writer}

/** Island-side owned mirrors of the carrier-free op payloads, with lossless
  * flat encode/decode over the [[Codec]] buffer format — the byte-for-byte
  * counterpart of the Rust `ls-pc-abi` `payloads` module for the requests the
  * island consumes (`register_target`/`did_open`/`did_change`/position) and the
  * carrier-free responses it produces (`hover`/`definition`/`type_definition`/
  * `prepare_rename`/`plugin_status` and the `symbol_definition` callback).
  *
  * The completion/resolve/signature-help carriers (which model the full
  * resolved-LSP4J-1.0.0 field surface) land with the facade wiring in a later
  * slice; their payload kinds are reserved here for reference.
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

  /** `DefinitionOrigin` ordinals (mirror the Scala `enum DefinitionOrigin`). */
  object Origin:
    val Workspace: Int = 0
    val Synthetic: Int = 1
    val Plugin: Int = 2

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
