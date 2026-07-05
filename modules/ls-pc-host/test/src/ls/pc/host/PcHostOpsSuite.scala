package ls.pc.host

import java.lang.foreign.{Arena, MemorySegment, ValueLayout}
import java.nio.charset.StandardCharsets.UTF_8
import java.nio.file.Path

import org.eclipse.lsp4j as l

import ls.pc.{CompilerPluginStatus, DefinitionLocation, DefinitionOrigin}
import ls.pc.{DefinitionResult as SpiDefinitionResult, PcPluginStatusReport, PcTargetConfig}
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

/** The 15 boundary ops routed through a [[PcHost]] over a stub facade: each op
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
