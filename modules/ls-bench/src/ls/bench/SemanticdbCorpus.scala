package ls.bench

import java.io.ByteArrayOutputStream
import java.nio.charset.StandardCharsets
import java.nio.file.{Files, Path}

import ls.index.Span
import ls.semanticdb.{Md5, SdbDocument, SdbLanguage, SdbOccurrence, SdbRange, SdbRole, SdbSymbolInfo}

/** A synthetic SemanticDB corpus written to disk so the production
  * [[ls.rename.ingest.IngestPipeline]] can parse it end-to-end (parse -> SQLite
  * -> segment -> publish). No scalac involved: a minimal protobuf encoder emits
  * `.semanticdb` files with matching md5-consistent `.scala` sources, and the
  * exact ground truth is retained for the benchmark consistency checks.
  *
  * Structure: `symbols` class symbols `pkg/SymN#`, each defined once in its
  * owning document and referenced a controlled number of times spread across
  * documents. Symbol 0 is the designated HOT symbol (>= `hotRefs`); symbols
  * `1..rareCount` are RARE (each `rareRefs`); the rest get a small fixed count.
  */
final case class SdbCorpusParams(
    docs: Int,
    symbols: Int,
    hotRefs: Int,
    rareCount: Int,
    rareRefs: Int,
    fillerRefs: Int
)

/** One rename target's exact ground truth: the cursor (uri, 0-based line, char
  * one inside the def token) and the expected edit plan (occurrence count and
  * per-uri span sets).
  */
final case class RenameTruth(
    displayName: String,
    cursorUri: String,
    cursorLine: Int,
    cursorChar: Int,
    occurrenceCount: Int,
    editsByUri: Map[String, Set[Span]]
)

/** Written corpus plus ground truth. */
final class SdbCorpusTruth(
    val root: Path,
    val semanticdbRoot: Path,
    val sourceroot: Path,
    val params: SdbCorpusParams,
    val docUris: Vector[String],
    /** doc index -> multiset of (packedStart, packedEnd, flags) occurrences. */
    val docOccKeys: Vector[Vector[(Int, Int, Int)]],
    val hot: RenameTruth,
    val rare: RenameTruth,
    /** semanticSymbol of a known group with its total occurrence count. */
    val probeSymbol: String,
    val probeSymbolOccurrences: Int
):
  def totalDocOccurrences: Int = docOccKeys.map(_.length).sum

