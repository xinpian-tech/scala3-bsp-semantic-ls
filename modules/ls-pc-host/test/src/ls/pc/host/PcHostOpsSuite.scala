package ls.pc.host

import java.lang.foreign.{Arena, MemorySegment, ValueLayout}
import java.nio.charset.StandardCharsets.UTF_8
import java.nio.file.Path

import org.eclipse.lsp4j as l

import ls.pc.{CompilerPluginStatus, DefinitionLocation, DefinitionOrigin}
import ls.pc.{DefinitionResult as SpiDefinitionResult, PcPluginStatusReport, PcTargetConfig}
import ls.pc.{
  PcAutoImport,
  PcCodeActionResult,
  PcFoldingRange,
  PcInlayHint,
  PcInlayLabelPart,
  PcNotYetSupported,
  PcSemanticNode
}
import ls.pc.host.boundary.{boundary_h, LsBuf, LsStr}
import ls.pc.host.codec.Payloads

/** A stub [[PcOps]] that records the arguments each op received and returns
  * canned lsp4j / spi results, so the boundary op routing (decode → facade →
  * `Marshal` → encode → `LsBuf`) can be exercised without a live compiler. `fail
  * = true` makes the query ops throw, to check the `STATUS_INTERNAL` mapping.
  */
final class RecordingPcOps extends PcOps:
  var registered: Option[PcTargetConfig] = None
  var opened: Option[(String, String, String)] = None
  var changed: Option[(String, String)] = None
  var closed: Option[String] = None
  var lastCompletion: Option[(String, Int, Int)] = None
  var lastHover: Option[(String, Int, Int)] = None
  var lastSignatureHelp: Option[(String, Int, Int)] = None
  var lastDefinition: Option[(String, Int, Int)] = None
  var lastTypeDefinition: Option[(String, Int, Int)] = None
  var lastPrepareRename: Option[(String, Int, Int)] = None
  var lastResolve: Option[(String, l.CompletionItem, String)] = None
  var restarted = 0
  var shutdowns = 0
  var fail = false

  var completionResult: l.CompletionList = l.CompletionList(false, java.util.ArrayList[l.CompletionItem]())
  var hoverResult: Option[l.Hover] = None
  var signatureHelpResult: l.SignatureHelp = l.SignatureHelp(java.util.ArrayList[l.SignatureInformation](), null, null)
  var definitionResult: SpiDefinitionResult = SpiDefinitionResult("", Vector.empty)
  var typeDefinitionResult: SpiDefinitionResult = SpiDefinitionResult("", Vector.empty)
  var prepareRenameResult: Option[l.Range] = None
  var resolveResult: l.CompletionItem = l.CompletionItem("")
  var pluginStatusResult: PcPluginStatusReport = PcPluginStatusReport(Vector.empty, Vector.empty, Vector.empty)

  private def guard[T](result: T): T = if fail then throw RuntimeException("boom") else result

  def registerTarget(config: PcTargetConfig): Unit = registered = Some(config)
  def didOpen(targetId: String, uri: String, text: String): Unit = opened = Some((targetId, uri, text))
  def didChange(uri: String, text: String): Unit = changed = Some((uri, text))
  def didClose(uri: String): Unit = closed = Some(uri)
  def completion(uri: String, line: Int, character: Int): l.CompletionList =
    lastCompletion = Some((uri, line, character)); guard(completionResult)
  def completionItemResolve(targetId: String, item: l.CompletionItem, symbol: String): l.CompletionItem =
    lastResolve = Some((targetId, item, symbol)); guard(resolveResult)
  def hover(uri: String, line: Int, character: Int): Option[l.Hover] =
    lastHover = Some((uri, line, character)); guard(hoverResult)
  def signatureHelp(uri: String, line: Int, character: Int): l.SignatureHelp =
    lastSignatureHelp = Some((uri, line, character)); guard(signatureHelpResult)
  def definition(uri: String, line: Int, character: Int): SpiDefinitionResult =
    lastDefinition = Some((uri, line, character)); guard(definitionResult)
  def typeDefinition(uri: String, line: Int, character: Int): SpiDefinitionResult =
    lastTypeDefinition = Some((uri, line, character)); guard(typeDefinitionResult)
  def prepareRename(uri: String, line: Int, character: Int): Option[l.Range] =
    lastPrepareRename = Some((uri, line, character)); guard(prepareRenameResult)
  def pluginStatus: PcPluginStatusReport = guard(pluginStatusResult)
  def restartInstances(): Unit = restarted += 1
  def shutdown(): Unit = shutdowns += 1

  // ABI v2 payload-query ops. `notYet = true` makes them answer the transport
  // stub (throw [[PcNotYetSupported]]), like [[FacadePcOps]] before the
  // provider task lands.
  var notYet = false
  var lastInlayHints: Option[(String, l.Range, Int)] = None
  var lastSemanticTokens: Option[String] = None
  var lastSelectionRanges: Option[(String, Vector[l.Position])] = None
  var lastCodeAction: Option[(String, Int, l.Position, Option[l.Position], Option[Vector[Int]])] = None
  var lastAutoImports: Option[(String, l.Position, String, Boolean)] = None
  var lastPcDiagnostics: Option[String] = None
  var lastFoldingRanges: Option[String] = None

  var inlayHintsResult: Vector[PcInlayHint] = Vector.empty
  var semanticTokensResult: Vector[PcSemanticNode] = Vector.empty
  var selectionRangesResult: Vector[Vector[l.Range]] = Vector.empty
  var codeActionResult: PcCodeActionResult = PcCodeActionResult(Vector.empty, None)
  var autoImportsResult: Vector[PcAutoImport] = Vector.empty
  var pcDiagnosticsResult: Vector[l.Diagnostic] = Vector.empty
  var foldingRangesResult: Vector[PcFoldingRange] = Vector.empty

  private def stubbed[T](op: String)(result: T): T =
    if notYet then throw PcNotYetSupported(op) else guard(result)

  def inlayHints(uri: String, range: l.Range, flags: Int): Vector[PcInlayHint] =
    lastInlayHints = Some((uri, range, flags)); stubbed("inlayHints")(inlayHintsResult)
  def semanticTokens(uri: String): Vector[PcSemanticNode] =
    lastSemanticTokens = Some(uri); stubbed("semanticTokens")(semanticTokensResult)
  def selectionRanges(uri: String, positions: Vector[l.Position]): Vector[Vector[l.Range]] =
    lastSelectionRanges = Some((uri, positions)); stubbed("selectionRanges")(selectionRangesResult)
  def codeAction(
      uri: String,
      actionId: Int,
      position: l.Position,
      extractionEnd: Option[l.Position],
      argIndices: Option[Vector[Int]]
  ): PcCodeActionResult =
    lastCodeAction = Some((uri, actionId, position, extractionEnd, argIndices))
    stubbed("codeAction")(codeActionResult)
  def autoImports(uri: String, position: l.Position, name: String, isExtension: Boolean): Vector[PcAutoImport] =
    lastAutoImports = Some((uri, position, name, isExtension)); stubbed("autoImports")(autoImportsResult)
  def pcDiagnostics(uri: String): Vector[l.Diagnostic] =
    lastPcDiagnostics = Some(uri); stubbed("pcDiagnostics")(pcDiagnosticsResult)
  def foldingRanges(uri: String): Vector[PcFoldingRange] =
    lastFoldingRanges = Some(uri); stubbed("foldingRanges")(foldingRangesResult)

