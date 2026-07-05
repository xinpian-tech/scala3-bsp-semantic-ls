package ls.pc.host.codec

import scala.io.Source

import ls.pc.host.codec.Payloads.*

/** Cross-language parity for the flat codec. For each carrier-free payload the
  * suite (a) decodes the Rust-produced golden vector and asserts the fields,
  * (b) re-encodes the equivalent Java instance and asserts the bytes are
  * identical to the golden, and (c) checks that malformed buffers decode to a
  * typed [[CodecException]] rather than crashing. The goldens are produced by
  * the Rust `ls-pc-abi` encoder (see `test/resources/codec-vectors.txt`), so a
  * byte mismatch means the two sides disagree on the wire format.
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
