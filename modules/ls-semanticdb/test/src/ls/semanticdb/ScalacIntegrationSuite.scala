package ls.semanticdb

import java.io.File
import java.nio.charset.StandardCharsets
import java.nio.file.{Files, Path}
import scala.concurrent.duration.Duration

import ls.index.*

/** End-to-end check against the REAL compiler: fixture sources are compiled
  * in-process with `dotty.tools.dotc.Main` and `-Xsemanticdb`, then located,
  * parsed, md5-validated, normalized and grouped.
  */
class ScalacIntegrationSuite extends munit.FunSuite:

  override def munitTimeout: Duration = Duration(300, "s")

  private case class Fixture(
      root: Path,
      out: Path,
      locator: SemanticdbLocator,
      sources: Map[String, String], // uri -> text
      raw: Map[String, SdbDocument], // uri -> parsed document
      docIds: Map[String, DocId],
      batch: SemanticBatch
  )

  private val fixtureSources: Map[String, String] = Map(
    "src/fix/Person.scala" ->
      """package fix
        |
        |case class Person(name: String):
        |  def greet: String = "hi " + name
        |
        |object Person:
        |  def default: Person = new Person("d")
        |""".stripMargin,
    "src/fix/Use.scala" ->
      """package fix
        |
        |object Use:
        |  val a: Person = Person("x")
        |  val b = new Person("y")
        |  val c = Person.apply("z")
        |  def show(p: Person): String = p.greet
        |""".stripMargin,
    "src/fix/Box.scala" ->
      """package fix
        |
        |class Box:
        |  var value: Int = 0
        |
        |object BoxUse:
        |  def bump(b: Box): Unit =
        |    val tmp = b.value + 1
        |    b.value = tmp
        |""".stripMargin,
    "src/fix/Over.scala" ->
      """package fix
        |
        |object Over:
        |  def f(i: Int): Int = i
        |  def f(s: String): String = s
        |  val x = f(1)
        |  val y = f("s")
        |""".stripMargin,
    "src/fix/Animals.scala" ->
      """package fix
        |
        |trait Animal:
        |  def sound: String
        |
        |class Dog extends Animal:
        |  def sound: String = "woof"
        |""".stripMargin,
    "src/fix/Copy.scala" ->
      """package fix
        |
        |case class Pt(x: Int, y: Int)
        |
        |object CopyUse:
        |  val p = Pt(1, 2)
        |  val q = p.copy(x = 3)
        |""".stripMargin,
    "src/fix/Export.scala" ->
      """package fix
        |
        |object Impl:
        |  def work(x: Int): Int = x
        |
        |object Api:
        |  export Impl.work
        |
        |object ExportUse:
        |  val use = Api.work(1)
        |""".stripMargin
  )

  private lazy val fixture: Fixture = compileFixtures()

  private def compileFixtures(): Fixture =
    val root = Files.createTempDirectory(
      Path.of(System.getProperty("user.dir")),
      "sdb-fixture-"
    )
    val out = root.resolve("out")
    Files.createDirectories(out)
    val files = fixtureSources.toVector.sortBy(_._1).map { (rel, text) =>
      val p = root.resolve(rel)
      Files.createDirectories(p.getParent)
      Files.write(p, text.getBytes(StandardCharsets.UTF_8))
      p.toString
    }

    // scala3-library + scala-library jars from the test JVM's classpath.
    val libraryJars = System
      .getProperty("java.class.path")
      .split(File.pathSeparator)
      .filter { p =>
        val name = Path.of(p).getFileName.toString
        name.startsWith("scala3-library") || name.startsWith("scala-library")
      }
    assert(libraryJars.nonEmpty, "scala library jars not found on java.class.path")

    val args = Array(
      "-Xsemanticdb",
      "-sourceroot",
      root.toString,
      "-d",
      out.toString,
      "-classpath",
      libraryJars.mkString(File.pathSeparator)
    ) ++ files
    val reporter = dotty.tools.dotc.Main.process(args)
    assert(!reporter.hasErrors, s"scalac failed:\n${reporter.allErrors.mkString("\n")}")

    val locator = SemanticdbLocator(out)
    val sdbFiles = locator.listSemanticdbFiles()
    val rawDocs =
      for
        f <- sdbFiles
        d <- SemanticdbParser.parseFile(f).documents
      yield d
    val raw = rawDocs.map(d => d.uri -> d).toMap
    val docIds = raw.keys.toVector.sorted.zipWithIndex.map((uri, i) => uri -> DocId(i + 1L)).toMap
    val normalized = rawDocs
      .sortBy(_.uri)
      .map(d => Normalizer.normalize(d, docIds(d.uri)))
    Fixture(
      root = root,
      out = out,
      locator = locator,
      sources = fixtureSources,
      raw = raw,
      docIds = docIds,
      batch = SemanticBatch.assemble(normalized)
    )

  private def allGlobalSymbols: Vector[String] =
    fixture.batch.groups.refGroupIndex.keysIterator
      .filter(!_.isLocal)
      .map(_.semanticSymbol)
      .toVector
      .sorted

  private def normalizedDoc(uri: String): NormalizedDocument =
    fixture.batch.documents.find(_.uri == uri).getOrElse(fail(s"no normalized doc for $uri"))

  private def refGroup(sym: String): Int =
    fixture.batch
      .refGroupOf(SymbolKey.global(sym))
      .getOrElse(fail(s"no ref group for $sym", clues(allGlobalSymbols)))

  // ------------------------------------------------------------------- tests

  test("locator finds one .semanticdb file per fixture source"):
    val files = fixture.locator.listSemanticdbFiles()
    assertEquals(files.size, fixtureSources.size, clues(files))
    val rels = files.flatMap(fixture.locator.sourceRelativePathFor)
    assertEquals(rels.toSet, fixtureSources.keySet)

  test("TextDocument uris match source-relative paths and schema is SemanticDB4"):
    assertEquals(fixture.raw.keySet, fixtureSources.keySet)
    for (uri, doc) <- fixture.raw do
      assertEquals(doc.schema, 4, clues(uri))
      assertEquals(doc.languageCode, SdbLanguage.Scala, clues(uri))

  test("md5 validation is Fresh for every compiled source"):
    for (uri, doc) <- fixture.raw do
      assertEquals(Md5.validate(fixture.sources(uri), doc), FreshnessCheck.Fresh, clues(uri))

  test("md5 validation catches edits"):
    val doc = fixture.raw("src/fix/Person.scala")
    val edited = fixture.sources("src/fix/Person.scala") + "\n// edited\n"
    assert(!Md5.validate(edited, doc).isFresh)

  test("known symbols with kinds and properties are present"):
    val person = normalizedDoc("src/fix/Person.scala")
    val byName = person.symbols.map(s => s.key.semanticSymbol -> s).toMap
    val cls = byName.getOrElse("fix/Person#", fail("missing fix/Person#", clues(byName.keys.toVector)))
    assertEquals(cls.kind, SymKind.Class)
    assert((cls.properties & SymProps.Case) != 0, "case class property bit")
    assertEquals(cls.displayName, "Person")
    assertEquals(cls.packageName, Some("fix"))
    val obj = byName.getOrElse("fix/Person.", fail("missing fix/Person.", clues(byName.keys.toVector)))
    assertEquals(obj.kind, SymKind.Object)

  test("occurrence roles: definition at the definition site, references at use sites"):
    val person = normalizedDoc("src/fix/Person.scala")
    val defs = person.occurrences.filter(o =>
      o.key == SymbolKey.global("fix/Person#") && o.role == Role.Definition
    )
    assert(defs.nonEmpty, clues(person.occurrences.map(o => (o.key.semanticSymbol, o.role))))
    // `case class Person` is on line 2 (0-based), token "Person" at chars 11-17
    assert(defs.exists(o => o.span.startLine == 2), clues(defs))

    val use = normalizedDoc("src/fix/Use.scala")
    val personGroup = refGroup("fix/Person#")
    val refOccs = use.occurrences.filter(o =>
      !o.key.isLocal &&
        fixture.batch.refGroupOf(o.key).contains(personGroup) &&
        o.role == Role.Reference
    )
    // Person appears on at least 4 lines in Use.scala
    assert(refOccs.map(_.span.startLine).distinct.size >= 4, clues(use.occurrences))

  test("class, companion object, constructor and apply land in one ref group"):
    val g = refGroup("fix/Person#")
    assertEquals(refGroup("fix/Person."), g, clues(allGlobalSymbols))
    val ctors = allGlobalSymbols.filter(s =>
      s.startsWith("fix/Person#") && SymbolStrings.isConstructor(s)
    )
    assert(ctors.nonEmpty, clues(allGlobalSymbols))
    for c <- ctors do assertEquals(refGroup(c), g, clues(c))
    // methods only: parameter symbols like fix/Person.apply().(name) end with ")"
    val applies = allGlobalSymbols.filter(s => s.startsWith("fix/Person.apply(") && s.endsWith("."))
    assert(applies.nonEmpty, clues(allGlobalSymbols))
    for a <- applies do assertEquals(refGroup(a), g, clues(a))

  test("case-class fixture yields hasCompanion on its rename profile"):
    val profile = fixture.batch.renameProfileOf(SymbolKey.global("fix/Person#")).get
    assert(profile.hasCompanion)
    assert(!profile.isExternal)
    assert(profile.editableOccurrenceCount > 0)

  test("var getter and setter are merged"):
    val setters = allGlobalSymbols.filter(SymbolStrings.isSetter)
    val valueSetter = setters.find(_.contains("value_=")).getOrElse(
      fail("no value_= setter in batch", clues(allGlobalSymbols))
    )
    val getterCandidates = Vector("fix/Box#value().", "fix/Box#value.")
    val getter = getterCandidates.find(s => fixture.batch.refGroupOf(SymbolKey.global(s)).isDefined)
      .getOrElse(fail("no getter symbol found", clues(allGlobalSymbols)))
    assertEquals(refGroup(valueSetter), refGroup(getter), clues(valueSetter, getter))
    // v1: rename groups agree
    assertEquals(
      fixture.batch.renameGroupOf(SymbolKey.global(valueSetter)),
      fixture.batch.renameGroupOf(SymbolKey.global(getter))
    )

  test("method overloads stay in separate groups"):
    val overloads = allGlobalSymbols.filter(s => s.startsWith("fix/Over.f(") && s.endsWith("."))
    assertEquals(overloads.size, 2, clues(allGlobalSymbols))
    assertNotEquals(refGroup(overloads(0)), refGroup(overloads(1)))

  test("override family is flagged unsafe on both sides"):
    val dogSound = allGlobalSymbols.find(_.startsWith("fix/Dog#sound")).getOrElse(
      fail("no Dog#sound", clues(allGlobalSymbols))
    )
    val animalSound = allGlobalSymbols.find(_.startsWith("fix/Animal#sound")).getOrElse(
      fail("no Animal#sound", clues(allGlobalSymbols))
    )
    val dogProfile = fixture.batch.renameProfileOf(SymbolKey.global(dogSound)).get
    val animalProfile = fixture.batch.renameProfileOf(SymbolKey.global(animalSound)).get
    assert(dogProfile.hasOverrideFamily, clues(dogSound))
    assert(animalProfile.hasOverrideFamily, clues(animalSound))
    assert((dogProfile.unsafeReasonMask & UnsafeReason.OverrideFamily) != 0L)
    assert((animalProfile.unsafeReasonMask & UnsafeReason.OverrideFamily) != 0L)
    assert(!dogProfile.isSafe)

  test("references to library symbols are external"):
    // scala/Int# is referenced (Box.value: Int) but never defined here
    val profile = fixture.batch.renameProfileOf(SymbolKey.global("scala/Int#"))
    assert(profile.isDefined, clues(allGlobalSymbols))
    assert(profile.get.isExternal)
    assert((profile.get.unsafeReasonMask & UnsafeReason.External) != 0L)

  test("local symbols carry the caller-supplied DocId and stay isolated"):
    val boxDocId = fixture.docIds("src/fix/Box.scala")
    val box = normalizedDoc("src/fix/Box.scala")
    val locals = box.occurrences.filter(_.key.isLocal).map(_.key).distinct
    assert(locals.nonEmpty, clues(box.occurrences.map(_.key.semanticSymbol)))
    for l <- locals do assertEquals(l.localDoc, Some(boxDocId))
    val profile = fixture.batch.renameProfileOf(locals.head).get
    assert(profile.isLocal)

  test("marking a document generated poisons groups touching it"):
    val facts = Map(
      "src/fix/Use.scala" -> DocFacts(generated = true, readonly = false, isDependencySource = false)
    )
    val poisoned = SemanticBatch.assemble(fixture.batch.documents, facts)
    val before = fixture.batch.renameProfileOf(SymbolKey.global("fix/Person#")).get
    val after = poisoned.renameProfileOf(SymbolKey.global("fix/Person#")).get
    assert(!before.hasGeneratedOccurrences)
    assert(after.hasGeneratedOccurrences)
    assert((after.unsafeReasonMask & UnsafeReason.GeneratedOccurrence) != 0L)
    assert(after.editableOccurrenceCount < before.editableOccurrenceCount)

  test("export SemanticDB shape: forwarder symbol exists with no definition occurrence"):
    // characterization input for the export rule (real scalac 3 output):
    val doc = normalizedDoc("src/fix/Export.scala")
    val bySym = doc.symbols.map(s => s.key.semanticSymbol -> s).toMap
    // the `export Impl.work` forwarder is a distinct Method symbol under Api,
    // carrying NO overriddenSymbols
    val fwd = bySym.getOrElse("fix/Api.work().", fail("no forwarder symbol", clues(bySym.keys.toVector)))
    assertEquals(fwd.kind, SymKind.Method)
    assertEquals(fwd.displayName, "work")
    assertEquals(fwd.overriddenSymbols, Nil)
    // the original method exists too
    assert(bySym.contains("fix/Impl.work()."), clues(bySym.keys.toVector))
    // the forwarder has NO definition occurrence (synthesized), while the
    // original does — this is the signal the rule keys on
    def defOccsOf(sym: String) =
      doc.occurrences.filter(o => o.key == SymbolKey.global(sym) && o.role == Role.Definition)
    assert(defOccsOf("fix/Api.work().").isEmpty, "forwarder must have no definition occurrence")
    assert(defOccsOf("fix/Impl.work().").nonEmpty, "original must have a definition occurrence")
    // the export clause references the exported-from object (Impl), and the
    // `Api.work(1)` call site references the forwarder symbol
    assert(
      doc.occurrences.exists(o => o.key == SymbolKey.global("fix/Impl.") && o.role == Role.Reference),
      "export clause should reference Impl"
    )
    assert(
      doc.occurrences.exists(o => o.key == SymbolKey.global("fix/Api.work().") && o.role == Role.Reference),
      "Api.work(1) should reference the forwarder"
    )

  test("export forwarder call sites join the original's ref group"):
    assertEquals(refGroup("fix/Api.work()."), refGroup("fix/Impl.work()."), clues(allGlobalSymbols))

  test("export forwarder marks the rename group UnsupportedSymbolFamily"):
    val profile = fixture.batch.renameProfileOf(SymbolKey.global("fix/Impl.work().")).get
    assert(
      (profile.unsafeReasonMask & UnsafeReason.UnsupportedSymbolFamily) != 0L,
      s"expected UnsupportedSymbolFamily, mask=0x${profile.unsafeReasonMask.toHexString}"
    )
    assert(!profile.isSafe)

  test("synthetic-only case-class copy has no definition occurrence but a defined owner"):
    // characterization input (real scalac 3): the synthesized `copy` carries a
    // Method symbol and a reference at the call site, but NO definition
    // occurrence of its own (scalac emits `def copy` only in the skipped
    // synthetics payload); its owner (the case class) IS defined here
    val doc = normalizedDoc("src/fix/Copy.scala")
    val copyKey = SymbolKey.global("fix/Pt#copy().")
    assert(doc.symbols.exists(_.key == copyKey), clues(doc.symbols.map(_.key.semanticSymbol)))
    assertEquals(doc.occurrences.count(o => o.key == copyKey && o.role == Role.Definition), 0)
    assert(doc.occurrences.exists(o => o.key == copyKey && o.role == Role.Reference), "copy call site")
    assert(
      doc.occurrences.exists(o => o.key == SymbolKey.global("fix/Pt#") && o.role == Role.Definition),
      "owner Pt# must be defined in the workspace"
    )

  test("synthetic-only symbol is flagged UnsafeReason.SyntheticOnly at ingest (not External)"):
    val profile = fixture.batch.renameProfileOf(SymbolKey.global("fix/Pt#copy().")).get
    assert(
      (profile.unsafeReasonMask & UnsafeReason.SyntheticOnly) != 0L,
      s"expected SyntheticOnly, mask=0x${profile.unsafeReasonMask.toHexString}"
    )
    assert(!profile.isExternal, "a synthesized member of a workspace type is not external")
    assert(!profile.isSafe)

  test("a symbol with an editable definition is NOT flagged synthetic-only"):
    // the case-class field accessor has a real definition occurrence
    val xProfile = fixture.batch.renameProfileOf(SymbolKey.global("fix/Pt#x.")).get
    assertEquals(xProfile.unsafeReasonMask & UnsafeReason.SyntheticOnly, 0L, clues(xProfile))
    // a truly external library symbol stays External, never synthetic-only
    val intProfile = fixture.batch.renameProfileOf(SymbolKey.global("scala/Int#")).get
    assertEquals(intProfile.unsafeReasonMask & UnsafeReason.SyntheticOnly, 0L)
    assert(intProfile.isExternal)

  test("normalization is deterministic"):
    val docs2 = fixture.raw.values.toVector
      .sortBy(_.uri)
      .map(d => Normalizer.normalize(d, fixture.docIds(d.uri)))
    val batch2 = SemanticBatch.assemble(docs2)
    assertEquals(batch2.documents, fixture.batch.documents)
    assertEquals(batch2.groups, fixture.batch.groups)
    assertEquals(batch2.renameProfiles, fixture.batch.renameProfiles)
