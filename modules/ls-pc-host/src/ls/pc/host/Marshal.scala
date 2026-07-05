package ls.pc.host

import java.nio.charset.StandardCharsets.UTF_8
import java.nio.file.Path

import scala.jdk.CollectionConverters.*

import com.google.gson.{Gson, JsonParser}
import org.eclipse.lsp4j as l
import org.eclipse.lsp4j.jsonrpc.messages.{Either as JEither, Tuple}

import ls.pc.{DefinitionOrigin, DefinitionResult as SpiDefinitionResult, PcPluginStatusReport, PcTargetConfig}
import ls.pc.host.codec.Payloads

/** Translates between the live presentation-compiler facade's carrier values —
  * the resolved LSP4J 1.0.0 objects and the PC `spi` result types — and the
  * flat [[Payloads]] wire form the boundary encodes. Responses convert LSP4J →
  * flat; the two inputs the island reconstructs (`register_target` config and a
  * `completion_resolve` item) convert flat → LSP4J.
  *
  * Opaque JSON fields (`CompletionItem.data`, `Command.arguments`,
  * `itemDefaults.data`) have no structural mirror, so they are carried verbatim
  * as their canonical JSON bytes via the same GSON codec LSP4J itself uses.
  */
object Marshal:
  private val gson = new Gson()

  // ---- Responses: LSP4J / spi → flat -------------------------------------

  def completionList(cl: l.CompletionList): Payloads.CompletionList =
    Payloads.CompletionList(
      isIncomplete = cl.isIncomplete,
      itemDefaults = Option(cl.getItemDefaults).map(itemDefaults),
      applyKind = Option(cl.getApplyKind).map(applyKind),
      items = Option(cl.getItems).map(_.asScala.toVector.map(completionItem)).getOrElse(Vector.empty)
    )

  def completionItem(i: l.CompletionItem): Payloads.CompletionItem =
    Payloads.CompletionItem(
      label = i.getLabel,
      labelDetails =
        Option(i.getLabelDetails).map(d => Payloads.LabelDetails(Option(d.getDetail), Option(d.getDescription))),
      kind = Option(i.getKind).map(_.getValue),
      tags = Option(i.getTags).map(_.asScala.toVector.map(_.getValue)),
      detail = Option(i.getDetail),
      documentation = documentation(i.getDocumentation),
      deprecated = optBool(i.getDeprecated),
      preselect = optBool(i.getPreselect),
      sortText = Option(i.getSortText),
      filterText = Option(i.getFilterText),
      insertText = Option(i.getInsertText),
      insertTextFormat = Option(i.getInsertTextFormat).map(_.getValue),
      insertTextMode = Option(i.getInsertTextMode).map(_.getValue),
      textEdit = completionEdit(i.getTextEdit),
      textEditText = Option(i.getTextEditText),
      additionalTextEdits = Option(i.getAdditionalTextEdits).map(_.asScala.toVector.map(textEdit)),
      commitCharacters = Option(i.getCommitCharacters).map(_.asScala.toVector),
      command = Option(i.getCommand).map(command),
      data = jsonBytes(i.getData)
    )

  def hover(h: Option[l.Hover]): Payloads.HoverResult =
    Payloads.HoverResult(h.map { hv =>
      Payloads.Hover(hoverContents(hv.getContents), Option(hv.getRange).map(rng))
    })

  def signatureHelp(s: l.SignatureHelp): Payloads.SignatureHelp =
    Payloads.SignatureHelp(
      signatures = seq(s.getSignatures).map { sig =>
        Payloads.SignatureInfo(
          label = sig.getLabel,
          documentation = documentation(sig.getDocumentation),
          parameters = Option(sig.getParameters).map(_.asScala.toVector.map { p =>
            Payloads.ParameterInfo(parameterLabel(p.getLabel), documentation(p.getDocumentation))
          }),
          activeParameter = optInt(sig.getActiveParameter)
        )
      },
      activeSignature = optInt(s.getActiveSignature),
      activeParameter = optInt(s.getActiveParameter)
    )

  def definition(d: SpiDefinitionResult): Payloads.DefinitionResult =
    Payloads.DefinitionResult(
      d.symbol,
      d.locations.map(dl => Payloads.Location(dl.location.getUri, rng(dl.location.getRange), origin(dl.origin)))
    )

  def prepareRename(range: Option[l.Range]): Payloads.PrepareRenameResult =
    Payloads.PrepareRenameResult(range.map(rng))

  def pluginStatus(report: PcPluginStatusReport): Payloads.PluginStatus =
    Payloads.PluginStatus(
      compilerPlugins =
        report.compilerPlugins.map(c => Payloads.CompilerPlugin(c.jars, c.options, c.loaded, c.detail)),
      servicePlugins = report.servicePlugins.map(p =>
        Payloads.ServicePlugin(p.id, p.source, p.enabled, p.selfTestOk, p.selfTestDetail)
      ),
      disabled = report.disabled.map(d => Payloads.DisabledPlugin(d.id, d.reason))
    )

  // ---- Requests: flat → LSP4J / spi --------------------------------------

  def targetConfig(c: Payloads.TargetConfig): PcTargetConfig =
    PcTargetConfig(
      bspId = c.bspId,
      classpath = c.classpath.iterator.map(Path.of(_)).toVector,
      scalacOptions = c.scalacOptions.toVector,
      sourceDirs = c.sourceDirs.iterator.map(Path.of(_)).toVector,
      scalaVersion = c.scalaVersion
    )

  def toLsp4jItem(i: Payloads.CompletionItem): l.CompletionItem =
    val out = l.CompletionItem()
    out.setLabel(i.label)
    i.labelDetails.foreach { d =>
      val ld = l.CompletionItemLabelDetails()
      d.detail.foreach(ld.setDetail)
      d.description.foreach(ld.setDescription)
      out.setLabelDetails(ld)
    }
    i.kind.foreach(k => out.setKind(l.CompletionItemKind.forValue(k)))
    i.tags.foreach(ts => out.setTags(ts.map(l.CompletionItemTag.forValue).asJava))
    i.detail.foreach(out.setDetail)
    out.setDocumentation(toDocumentation(i.documentation))
    i.deprecated.foreach(b => out.setDeprecated(java.lang.Boolean.valueOf(b)))
    i.preselect.foreach(b => out.setPreselect(java.lang.Boolean.valueOf(b)))
    i.sortText.foreach(out.setSortText)
    i.filterText.foreach(out.setFilterText)
    i.insertText.foreach(out.setInsertText)
    i.insertTextFormat.foreach(f => out.setInsertTextFormat(l.InsertTextFormat.forValue(f)))
    i.insertTextMode.foreach(m => out.setInsertTextMode(l.InsertTextMode.forValue(m)))
    i.textEdit.foreach {
      case Payloads.CompletionEdit.Plain(e) =>
        out.setTextEdit(JEither.forLeft(toTextEdit(e)))
      case Payloads.CompletionEdit.InsertReplace(e) =>
        val ire = l.InsertReplaceEdit()
        ire.setNewText(e.newText)
        ire.setInsert(toRange(e.insert))
        ire.setReplace(toRange(e.replace))
        out.setTextEdit(JEither.forRight(ire))
    }
    i.textEditText.foreach(out.setTextEditText)
    i.additionalTextEdits.foreach(es => out.setAdditionalTextEdits(es.map(toTextEdit).asJava))
    i.commitCharacters.foreach(cs => out.setCommitCharacters(cs.asJava))
    i.command.foreach(c => out.setCommand(toCommand(c)))
    i.data.foreach(bytes => out.setData(toJson(bytes)))
    out

  // ---- Shared value conversions ------------------------------------------

  private def rng(r: l.Range): Payloads.Rng =
    val s = r.getStart
    val e = r.getEnd
    Payloads.Rng(s.getLine, s.getCharacter, e.getLine, e.getCharacter)

  private def toRange(r: Payloads.Rng): l.Range =
    l.Range(l.Position(r.startLine, r.startCharacter), l.Position(r.endLine, r.endCharacter))

  private def markup(m: l.MarkupContent): Payloads.MarkupContent =
    Payloads.MarkupContent(m.getKind, m.getValue)

  private def documentation(e: JEither[String, l.MarkupContent]): Option[Payloads.Documentation] =
    if e == null then None
    else if e.isLeft then Some(Payloads.Documentation.Plain(e.getLeft))
    else Some(Payloads.Documentation.Markup(markup(e.getRight)))

  private def toDocumentation(d: Option[Payloads.Documentation]): JEither[String, l.MarkupContent] = d match
    case None => null
    case Some(Payloads.Documentation.Plain(s)) => JEither.forLeft(s)
    case Some(Payloads.Documentation.Markup(m)) => JEither.forRight(l.MarkupContent(m.kind, m.value))

  private def textEdit(e: l.TextEdit): Payloads.TextEdit =
    Payloads.TextEdit(rng(e.getRange), e.getNewText)

  private def toTextEdit(e: Payloads.TextEdit): l.TextEdit =
    l.TextEdit(toRange(e.range), e.newText)

  private def completionEdit(e: JEither[l.TextEdit, l.InsertReplaceEdit]): Option[Payloads.CompletionEdit] =
    if e == null then None
    else if e.isLeft then Some(Payloads.CompletionEdit.Plain(textEdit(e.getLeft)))
    else
      val ir = e.getRight
      Some(Payloads.CompletionEdit.InsertReplace(Payloads.InsertReplaceEdit(ir.getNewText, rng(ir.getInsert), rng(ir.getReplace))))

  private def command(c: l.Command): Payloads.Command =
    Payloads.Command(c.getTitle, Option(c.getTooltip), c.getCommand, jsonArrayBytes(c.getArguments))

  private def toCommand(c: Payloads.Command): l.Command =
    val out = l.Command()
    out.setTitle(c.title)
    c.tooltip.foreach(out.setTooltip)
    out.setCommand(c.command)
    c.arguments.foreach(bytes => out.setArguments(toJsonArray(bytes)))
    out

  private def itemDefaults(d: l.CompletionItemDefaults): Payloads.CompletionItemDefaults =
    Payloads.CompletionItemDefaults(
      commitCharacters = Option(d.getCommitCharacters).map(_.asScala.toVector),
      editRange = editRange(d.getEditRange),
      insertTextFormat = Option(d.getInsertTextFormat).map(_.getValue),
      insertTextMode = Option(d.getInsertTextMode).map(_.getValue),
      data = jsonBytes(d.getData)
    )

  private def editRange(e: JEither[l.Range, l.InsertReplaceRange]): Option[Payloads.EditRange] =
    if e == null then None
    else if e.isLeft then Some(Payloads.EditRange.Range(rng(e.getLeft)))
    else
      val ir = e.getRight
      Some(Payloads.EditRange.InsertReplace(rng(ir.getInsert), rng(ir.getReplace)))

  private def applyKind(k: l.CompletionApplyKind): Payloads.CompletionApplyKind =
    Payloads.CompletionApplyKind(
      Option(k.getCommitCharacters).map(_.getValue),
      Option(k.getData).map(_.getValue)
    )

  private def hoverContents(
      c: JEither[java.util.List[JEither[String, l.MarkedString]], l.MarkupContent]
  ): Payloads.HoverContents =
    if c.isRight then Payloads.HoverContents.Markup(markup(c.getRight))
    else
      val items = c.getLeft.asScala.toVector.map { e =>
        if e.isLeft then Payloads.MarkedStringItem.Plain(e.getLeft)
        else Payloads.MarkedStringItem.Marked(e.getRight.getLanguage, e.getRight.getValue)
      }
      Payloads.HoverContents.Marked(items)

  private def parameterLabel(e: JEither[String, Tuple.Two[Integer, Integer]]): Payloads.ParameterLabel =
    if e.isLeft then Payloads.ParameterLabel.Str(e.getLeft)
    else
      val t = e.getRight
      Payloads.ParameterLabel.Offsets(t.getFirst, t.getSecond)

  private def origin(o: DefinitionOrigin): Int = o match
    case DefinitionOrigin.Workspace => Payloads.Origin.Workspace
    case DefinitionOrigin.Synthetic => Payloads.Origin.Synthetic
    case DefinitionOrigin.Plugin => Payloads.Origin.Plugin

  // ---- Small boxing / GSON helpers ---------------------------------------

  private def seq[A](list: java.util.List[A]): Vector[A] =
    if list == null then Vector.empty else list.asScala.toVector

  private def optInt(i: Integer): Option[Int] = if i == null then None else Some(i.intValue)

  private def optBool(b: java.lang.Boolean): Option[Boolean] = if b == null then None else Some(b.booleanValue)

  /** Serializes an opaque LSP4J value (`Object` / `JsonElement`) to its
    * canonical JSON bytes, or `None` when absent.
    */
  private def jsonBytes(value: Object): Option[Seq[Byte]] =
    if value == null then None else Some(gson.toJson(value).getBytes(UTF_8).toIndexedSeq)

  private def jsonArrayBytes(args: java.util.List[Object]): Option[Seq[Byte]] =
    if args == null then None else Some(gson.toJson(args).getBytes(UTF_8).toIndexedSeq)

  /** Parses opaque JSON bytes back into an LSP4J value (a `JsonElement`). */
  private def toJson(bytes: Seq[Byte]): Object =
    JsonParser.parseString(String(bytes.toArray, UTF_8))

  private def toJsonArray(bytes: Seq[Byte]): java.util.List[Object] =
    val array = JsonParser.parseString(String(bytes.toArray, UTF_8)).getAsJsonArray
    val out = java.util.ArrayList[Object](array.size)
    array.forEach(e => out.add(e))
    out
