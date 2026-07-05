package ls.pc.host

import java.nio.charset.StandardCharsets.UTF_8
import java.nio.file.Path

import org.eclipse.lsp4j as l
import org.eclipse.lsp4j.jsonrpc.messages.{Either as JEither, Tuple}

import ls.pc.{CompilerPluginStatus, DefinitionLocation, DefinitionOrigin}
import ls.pc.{DefinitionResult as SpiDefinitionResult, DisabledPlugin as SpiDisabledPlugin}
import ls.pc.{PcPluginStatusReport, ServicePluginStatus}
import ls.pc.host.codec.Payloads

/** The LSP4J-1.0.0 ↔ flat-`Payloads` converter, exercised against real LSP4J
  * objects on the classpath (no live PC facade). Response conversions
  * (LSP4J/spi → flat) are asserted directly; the two flat → LSP4J inputs are
  * checked by round-tripping back to flat, which proves both directions and
  * that the resolved-1.0.0 fields survive.
  */
class MarshalSuite extends munit.FunSuite:

  private def jsonBytes(s: String): Seq[Byte] = s.getBytes(UTF_8).toIndexedSeq

  private def fullItem: Payloads.CompletionItem =
    Payloads.CompletionItem(
      label = "map",
      labelDetails = Some(Payloads.LabelDetails(Some("[B]"), Some("on List"))),
      kind = Some(2), // Method
      tags = Some(Seq(1)), // Deprecated
      detail = Some("def map"),
      documentation = Some(Payloads.Documentation.Markup(Payloads.MarkupContent("markdown", "maps"))),
      deprecated = Some(false),
      preselect = Some(true),
      sortText = Some("00"),
      filterText = Some("map"),
      insertText = None,
      insertTextFormat = Some(2), // Snippet
      insertTextMode = Some(1), // AsIs
      textEdit = Some(
        Payloads.CompletionEdit.InsertReplace(
          Payloads.InsertReplaceEdit("map()", Payloads.Rng(1, 2, 1, 2), Payloads.Rng(1, 2, 1, 5))
        )
      ),
      textEditText = Some("map()"),
      additionalTextEdits = Some(Seq(Payloads.TextEdit(Payloads.Rng(0, 0, 0, 0), "import x\n"))),
      commitCharacters = Some(Seq(".")),
      command = Some(Payloads.Command("trigger", Some("tip"), "editor.action", Some(jsonBytes("[1,2]")))),
      data = Some(jsonBytes("""{"s":1}"""))
    )

  test("a fully-populated completion item round-trips flat -> LSP4J -> flat (1.0.0 fields survive)"):
    val item = fullItem
    assertEquals(Marshal.completionItem(Marshal.toLsp4jItem(item)), item)
    // Sanity: the 1.0.0-only field actually rode across LSP4J.
    assertEquals(Marshal.toLsp4jItem(item).getTextEditText, "map()")

  test("a bare completion item (all optionals absent) round-trips flat -> LSP4J -> flat"):
    val bare = Payloads.CompletionItem(
      "id", None, None, None, None, None, None, None, None, None, None, None, None, None, None, None, None, None, None
    )
    assertEquals(Marshal.completionItem(Marshal.toLsp4jItem(bare)), bare)

  test("a completion list carries incomplete, item defaults, apply-kind, and items"):
    val cl = l.CompletionList()
    cl.setIsIncomplete(true)
    val item = l.CompletionItem()
    item.setLabel("id")
    item.setTextEditText("id()")
    cl.setItems(java.util.List.of(item))
    val defaults = l.CompletionItemDefaults()
    defaults.setCommitCharacters(java.util.List.of(".", ","))
    defaults.setInsertTextFormat(l.InsertTextFormat.Snippet)
    defaults.setEditRange(JEither.forLeft(l.Range(l.Position(1, 0), l.Position(1, 4))))
    cl.setItemDefaults(defaults)
    val applyKind = l.CompletionApplyKind()
    applyKind.setCommitCharacters(l.ApplyKind.Replace)
    cl.setApplyKind(applyKind)

    val flat = Marshal.completionList(cl)
    assert(flat.isIncomplete)
    assertEquals(flat.items.map(_.label), Seq("id"))
    assertEquals(flat.items.head.textEditText, Some("id()"))
    assertEquals(flat.itemDefaults.flatMap(_.commitCharacters), Some(Seq(".", ",")))
    assertEquals(flat.itemDefaults.flatMap(_.insertTextFormat), Some(l.InsertTextFormat.Snippet.getValue))
    assertEquals(flat.itemDefaults.flatMap(_.editRange), Some(Payloads.EditRange.Range(Payloads.Rng(1, 0, 1, 4))))
    assertEquals(flat.applyKind.flatMap(_.commitCharacters), Some(l.ApplyKind.Replace.getValue))

  test("hover with markup contents and a range converts to flat"):
    val hover = l.Hover()
    hover.setContents(l.MarkupContent("markdown", "**x**"))
    hover.setRange(l.Range(l.Position(1, 2), l.Position(3, 4)))
    assertEquals(
      Marshal.hover(Some(hover)),
      Payloads.HoverResult(
        Some(Payloads.Hover(Payloads.HoverContents.Markup(Payloads.MarkupContent("markdown", "**x**")), Some(Payloads.Rng(1, 2, 3, 4))))
      )
    )

  test("hover with marked-string contents (both variants) converts to flat"):
    val hover = l.Hover()
    val contents = java.util.ArrayList[JEither[String, l.MarkedString]]()
    contents.add(JEither.forLeft("plain"))
    contents.add(JEither.forRight(l.MarkedString("scala", "def f")))
    hover.setContents(contents)
    assertEquals(
      Marshal.hover(Some(hover)).hover.get.contents,
      Payloads.HoverContents.Marked(
        Seq(Payloads.MarkedStringItem.Plain("plain"), Payloads.MarkedStringItem.Marked("scala", "def f"))
      )
    )

  test("a null hover is a None result"):
    assertEquals(Marshal.hover(None), Payloads.HoverResult(None))

  test("signature help (both parameter-label variants, docs) converts to flat"):
    val sig = l.SignatureInformation()
    sig.setLabel("def f(x: Int): Int")
    sig.setDocumentation("f doc")
    val p1 = l.ParameterInformation()
    p1.setLabel("x: Int")
    val p2 = l.ParameterInformation()
    p2.setLabel(Tuple.two(Integer.valueOf(6), Integer.valueOf(12)))
    sig.setParameters(java.util.List.of(p1, p2))
    sig.setActiveParameter(Integer.valueOf(0))
    val help = l.SignatureHelp()
    help.setSignatures(java.util.List.of(sig))
    help.setActiveSignature(Integer.valueOf(0))
    help.setActiveParameter(Integer.valueOf(1))

    val flat = Marshal.signatureHelp(help)
    assertEquals(flat.activeSignature, Some(0))
    assertEquals(flat.activeParameter, Some(1))
    val s0 = flat.signatures.head
    assertEquals(s0.label, "def f(x: Int): Int")
    assertEquals(s0.documentation, Some(Payloads.Documentation.Plain("f doc")))
    assertEquals(s0.activeParameter, Some(0))
    assertEquals(
      s0.parameters.get.map(_.label),
      Seq(Payloads.ParameterLabel.Str("x: Int"), Payloads.ParameterLabel.Offsets(6, 12))
    )

  test("a definition result carries origin markings to flat"):
    val locA = l.Location("file:///B.scala", l.Range(l.Position(0, 0), l.Position(0, 5)))
    val locB = l.Location("file:///C.scala", l.Range(l.Position(2, 1), l.Position(2, 9)))
    val spi = SpiDefinitionResult(
      "a/B#",
      Vector(
        DefinitionLocation(locA, DefinitionOrigin.Workspace),
        DefinitionLocation(locB, DefinitionOrigin.Plugin)
      )
    )
    assertEquals(
      Marshal.definition(spi),
      Payloads.DefinitionResult(
        "a/B#",
        Seq(
          Payloads.Location("file:///B.scala", Payloads.Rng(0, 0, 0, 5), Payloads.Origin.Workspace),
          Payloads.Location("file:///C.scala", Payloads.Rng(2, 1, 2, 9), Payloads.Origin.Plugin)
        )
      )
    )

  test("a plugin status report converts to flat"):
    val report = PcPluginStatusReport(
      Vector(CompilerPluginStatus(Vector("/p.jar"), Vector("-P:x"), loaded = true, "ok")),
      Vector(ServicePluginStatus("svc", "spi", enabled = true, selfTestOk = false, "failed self-test")),
      Vector(SpiDisabledPlugin("bad", "threw"))
    )
    assertEquals(
      Marshal.pluginStatus(report),
      Payloads.PluginStatus(
        Seq(Payloads.CompilerPlugin(Seq("/p.jar"), Seq("-P:x"), loaded = true, "ok")),
        Seq(Payloads.ServicePlugin("svc", "spi", enabled = true, selfTestOk = false, "failed self-test")),
        Seq(Payloads.DisabledPlugin("bad", "threw"))
      )
    )

  test("prepare-rename converts a range and a null"):
    assertEquals(
      Marshal.prepareRename(Some(l.Range(l.Position(5, 6), l.Position(5, 10)))),
      Payloads.PrepareRenameResult(Some(Payloads.Rng(5, 6, 5, 10)))
    )
    assertEquals(Marshal.prepareRename(None), Payloads.PrepareRenameResult(None))

  test("a target config decodes classpath and source dirs to paths"):
    val flat = Payloads.TargetConfig("root://build", "3.8.4", Seq("/a.jar", "/b.jar"), Seq("-deprecation"), Seq("/src"))
    val cfg = Marshal.targetConfig(flat)
    assertEquals(cfg.bspId, "root://build")
    assertEquals(cfg.scalaVersion, "3.8.4")
    assertEquals(cfg.classpath, Vector(Path.of("/a.jar"), Path.of("/b.jar")))
    assertEquals(cfg.scalacOptions, Vector("-deprecation"))
    assertEquals(cfg.sourceDirs, Vector(Path.of("/src")))