object SemanticdbCorpus:

  // --- minimal protobuf encoder (scalameta semanticdb.proto field numbers) ---

  private final class W:
    private val out = new ByteArrayOutputStream()
    def bytes: Array[Byte] = out.toByteArray
    def rawVarint(v0: Long): Unit =
      var v = v0
      var go = true
      while go do
        val b = (v & 0x7f).toInt
        v = v >>> 7
        if v == 0L then { out.write(b); go = false }
        else out.write(b | 0x80)
    def tag(field: Int, wire: Int): Unit = rawVarint((field.toLong << 3) | wire.toLong)
    def varint(field: Int, value: Long): Unit = { tag(field, 0); rawVarint(value) }
    def int32(field: Int, value: Int): Unit = varint(field, value.toLong)
    def bytesF(field: Int, data: Array[Byte]): Unit =
      tag(field, 2); rawVarint(data.length.toLong); out.write(data, 0, data.length)
    def string(field: Int, value: String): Unit = bytesF(field, value.getBytes(StandardCharsets.UTF_8))
    def message(field: Int)(build: W => Unit): Unit =
      val n = new W; build(n); bytesF(field, n.bytes)

  private def writeRange(w: W, r: SdbRange): Unit =
    w.int32(1, r.startLine); w.int32(2, r.startCharacter)
    w.int32(3, r.endLine); w.int32(4, r.endCharacter)

  private def writeOcc(w: W, o: SdbOccurrence): Unit =
    o.range.foreach(r => w.message(1)(writeRange(_, r)))
    w.string(2, o.symbol)
    w.varint(3, o.roleCode.toLong)

  private def writeSym(w: W, s: SdbSymbolInfo): Unit =
    w.string(1, s.symbol)
    w.varint(3, s.kindCode.toLong)
    w.int32(4, s.properties)
    w.string(5, s.displayName)
    s.overriddenSymbols.foreach(o => w.string(19, o))

  private def writeDoc(w: W, d: SdbDocument): Unit =
    w.varint(1, d.schema.toLong)
    w.string(2, d.uri)
    w.string(3, d.text)
    w.string(11, d.md5)
    w.varint(10, d.languageCode.toLong)
    d.symbols.foreach(s => w.message(5)(writeSym(_, s)))
    d.occurrences.foreach(o => w.message(6)(writeOcc(_, o)))

  /** TextDocuments: repeated TextDocument documents = 1. */
  def encode(docs: Seq[SdbDocument]): Array[Byte] =
    val w = new W
    docs.foreach(d => w.message(1)(writeDoc(_, d)))
    w.bytes

  private val ClassKind = ls.index.SymKind.Class.code

  /** Generates the corpus under `root`, writing sources under `root/src` and
    * `.semanticdb` files under `root/out/META-INF/semanticdb/`.
    */
  def generate(params: SdbCorpusParams, root: Path): SdbCorpusTruth =
    require(params.docs > 0 && params.symbols > 0, "corpus dimensions must be positive")
    val n = params.docs
    val s = params.symbols
    val sourceroot = root
    val semanticdbRoot = root.resolve("out")
    val sdbDir = semanticdbRoot.resolve("META-INF").resolve("semanticdb")

    val docUris = Vector.tabulate(n)(d => s"src/Doc$d.scala")
    val display = (i: Int) => s"Sym$i"
    val symbolOf = (i: Int) => s"pkg/Sym$i#"
    val ownerDoc = (i: Int) => i % n

    def refCountOf(i: Int): Int =
      if i == 0 then params.hotRefs
      else if i <= params.rareCount then params.rareRefs
      else params.fillerRefs

    // Per-doc accumulators of (SdbSymbolInfo defined here, occurrences here).
    val docSymbols = Array.fill(n)(Vector.newBuilder[SdbSymbolInfo])
    val docOccs = Array.fill(n)(Vector.newBuilder[SdbOccurrence])
    val docOccKeys = Array.fill(n)(Vector.newBuilder[(Int, Int, Int)])
    // Per-doc, per-line display name: the source line for line l is `    <name>`
    // so the token at chars [4, 4+len) equals the symbol's display name (the
    // ingest only makes an occurrence renameable when the source token matches).
    val docLineNames = Array.fill(n)(Vector.newBuilder[String])
    val nextLine = Array.fill(n)(0)
    // hot/rare rename ground truth
    val hotEdits = scala.collection.mutable.HashMap.empty[String, scala.collection.mutable.Set[Span]]
    val rareEdits = scala.collection.mutable.HashMap.empty[String, scala.collection.mutable.Set[Span]]
    var hotCursor: (String, Int, Int) = null
    var rareCursor: (String, Int, Int) = null

    import ls.index.OccFlags

    def place(doc: Int, sym: String, name: String, definition: Boolean, target: Int): Unit =
      val line = nextLine(doc)
      nextLine(doc) += 1
      docLineNames(doc) += name
      val span = Span(line, 4, line, 4 + name.length)
      val role = if definition then SdbRole.Definition else SdbRole.Reference
      docOccs(doc) += SdbOccurrence(Some(SdbRange(line, 4, line, 4 + name.length)), sym, role)
      var flags = OccFlags.Editable
      if definition then flags |= OccFlags.Definition
      docOccKeys(doc) += ((Span.pack(line, 4), Span.pack(line, 4 + name.length), flags))
      // record rename ground truth for the hot (0) and first rare (1) symbols
      if target == 0 then
        hotEdits.getOrElseUpdate(docUris(doc), scala.collection.mutable.Set.empty) += span
        if definition then hotCursor = (docUris(doc), line, 5)
      else if target == 1 && params.rareCount >= 1 then
        rareEdits.getOrElseUpdate(docUris(doc), scala.collection.mutable.Set.empty) += span
        if definition then rareCursor = (docUris(doc), line, 5)

    // definitions first (so def line is deterministic), then references
    for i <- 0 until s do
      docSymbols(ownerDoc(i)) += SdbSymbolInfo(symbolOf(i), ClassKind, 0, display(i), Vector.empty)
      place(ownerDoc(i), symbolOf(i), display(i), definition = true, target = i)
    for i <- 0 until s do
      val rc = refCountOf(i)
      var k = 0
      while k < rc do
        val doc = math.floorMod(i + k * 7 + 1, n)
        place(doc, symbolOf(i), display(i), definition = false, target = i)
        k += 1

    // write source + .semanticdb files
    Files.createDirectories(sourceroot.resolve("src"))
    for d <- 0 until n do
      val names = docLineNames(d).result()
      // one source line per occurrence, `    <displayName>`, so every occurrence
      // span's token equals its symbol's display name (required for renameability).
      val text =
        (if names.isEmpty then Vector("// empty") else names.map(nm => "    " + nm)).mkString("\n") + "\n"
      val sourcePath = sourceroot.resolve(docUris(d))
      Files.createDirectories(sourcePath.getParent)
      Files.write(sourcePath, text.getBytes(StandardCharsets.UTF_8))
      val doc = SdbDocument(
        schema = 4,
        uri = docUris(d),
        text = "",
        md5 = Md5.computeHex(text),
        languageCode = SdbLanguage.Scala,
        symbols = docSymbols(d).result(),
        occurrences = docOccs(d).result()
      )
      val sdbFile = sdbDir.resolve(docUris(d) + ".semanticdb")
      Files.createDirectories(sdbFile.getParent)
      Files.write(sdbFile, encode(Vector(doc)))

    def truthOf(
        target: Int,
        cursor: (String, Int, Int),
        edits: scala.collection.mutable.HashMap[String, scala.collection.mutable.Set[Span]]
    ): RenameTruth =
      RenameTruth(
        displayName = display(target),
        cursorUri = cursor._1,
        cursorLine = cursor._2,
        cursorChar = cursor._3,
        occurrenceCount = refCountOf(target) + 1,
        editsByUri = edits.map((u, set) => u -> set.toSet).toMap
      )

    new SdbCorpusTruth(
      root = root,
      semanticdbRoot = semanticdbRoot,
      sourceroot = sourceroot,
      params = params,
      docUris = docUris,
      docOccKeys = docOccKeys.map(_.result()).toVector,
      hot = truthOf(0, hotCursor, hotEdits),
      rare = truthOf(1, rareCursor, rareEdits),
      probeSymbol = symbolOf(0),
      probeSymbolOccurrences = params.hotRefs + 1
    )
