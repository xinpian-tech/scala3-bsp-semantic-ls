package ls.pc.host.codec

import scala.io.Source

import ls.pc.host.codec.Payloads.*

/** Cross-language parity for the flat codec. For each payload (carrier-free and
  * LSP4J-carrier alike) the suite (a) decodes the Rust-produced golden vector
  * and asserts the fields, (b) re-encodes the equivalent Java instance and
  * asserts the bytes are identical to the golden, and (c) checks that malformed
  * buffers decode to a typed [[CodecException]] rather than crashing. The
  * goldens are produced by the Rust `ls-pc-abi` encoder (see
  * `test/resources/codec-vectors.txt`), so a byte mismatch means the two sides
  * disagree on the wire format.
  */
class CodecSuite extends munit.FunSuite:

  private val vectors: Map[String, Array[Byte]] =
    val stream = getClass.getResourceAsStream("/codec-vectors.txt")
    assert(stream != null, "codec-vectors.txt not on the test classpath")
    val source = Source.fromInputStream(stream, "UTF-8")
    try
      source
        .getLines()
        .map(_.trim)
        .filter(l => l.nonEmpty && !l.startsWith("#"))
        .map { line =>
          val eq = line.indexOf('=')
          (line.substring(0, eq), hexToBytes(line.substring(eq + 1)))
        }
        .toMap
    finally source.close()

  private def hexToBytes(hex: String): Array[Byte] =
    Array.tabulate(hex.length / 2)(i => Integer.parseInt(hex.substring(2 * i, 2 * i + 2), 16).toByte)

  private def golden(name: String): Array[Byte] =
    vectors.getOrElse(name, fail(s"golden vector '$name' missing"))

  /** Asserts both directions: the Java instance encodes to the golden bytes,
    * and the golden bytes decode back to the Java instance.
    */
  private def parity[T](name: String, instance: T, encode: T => Array[Byte], decode: Array[Byte] => T)(
      using munit.Location
  ): Unit =
    val g = golden(name)
    assertEquals(encode(instance).toList, g.toList, s"$name encode mismatch")
    assertEquals(decode(g), instance, s"$name decode mismatch")

  test("target_config round-trips against the Rust golden"):
    parity(
      "target_config",
      TargetConfig("root://build", "3.8.4", Seq("/a.jar", "/b.jar"), Seq("-deprecation"), Seq.empty),
      _.encode(),
      TargetConfig.decode
    )

  test("did_open round-trips (unicode text preserved)"):
    parity(
      "did_open",
      DidOpenParams("t1", "file:///Main.scala", "object Main // café"),
      _.encode(),
      DidOpenParams.decode
    )

  test("did_change round-trips (empty text distinct from a missing field)"):
    parity(
      "did_change",
      DidChangeParams("file:///Main.scala", ""),
      _.encode(),
      DidChangeParams.decode
    )

  test("position round-trips"):
    parity(
      "position",
      PositionParams("file:///X.scala", 10, 4),
      _.encode(),
      PositionParams.decode
    )

  test("hover with markup contents and a range round-trips"):
    parity(
      "hover_markup",
      HoverResult(
        Some(Hover(HoverContents.Markup(MarkupContent("markdown", "**x**: Int")), Some(Rng(1, 2, 3, 4))))
      ),
      _.encode(),
      HoverResult.decode
    )

  test("hover with marked-string contents (both variants) round-trips"):
    parity(
      "hover_marked",
      HoverResult(
        Some(
          Hover(
            HoverContents.Marked(
              Seq(MarkedStringItem.Plain("plain"), MarkedStringItem.Marked("scala", "def f"))
            ),
            None
          )
        )
      ),
      _.encode(),
      HoverResult.decode
    )

  test("a null hover is distinct from a present-but-empty hover"):
    parity("hover_none", HoverResult(None), _.encode(), HoverResult.decode)

  test("definition with multiple origin-tagged locations round-trips"):
    parity(
      "definition",
      DefinitionResult(
        "a/B#",
        Seq(
          Location("file:///B.scala", Rng(0, 0, 0, 5), Origin.Workspace),
          Location("file:///C.scala", Rng(2, 1, 2, 9), Origin.Plugin)
        )
      ),
      _.encode(),
      DefinitionResult.decode
    )

  test("symbol_definition locations round-trip"):
    parity(
      "locations",
      LocationsResult(Seq(Location("file:///D.scala", Rng(3, 0, 3, 4), Origin.Synthetic))),
      _.encode(),
      LocationsResult.decode
    )

  test("an empty locations list round-trips"):
    parity("locations_empty", LocationsResult(Seq.empty), _.encode(), LocationsResult.decode)

  test("prepare-rename with a range round-trips"):
    parity(
      "prepare_rename_some",
      PrepareRenameResult(Some(Rng(5, 6, 5, 10))),
      _.encode(),
      PrepareRenameResult.decode
    )

  test("a non-renameable prepare-rename (null) round-trips"):
    parity("prepare_rename_none", PrepareRenameResult(None), _.encode(), PrepareRenameResult.decode)

  test("plugin status round-trips compiler/service/disabled entries"):
    parity(
      "plugin_status",
      PluginStatus(
        Seq(CompilerPlugin(Seq("/p.jar"), Seq("-P:x"), loaded = true, "ok")),
        Seq(ServicePlugin("svc", "spi", enabled = true, selfTestOk = false, "failed self-test")),
        Seq(DisabledPlugin("bad", "threw"))
      ),
      _.encode(),
      PluginStatus.decode
    )

  // ---- LSP4J-carrier payloads (completion / resolve / signature help). ----

  private def itemFull: CompletionItem =
    CompletionItem(
      "map",
      Some(LabelDetails(Some("[B]"), None)),
      Some(2),
      Some(Seq(1)),
      Some("def map"),
      Some(Documentation.Markup(MarkupContent("markdown", "maps"))),
      Some(false),
      Some(true),
      Some("00"),
      Some("map"),
      None,
      Some(2),
      Some(1),
      Some(CompletionEdit.InsertReplace(InsertReplaceEdit("map()", Rng(1, 2, 1, 2), Rng(1, 2, 1, 5)))),
      Some("map()"),
      Some(Seq(TextEdit(Rng(0, 0, 0, 0), "import x\n"))),
      Some(Seq(".")),
      Some(Command("trigger", Some("tip"), "editor.action", Some(Seq[Byte](0x7b, 0x7d)))),
      Some(Seq[Byte](1, 2, 3))
    )

  private def itemPlain: CompletionItem =
    CompletionItem(
      "id",
      None,
      None,
      None,
      None,
      Some(Documentation.Plain("plain doc")),
      None,
      None,
      None,
      None,
      Some("id"),
      None,
      None,
      Some(CompletionEdit.Plain(TextEdit(Rng(3, 0, 3, 2), "id"))),
      None,
      None,
      None,
      None,
      None
    )

  private def itemNone: CompletionItem =
    CompletionItem(
      "",
      None,
      None,
      None,
      None,
      None,
      None,
      None,
      None,
      None,
      None,
      None,
      None,
      None,
      None,
      None,
      None,
      None,
      None
    )

  test("a fully-populated completion item (all 1.0.0 fields) round-trips"):
    parity("completion_item_full", itemFull, _.encode(), CompletionItem.decode)

  test("a completion item with a plain text edit and plain documentation round-trips"):
    parity("completion_item_plain", itemPlain, _.encode(), CompletionItem.decode)

  test("a bare completion item (all optionals absent) round-trips"):
    parity("completion_item_none", itemNone, _.encode(), CompletionItem.decode)

  test("a completion list with defaults, apply-kind, and items round-trips"):
    parity(
      "completion_list",
      CompletionList(
        isIncomplete = true,
        Some(
          CompletionItemDefaults(
            Some(Seq(".", ",")),
            Some(EditRange.InsertReplace(Rng(1, 0, 1, 0), Rng(1, 0, 1, 4))),
            Some(2),
            None,
            Some(Seq[Byte](9))
          )
        ),
        Some(CompletionApplyKind(Some(1), None)),
        Seq(itemFull, itemNone)
      ),
      _.encode(),
      CompletionList.decode
    )

  test("an empty completion list round-trips (empty items distinct from null)"):
    parity(
      "completion_list_empty",
      CompletionList(isIncomplete = false, None, None, Seq.empty),
      _.encode(),
      CompletionList.decode
    )

  test("resolve params (an item to enrich) round-trips"):
    parity(
      "resolve_params",
      ResolveParams("t1", "a/B#map().", itemPlain),
      _.encode(),
      ResolveParams.decode
    )

  test("signature help (both parameter-label variants, docs) round-trips"):
    parity(
      "signature_help",
      SignatureHelp(
        Seq(
          SignatureInfo(
            "def f(x: Int): Int",
            Some(Documentation.Plain("f doc")),
            Some(
              Seq(
                ParameterInfo(ParameterLabel.Str("x: Int"), None),
                ParameterInfo(
                  ParameterLabel.Offsets(6, 12),
                  Some(Documentation.Markup(MarkupContent("markdown", "the arg")))
                )
              )
            ),
            Some(0)
          )
        ),
        Some(0),
        Some(1)
      ),
      _.encode(),
      SignatureHelp.decode
    )

  test("an empty signature help round-trips"):
    parity(
      "signature_help_empty",
      SignatureHelp(Seq.empty, None, None),
      _.encode(),
      SignatureHelp.decode
    )

  // ---- Malformed buffers decode to a typed error, never a crash. ----

  test("a bad envelope magic is a typed decode error"):
    val buf = golden("position").clone()
    buf(0) = 0x00
    intercept[CodecException](PositionParams.decode(buf))

  test("a wrong payload kind is a typed decode error"):
    intercept[CodecException](DidOpenParams.decode(golden("target_config")))

  test("a truncated buffer (declared length != actual) is a typed decode error"):
    intercept[CodecException](PositionParams.decode(golden("position").dropRight(4)))

  test("a buffer shorter than the envelope is a typed decode error"):
    intercept[CodecException](PositionParams.decode(Array[Byte](1, 2, 3)))

  test("a fabricated huge list count is rejected before allocation"):
    // A LOCATIONS envelope whose body is a single count of 0xffffffff.
    val w = Codec.Writer()
    w.u32(0xffffffff)
    val buf = w.finish(Payloads.KindLocations)
    intercept[CodecException](LocationsResult.decode(buf))

  test("an out-of-range blob slice is a typed decode error"):
    // A LOCATIONS body claiming one location whose uri BlobStr points past the
    // (empty) blob.
    val w = Codec.Writer()
    w.u32(1) // one location
    w.u32(0) // uri offset
    w.u32(64) // uri len — past the empty blob
    w.range(0, 0, 0, 0)
    w.u32(0) // origin
    val buf = w.finish(Payloads.KindLocations)
    intercept[CodecException](LocationsResult.decode(buf))

  test("a required string with invalid UTF-8 is a typed decode error"):
    // Reject malformed UTF-8 like Rust's str::from_utf8, not substitute U+FFFD.
    val w = Codec.Writer()
    w.str("y")
    val buf = w.finish(0x7fffffff)
    buf(buf.length - 1) = 0xff.toByte // corrupt the "y" blob byte (0xff is never valid UTF-8)
    intercept[CodecException](Codec.Reader(buf, 0x7fffffff).str())

  test("an optional string with invalid UTF-8 is a typed decode error"):
    val w = Codec.Writer()
    w.optStr(Some("y"))
    val buf = w.finish(0x7fffffff)
    buf(buf.length - 1) = 0xff.toByte
    intercept[CodecException](Codec.Reader(buf, 0x7fffffff).optStr())
