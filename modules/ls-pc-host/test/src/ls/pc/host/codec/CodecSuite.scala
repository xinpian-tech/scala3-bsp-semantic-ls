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

  test("search_methods hits round-trip"):
    parity(
      "method_hits",
      MethodHitsResult(
        Seq(MethodHit("file:///E.scala", "a/b/A$package.incr().", 3, Rng(1, 6, 1, 10)))
      ),
      _.encode(),
      MethodHitsResult.decode
    )

  test("an empty method-hits list round-trips"):
    parity("method_hits_empty", MethodHitsResult(Seq.empty), _.encode(), MethodHitsResult.decode)

  test("a method-hits buffer is not decodable as locations (distinct kinds)"):
    intercept[CodecException](LocationsResult.decode(golden("method_hits")))

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

  // ---- ABI v2 payload-query carriers. ----

  test("inlay-hint params round-trip"):
    parity(
      "inlay_hint_params",
      InlayHintParams("file:///H.scala", Rng(0, 0, 20, 0), 3),
      _.encode(),
      InlayHintParams.decode
    )

  test("inlay hints (label parts, padding, edits, opaque data) round-trip"):
    parity(
      "inlay_hints",
      InlayHintsResult(
        Seq(
          InlayHint(
            Pos(2, 10),
            Seq(
              InlayLabelPart(": Int", Some(("file:///I.scala", Rng(1, 0, 1, 3))), Some("inferred type")),
              InlayLabelPart("=>", None, None)
            ),
            kind = 1,
            paddingLeft = true,
            paddingRight = false,
            textEdits = Some(Seq(TextEdit(Rng(2, 10, 2, 10), ": Int"))),
            data = Some(Seq[Byte](1, 2, 3))
          )
        )
      ),
      _.encode(),
      InlayHintsResult.decode
    )

  test("an empty inlay-hints list round-trips"):
    parity("inlay_hints_empty", InlayHintsResult(Seq.empty), _.encode(), InlayHintsResult.decode)

  test("uri params round-trip"):
    parity("uri_params", UriParams("file:///U.scala"), _.encode(), UriParams.decode)

  test("semantic tokens round-trip as offsets"):
    parity(
      "semantic_tokens",
      SemanticTokensResult(Seq(SemanticNode(0, 6, 3, 1), SemanticNode(10, 14, 15, 0))),
      _.encode(),
      SemanticTokensResult.decode
    )

  test("an empty semantic-tokens list round-trips"):
    parity(
      "semantic_tokens_empty",
      SemanticTokensResult(Seq.empty),
      _.encode(),
      SemanticTokensResult.decode
    )

  test("selection-range params round-trip"):
    parity(
      "selection_range_params",
      SelectionRangeParams("file:///S.scala", Seq(Pos(1, 2), Pos(3, 4))),
      _.encode(),
      SelectionRangeParams.decode
    )

  test("selection ranges round-trip innermost-first chains (empty chain kept)"):
    parity(
      "selection_ranges",
      SelectionRangesResult(
        Seq(Seq(Rng(1, 2, 1, 4), Rng(1, 0, 2, 0), Rng(0, 0, 9, 0)), Seq.empty)
      ),
      _.encode(),
      SelectionRangesResult.decode
    )

  test("an empty selection-ranges list round-trips"):
    parity(
      "selection_ranges_empty",
      SelectionRangesResult(Seq.empty),
      _.encode(),
      SelectionRangesResult.decode
    )

  test("code-action params with both optionals round-trip"):
    parity(
      "code_action_params",
      CodeActionParams(
        "file:///C.scala",
        CodeActionId.ExtractMethod,
        Pos(5, 1),
        Some(Pos(7, 2)),
        Some(Seq(0, 2))
      ),
      _.encode(),
      CodeActionParams.decode
    )

  test("code-action params without optionals round-trip"):
    parity(
      "code_action_params_bare",
      CodeActionParams("file:///C.scala", CodeActionId.InsertInferredType, Pos(5, 1), None, None),
      _.encode(),
      CodeActionParams.decode
    )

  test("code-action edits round-trip"):
    parity(
      "code_action_edits",
      CodeActionResult(Seq(TextEdit(Rng(3, 0, 3, 0), ": Int")), None),
      _.encode(),
      CodeActionResult.decode
    )

  test("a code-action refusal is data, not an error"):
    parity(
      "code_action_refusal",
      CodeActionResult(Seq.empty, Some("Cannot extract selection")),
      _.encode(),
      CodeActionResult.decode
    )

  test("auto-import params round-trip"):
    parity(
      "auto_import_params",
      AutoImportParams("file:///A.scala", Pos(4, 9), "Future", isExtension = false),
      _.encode(),
      AutoImportParams.decode
    )

  test("auto-import candidates round-trip"):
    parity(
      "auto_imports",
      AutoImportsResult(
        Seq(
          AutoImport(
            "scala.concurrent",
            Seq(TextEdit(Rng(0, 0, 0, 0), "import scala.concurrent.Future\n")),
            Some("scala/concurrent/Future#")
          )
        )
      ),
      _.encode(),
      AutoImportsResult.decode
    )

  test("an empty auto-imports list round-trips"):
    parity("auto_imports_empty", AutoImportsResult(Seq.empty), _.encode(), AutoImportsResult.decode)

  test("pc diagnostics round-trip"):
    parity(
      "pc_diagnostics",
      PcDiagnosticsResult(Seq(PcDiagnostic(Rng(3, 0, 3, 5), 1, "E007", "not found: value x"))),
      _.encode(),
      PcDiagnosticsResult.decode
    )

  test("an empty pc-diagnostics list round-trips"):
    parity(
      "pc_diagnostics_empty",
      PcDiagnosticsResult(Seq.empty),
      _.encode(),
      PcDiagnosticsResult.decode
    )

  test("folding ranges round-trip with kind ordinals"):
    parity(
      "folding_ranges",
      FoldingRangesResult(
        Seq(
          FoldingRange(Rng(0, 0, 5, 1), FoldingKind.Imports),
          FoldingRange(Rng(6, 10, 9, 1), FoldingKind.None)
        )
      ),
      _.encode(),
      FoldingRangesResult.decode
    )

  test("an empty folding-ranges list round-trips"):
    parity(
      "folding_ranges_empty",
      FoldingRangesResult(Seq.empty),
      _.encode(),
      FoldingRangesResult.decode
    )

  test("definition-source toplevels round-trip"):
    parity(
      "toplevels",
      ToplevelsResult(Seq("a/b/Main.", "a/b/Main#")),
      _.encode(),
      ToplevelsResult.decode
    )

  test("an empty toplevels list round-trips"):
    parity("toplevels_empty", ToplevelsResult(Seq.empty), _.encode(), ToplevelsResult.decode)

  test("v2 payload kinds cannot be confused"):
    // A buffer of one v2 payload never decodes as another (distinct envelope
    // kinds), matching the Rust kind-confusion pins.
    intercept[CodecException](InlayHintParams.decode(golden("uri_params")))
    intercept[CodecException](SemanticTokensResult.decode(golden("uri_params")))
    intercept[CodecException](LocationsResult.decode(golden("toplevels")))
    intercept[CodecException](MethodHitsResult.decode(golden("toplevels")))
    intercept[CodecException](SelectionRangesResult.decode(golden("semantic_tokens")))
    intercept[CodecException](InlayHintsResult.decode(golden("semantic_tokens")))
    intercept[CodecException](AutoImportsResult.decode(golden("code_action_edits")))
    intercept[CodecException](CodeActionParams.decode(golden("code_action_edits")))

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