/** The 22 boundary ops routed through a [[PcHost]] over a stub facade: each op
  * decodes its request, calls the facade, converts the result with [[Marshal]],
  * and writes the flat response — no live compiler or booted JVM. */
class PcHostOpsSuite extends munit.FunSuite:

  private def OK = boundary_h.STATUS_OK()
  private def DECODE = boundary_h.STATUS_DECODE()
  private def INTERNAL = boundary_h.STATUS_INTERNAL()

  private def run(ops: PcOps, loan: Int => Unit = _ => ())(body: (Arena, PcHost) => Unit): Unit =
    val arena = Arena.ofConfined()
    try
      val host = PcHost(PcHostRuntime(size => arena.allocate(size.toLong)), ops, loan)
      body(arena, host)
    finally arena.close()

  private def req(arena: Arena, bytes: Array[Byte]): (MemorySegment, Int) =
    val buf = arena.allocate(bytes.length.toLong)
    MemorySegment.copy(bytes, 0, buf, ValueLayout.JAVA_BYTE, 0L, bytes.length)
    (buf, bytes.length)

  private def lsStr(arena: Arena, s: String): MemorySegment =
    val bytes = s.getBytes(UTF_8)
    val buf = arena.allocate((bytes.length max 1).toLong)
    MemorySegment.copy(bytes, 0, buf, ValueLayout.JAVA_BYTE, 0L, bytes.length)
    val st = LsStr.allocate(arena)
    LsStr.ptr(st, buf)
    LsStr.len(st, bytes.length)
    st

  private def outBytes(out: MemorySegment): Array[Byte] =
    LsBuf.ptr(out).reinterpret(LsBuf.len(out).toLong).toArray(ValueLayout.JAVA_BYTE)

  test("register_target decodes the config through Marshal and calls the facade"):
    val ops = RecordingPcOps()
    run(ops) { (arena, host) =>
      val (p, n) = req(arena, Payloads.TargetConfig("bId", "3.8.4", Seq("/a.jar"), Seq("-x"), Seq("/src")).encode())
      assertEquals(host.registerTarget(p, n), OK)
      assertEquals(ops.registered.map(_.bspId), Some("bId"))
      assertEquals(ops.registered.map(_.classpath), Some(Vector(Path.of("/a.jar"))))
      assertEquals(ops.registered.map(_.sourceDirs), Some(Vector(Path.of("/src"))))
    }

  test("did_open / did_change / did_close route to the facade"):
    val ops = RecordingPcOps()
    run(ops) { (arena, host) =>
      val (p1, n1) = req(arena, Payloads.DidOpenParams("t", "u", "x").encode())
      assertEquals(host.didOpen(p1, n1), OK)
      assertEquals(ops.opened, Some(("t", "u", "x")))
      val (p2, n2) = req(arena, Payloads.DidChangeParams("u", "y").encode())
      assertEquals(host.didChange(p2, n2), OK)
      assertEquals(ops.changed, Some(("u", "y")))
      assertEquals(host.didClose(lsStr(arena, "u")), OK)
      assertEquals(ops.closed, Some("u"))
    }

  test("completion routes decode -> facade -> encode"):
    val ops = RecordingPcOps()
    val item = l.CompletionItem("map")
    item.setTextEditText("map()")
    ops.completionResult = l.CompletionList(true, java.util.List.of(item))
    run(ops) { (arena, host) =>
      val out = LsBuf.allocate(arena)
      assertEquals(host.completion(lsStr(arena, "file:///A.scala"), 3, 7, out), OK)
      assertEquals(ops.lastCompletion, Some(("file:///A.scala", 3, 7)))
      assertEquals(Payloads.CompletionList.decode(outBytes(out)), Marshal.completionList(ops.completionResult))
    }

  test("hover routes decode -> facade -> encode"):
    val ops = RecordingPcOps()
    val h = l.Hover()
    h.setContents(l.MarkupContent("markdown", "**x**"))
    h.setRange(l.Range(l.Position(1, 2), l.Position(1, 5)))
    ops.hoverResult = Some(h)
    run(ops) { (arena, host) =>
      val out = LsBuf.allocate(arena)
      assertEquals(host.hover(lsStr(arena, "u"), 1, 2, out), OK)
      assertEquals(ops.lastHover, Some(("u", 1, 2)))
      assertEquals(Payloads.HoverResult.decode(outBytes(out)), Marshal.hover(ops.hoverResult))
    }

  test("signature_help routes decode -> facade -> encode"):
    val ops = RecordingPcOps()
    val sig = l.SignatureInformation()
    sig.setLabel("def f(x: Int): Int")
    ops.signatureHelpResult = l.SignatureHelp(java.util.List.of(sig), Integer.valueOf(0), null)
    run(ops) { (arena, host) =>
      val out = LsBuf.allocate(arena)
      assertEquals(host.signatureHelp(lsStr(arena, "u"), 4, 8, out), OK)
      assertEquals(ops.lastSignatureHelp, Some(("u", 4, 8)))
      assertEquals(Payloads.SignatureHelp.decode(outBytes(out)), Marshal.signatureHelp(ops.signatureHelpResult))
    }

  test("definition and type_definition route decode -> facade -> encode"):
    val ops = RecordingPcOps()
    val loc = l.Location("file:///B.scala", l.Range(l.Position(0, 0), l.Position(0, 3)))
    ops.definitionResult = SpiDefinitionResult("a/B#", Vector(DefinitionLocation(loc, DefinitionOrigin.Workspace)))
    ops.typeDefinitionResult = SpiDefinitionResult("a/T#", Vector(DefinitionLocation(loc, DefinitionOrigin.Plugin)))
    run(ops) { (arena, host) =>
      val out1 = LsBuf.allocate(arena)
      assertEquals(host.definition(lsStr(arena, "u"), 1, 1, out1), OK)
      assertEquals(ops.lastDefinition, Some(("u", 1, 1)))
      assertEquals(Payloads.DefinitionResult.decode(outBytes(out1)), Marshal.definition(ops.definitionResult))
      val out2 = LsBuf.allocate(arena)
      assertEquals(host.typeDefinition(lsStr(arena, "u"), 2, 2, out2), OK)
      assertEquals(ops.lastTypeDefinition, Some(("u", 2, 2)))
      assertEquals(Payloads.DefinitionResult.decode(outBytes(out2)), Marshal.definition(ops.typeDefinitionResult))
    }

  test("prepare_rename routes decode -> facade -> encode"):
    val ops = RecordingPcOps()
    ops.prepareRenameResult = Some(l.Range(l.Position(5, 6), l.Position(5, 10)))
    run(ops) { (arena, host) =>
      val out = LsBuf.allocate(arena)
      assertEquals(host.prepareRename(lsStr(arena, "u"), 5, 6, out), OK)
      assertEquals(ops.lastPrepareRename, Some(("u", 5, 6)))
      assertEquals(Payloads.PrepareRenameResult.decode(outBytes(out)), Marshal.prepareRename(ops.prepareRenameResult))
    }

  test("completion_resolve reconstructs the item, calls the facade, and encodes the result"):
    val ops = RecordingPcOps()
    val resolved = l.CompletionItem("resolved")
    resolved.setDetail("def map[B]")
    ops.resolveResult = resolved
    val flatItem = Payloads.CompletionItem(
      "orig", None, None, None, None, None, None, None, None, None, None, None, None, None, None, None, None, None, None
    )
    run(ops) { (arena, host) =>
      val (ip, il) = req(arena, flatItem.encode())
      val out = LsBuf.allocate(arena)
      assertEquals(host.completionResolve(lsStr(arena, "tgt"), lsStr(arena, "sym"), ip, il, out), OK)
      assertEquals(ops.lastResolve.map(_._1), Some("tgt"))
      assertEquals(ops.lastResolve.map(_._3), Some("sym"))
      assertEquals(ops.lastResolve.map(_._2.getLabel), Some("orig"))
      assertEquals(Payloads.CompletionItem.decode(outBytes(out)), Marshal.completionItem(ops.resolveResult))
    }

  test("plugin_status routes to the facade and encodes"):
    val ops = RecordingPcOps()
    ops.pluginStatusResult = PcPluginStatusReport(
      Vector(CompilerPluginStatus(Vector("/p.jar"), Vector("-P:x"), loaded = true, "ok")),
      Vector.empty,
      Vector.empty
    )
    run(ops) { (arena, host) =>
      val out = LsBuf.allocate(arena)
      assertEquals(host.pluginStatus(out), OK)
      assertEquals(Payloads.PluginStatus.decode(outBytes(out)), Marshal.pluginStatus(ops.pluginStatusResult))
    }

  test("restart_instances and shutdown route to the facade"):
    val ops = RecordingPcOps()
    run(ops) { (arena, host) =>
      assertEquals(host.restartInstances(), OK)
      assertEquals(ops.restarted, 1)
      assertEquals(host.shutdown(), OK)
      assertEquals(ops.shutdowns, 1)
    }

  test("spawn_dispatch loans a thread for the requested generation"):
    val ops = RecordingPcOps()
    var loaned: List[Int] = Nil
    run(ops, gen => loaned = gen :: loaned) { (arena, host) =>
      assertEquals(host.spawnDispatch(7), OK)
      assertEquals(loaned, List(7))
    }

  test("inlay_hints routes decode -> facade -> encode"):
    val ops = RecordingPcOps()
    ops.inlayHintsResult = Vector(
      PcInlayHint(
        l.Position(2, 10),
        Vector(PcInlayLabelPart(": Int", None, Some("inferred"))),
        kind = 1,
        paddingLeft = true,
        paddingRight = false,
        textEdits = Some(Vector(l.TextEdit(l.Range(l.Position(2, 10), l.Position(2, 10)), ": Int"))),
        data = Some(Seq[Byte](1, 2))
      )
    )
    run(ops) { (arena, host) =>
      val (p, n) = req(arena, Payloads.InlayHintParams("u", Payloads.Rng(0, 0, 9, 0), 3).encode())
      val out = LsBuf.allocate(arena)
      assertEquals(host.inlayHints(p, n, out), OK)
      assertEquals(ops.lastInlayHints.map(_._1), Some("u"))
      assertEquals(ops.lastInlayHints.map(_._2), Some(l.Range(l.Position(0, 0), l.Position(9, 0))))
      assertEquals(ops.lastInlayHints.map(_._3), Some(3))
      assertEquals(Payloads.InlayHintsResult.decode(outBytes(out)), Marshal.inlayHints(ops.inlayHintsResult))
    }

  test("semantic_tokens routes decode -> facade -> encode"):
    val ops = RecordingPcOps()
    ops.semanticTokensResult = Vector(PcSemanticNode(0, 6, 3, 1))
    run(ops) { (arena, host) =>
      val (p, n) = req(arena, Payloads.UriParams("u").encode())
      val out = LsBuf.allocate(arena)
      assertEquals(host.semanticTokens(p, n, out), OK)
      assertEquals(ops.lastSemanticTokens, Some("u"))
      assertEquals(
        Payloads.SemanticTokensResult.decode(outBytes(out)),
        Marshal.semanticTokens(ops.semanticTokensResult)
      )
    }

  test("selection_range routes decode -> facade -> encode"):
    val ops = RecordingPcOps()
    ops.selectionRangesResult = Vector(
      Vector(l.Range(l.Position(1, 2), l.Position(1, 4)), l.Range(l.Position(0, 0), l.Position(9, 0))),
      Vector.empty
    )
    run(ops) { (arena, host) =>
      val (p, n) = req(
        arena,
        Payloads.SelectionRangeParams("u", Seq(Payloads.Pos(1, 2), Payloads.Pos(3, 4))).encode()
      )
      val out = LsBuf.allocate(arena)
      assertEquals(host.selectionRange(p, n, out), OK)
      assertEquals(ops.lastSelectionRanges.map(_._1), Some("u"))
      assertEquals(
        ops.lastSelectionRanges.map(_._2),
        Some(Vector(l.Position(1, 2), l.Position(3, 4)))
      )
      assertEquals(
        Payloads.SelectionRangesResult.decode(outBytes(out)),
        Marshal.selectionRanges(ops.selectionRangesResult)
      )
    }

  test("code_action routes decode -> facade -> encode (refusal is data)"):
    val ops = RecordingPcOps()
    ops.codeActionResult = PcCodeActionResult(Vector.empty, Some("Cannot extract selection"))
    run(ops) { (arena, host) =>
      val (p, n) = req(
        arena,
        Payloads
          .CodeActionParams(
            "u",
            Payloads.CodeActionId.ExtractMethod,
            Payloads.Pos(5, 1),
            Some(Payloads.Pos(7, 2)),
            Some(Seq(0, 2))
          )
          .encode()
      )
      val out = LsBuf.allocate(arena)
      assertEquals(host.codeAction(p, n, out), OK)
      assertEquals(
        ops.lastCodeAction,
        Some(
          (
            "u",
            Payloads.CodeActionId.ExtractMethod,
            l.Position(5, 1),
            Some(l.Position(7, 2)),
            Some(Vector(0, 2))
          )
        )
      )
      val decoded = Payloads.CodeActionResult.decode(outBytes(out))
      assertEquals(decoded, Marshal.codeActionResult(ops.codeActionResult))
      assertEquals(decoded.refusal, Some("Cannot extract selection"))
    }

  test("auto_imports routes decode -> facade -> encode"):
    val ops = RecordingPcOps()
    ops.autoImportsResult = Vector(
      PcAutoImport(
        "scala.concurrent",
        Vector(l.TextEdit(l.Range(l.Position(0, 0), l.Position(0, 0)), "import scala.concurrent.Future\n")),
        Some("scala/concurrent/Future#")
      )
    )
    run(ops) { (arena, host) =>
      val (p, n) =
        req(arena, Payloads.AutoImportParams("u", Payloads.Pos(4, 9), "Future", isExtension = true).encode())
      val out = LsBuf.allocate(arena)
      assertEquals(host.autoImports(p, n, out), OK)
      assertEquals(ops.lastAutoImports, Some(("u", l.Position(4, 9), "Future", true)))
      assertEquals(
        Payloads.AutoImportsResult.decode(outBytes(out)),
        Marshal.autoImports(ops.autoImportsResult)
      )
    }

  test("pc_diagnostics routes decode -> facade -> encode"):
    val ops = RecordingPcOps()
    val diag = l.Diagnostic(l.Range(l.Position(3, 0), l.Position(3, 5)), "not found: value x")
    diag.setSeverity(l.DiagnosticSeverity.Error)
    diag.setCode("E007")
    ops.pcDiagnosticsResult = Vector(diag)
    run(ops) { (arena, host) =>
      val (p, n) = req(arena, Payloads.UriParams("u").encode())
      val out = LsBuf.allocate(arena)
      assertEquals(host.pcDiagnostics(p, n, out), OK)
      assertEquals(ops.lastPcDiagnostics, Some("u"))
      val decoded = Payloads.PcDiagnosticsResult.decode(outBytes(out))
      assertEquals(decoded, Marshal.pcDiagnostics(ops.pcDiagnosticsResult))
      assertEquals(decoded.diagnostics.head.code, "E007")
      assertEquals(decoded.diagnostics.head.severity, 1)
    }

  test("folding_range routes decode -> facade -> encode"):
    val ops = RecordingPcOps()
    ops.foldingRangesResult = Vector(
      PcFoldingRange(l.Range(l.Position(0, 0), l.Position(5, 1)), Payloads.FoldingKind.Imports)
    )
    run(ops) { (arena, host) =>
      val (p, n) = req(arena, Payloads.UriParams("u").encode())
      val out = LsBuf.allocate(arena)
      assertEquals(host.foldingRange(p, n, out), OK)
      assertEquals(ops.lastFoldingRanges, Some("u"))
      assertEquals(
        Payloads.FoldingRangesResult.decode(outBytes(out)),
        Marshal.foldingRanges(ops.foldingRangesResult)
      )
    }

  test("a not-yet-provided payload-query op maps to STATUS_NOT_YET"):
    // The transport-stub phase: the op crosses the boundary, decodes cleanly,
    // reaches the facade seam, and the typed PcNotYetSupported answer maps to
    // the distinct nonzero STATUS_NOT_YET — never a panic and never one of the
    // generic error statuses.
    val ops = RecordingPcOps()
    ops.notYet = true
    run(ops) { (arena, host) =>
      val out = LsBuf.allocate(arena)
      val (ip, in_) = req(arena, Payloads.InlayHintParams("u", Payloads.Rng(0, 0, 1, 0), 0).encode())
      assertEquals(host.inlayHints(ip, in_, out), boundary_h.STATUS_NOT_YET())
      val (up, un) = req(arena, Payloads.UriParams("u").encode())
      assertEquals(host.semanticTokens(up, un, out), boundary_h.STATUS_NOT_YET())
      assertEquals(host.pcDiagnostics(up, un, out), boundary_h.STATUS_NOT_YET())
      assertEquals(host.foldingRange(up, un, out), boundary_h.STATUS_NOT_YET())
      val (sp, sn) = req(arena, Payloads.SelectionRangeParams("u", Seq(Payloads.Pos(0, 0))).encode())
      assertEquals(host.selectionRange(sp, sn, out), boundary_h.STATUS_NOT_YET())
      val (cp, cn) = req(
        arena,
        Payloads.CodeActionParams("u", Payloads.CodeActionId.InlineValue, Payloads.Pos(0, 0), None, None).encode()
      )
      assertEquals(host.codeAction(cp, cn, out), boundary_h.STATUS_NOT_YET())
      val (ap, an) =
        req(arena, Payloads.AutoImportParams("u", Payloads.Pos(0, 0), "X", isExtension = false).encode())
      assertEquals(host.autoImports(ap, an, out), boundary_h.STATUS_NOT_YET())
      // The requests still reached the op seam (decode happened before the stub).
      assertEquals(ops.lastInlayHints.map(_._1), Some("u"))
      assertEquals(ops.lastAutoImports.map(_._3), Some("X"))
    }

  test("a malformed payload-query request maps to STATUS_DECODE before the facade"):
    val ops = RecordingPcOps()
    run(ops) { (arena, host) =>
      val out = LsBuf.allocate(arena)
      // A uri-params buffer is not an inlay-hint params buffer (wrong kind).
      val (p, n) = req(arena, Payloads.UriParams("u").encode())
      assertEquals(host.inlayHints(p, n, out), DECODE)
      assertEquals(ops.lastInlayHints, None)
    }

  test("a malformed request payload maps to STATUS_DECODE"):
    val ops = RecordingPcOps()
    run(ops) { (arena, host) =>
      val (p, n) = req(arena, Array[Byte](1, 2, 3)) // too short for a did_open envelope
      assertEquals(host.didOpen(p, n), DECODE)
      assertEquals(ops.opened, None)
    }

  test("a facade throwable maps to STATUS_INTERNAL"):
    val ops = RecordingPcOps()
    ops.fail = true
    run(ops) { (arena, host) =>
      val out = LsBuf.allocate(arena)
      assertEquals(host.completion(lsStr(arena, "u"), 0, 0, out), INTERNAL)
    }

  test("a failed thread loan maps to STATUS_INTERNAL"):
    val ops = RecordingPcOps()
    run(ops, _ => throw RuntimeException("no thread")) { (arena, host) =>
      assertEquals(host.spawnDispatch(2), INTERNAL)
    }
