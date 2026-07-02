package ls.semanticdb

import ls.index.*

/** Unit tests of the v1 alias-group policy and rename profiles over
  * hand-built normalized documents (symbol strings follow the SemanticDB
  * grammar exactly).
  */
class GroupsSuite extends munit.FunSuite:

  private def gk(sym: String): SymbolKey = SymbolKey.global(sym)

  private def info(
      sym: String,
      kind: SymKind,
      props: Int = 0,
      overridden: List[String] = Nil
  ): SymbolInfo =
    SymbolInfo(
      key = gk(sym),
      displayName = SymbolStrings.displayName(sym).getOrElse(sym),
      ownerName = SymbolStrings.ownerName(sym),
      packageName = SymbolStrings.packageName(sym),
      kind = kind,
      properties = props,
      overriddenSymbols = overridden
    )

  private def linfo(sym: String, docId: Long): SymbolInfo =
    SymbolInfo(
      key = SymbolKey.local(sym, DocId(docId)),
      displayName = sym,
      ownerName = None,
      packageName = None,
      kind = SymKind.LocalValue,
      properties = 0,
      overriddenSymbols = Nil
    )

  private var line = 0
  private def occ(key: SymbolKey, role: Role): Occurrence =
    line += 1
    Occurrence(key, Span(line, 0, line, 5), role)

  private def doc(
      uri: String,
      symbols: Vector[SymbolInfo],
      occurrences: Vector[Occurrence]
  ): NormalizedDocument =
    NormalizedDocument(uri, "MD5", 4, "scala", symbols, occurrences)

  // ---------------------------------------------------------------- fixtures

  private val X = "a/X#"
  private val XObj = "a/X."
  private val XCtor = "a/X#`<init>`()."
  private val XCtor2 = "a/X#`<init>`(+1)."
  private val XApply = "a/X.apply()."
  private val XApply2 = "a/X.apply(+1)."
  private val XUnapply = "a/X.unapply()."

  private def companionBatch: SemanticBatch =
    val defs = doc(
      "a/X.scala",
      Vector(
        info(X, SymKind.Class, SymProps.Case),
        info(XObj, SymKind.Object),
        info(XCtor, SymKind.Constructor, SymProps.Primary),
        info(XCtor2, SymKind.Constructor),
        info(XApply, SymKind.Method),
        info(XApply2, SymKind.Method),
        info(XUnapply, SymKind.Method)
      ),
      Vector(
        occ(gk(X), Role.Definition),
        occ(gk(XObj), Role.Definition),
        occ(gk(XCtor), Role.Definition),
        occ(gk(XCtor2), Role.Definition),
        occ(gk(XApply), Role.Definition),
        occ(gk(XApply2), Role.Definition),
        occ(gk(XUnapply), Role.Definition)
      )
    )
    val use = doc(
      "a/Use.scala",
      Vector.empty,
      Vector(
        occ(gk(X), Role.Reference),
        occ(gk(XCtor2), Role.Reference),
        occ(gk(XApply), Role.Reference)
      )
    )
    SemanticBatch.assemble(Vector(defs, use))

  // ------------------------------------------------------------------- tests

  test("class, companion object, all constructors, apply/unapply share one ref group"):
    val batch = companionBatch
    val expected = batch.refGroupOf(gk(X))
    assert(expected.isDefined)
    for sym <- List(XObj, XCtor, XCtor2, XApply, XApply2, XUnapply) do
      assertEquals(batch.refGroupOf(gk(sym)), expected, clues(sym))
    // v1: rename groups match ref groups
    val expectedRename = batch.renameGroupOf(gk(X))
    for sym <- List(XObj, XCtor, XCtor2, XApply, XApply2, XUnapply) do
      assertEquals(batch.renameGroupOf(gk(sym)), expectedRename, clues(sym))

  test("companion group has hasCompanion and is safe by default"):
    val batch = companionBatch
    val profile = batch.renameProfileOf(gk(X)).get
    assert(profile.hasCompanion)
    assert(!profile.isExternal)
    assert(!profile.isLocal)
    assertEquals(profile.unsafeReasonMask, 0L)
    assertEquals(profile.editableOccurrenceCount, 10)

  test("var getter and setter merge, including the field term when present"):
    val getter = "a/B#value()."
    val setter = "a/B#`value_=`()."
    val fieldTerm = "a/C#count."
    val fieldSetter = "a/C#`count_=`()."
    val batch = SemanticBatch.assemble(
      Vector(
        doc(
          "a/B.scala",
          Vector(
            info(getter, SymKind.Method, SymProps.Var),
            info(setter, SymKind.Method, SymProps.Var),
            info(fieldTerm, SymKind.LocalVariable, SymProps.Var),
            info(fieldSetter, SymKind.Method, SymProps.Var)
          ),
          Vector(
            occ(gk(getter), Role.Definition),
            occ(gk(setter), Role.Definition),
            occ(gk(fieldTerm), Role.Definition),
            occ(gk(fieldSetter), Role.Definition)
          )
        )
      )
    )
    assertEquals(batch.refGroupOf(gk(getter)), batch.refGroupOf(gk(setter)))
    assertEquals(batch.refGroupOf(gk(fieldTerm)), batch.refGroupOf(gk(fieldSetter)))
    assertNotEquals(batch.refGroupOf(gk(getter)), batch.refGroupOf(gk(fieldTerm)))

  test("method overloads stay in separate groups"):
    val f0 = "a/O.f()."
    val f1 = "a/O.f(+1)."
    val batch = SemanticBatch.assemble(
      Vector(
        doc(
          "a/O.scala",
          Vector(info(f0, SymKind.Method), info(f1, SymKind.Method)),
          Vector(occ(gk(f0), Role.Definition), occ(gk(f1), Role.Definition))
        )
      )
    )
    assertNotEquals(batch.refGroupOf(gk(f0)), batch.refGroupOf(gk(f1)))
    assertNotEquals(batch.renameGroupOf(gk(f0)), batch.renameGroupOf(gk(f1)))

  test("plain object without companion class does NOT merge with its apply"):
    val obj = "a/P."
    val applySym = "a/P.apply()."
    val batch = SemanticBatch.assemble(
      Vector(
        doc(
          "a/P.scala",
          Vector(info(obj, SymKind.Object), info(applySym, SymKind.Method)),
          Vector(occ(gk(obj), Role.Definition), occ(gk(applySym), Role.Definition))
        )
      )
    )
    assertNotEquals(batch.refGroupOf(gk(obj)), batch.refGroupOf(gk(applySym)))

  test("constructor-only batch synthesizes the class key into the same group"):
    val ctor = "q/Ext#`<init>`()."
    val batch = SemanticBatch.assemble(
      Vector(doc("q/U.scala", Vector.empty, Vector(occ(gk(ctor), Role.Reference))))
    )
    val classGroup = batch.refGroupOf(gk("q/Ext#"))
    assert(classGroup.isDefined, clues(batch.groups.refGroups))
    assertEquals(batch.refGroupOf(gk(ctor)), classGroup)

  test("identical local names in different documents stay separate"):
    val d1 = doc(
      "a/F1.scala",
      Vector(linfo("local0", 1L)),
      Vector(occ(SymbolKey.local("local0", DocId(1L)), Role.Definition))
    )
    val d2 = doc(
      "a/F2.scala",
      Vector(linfo("local0", 2L)),
      Vector(occ(SymbolKey.local("local0", DocId(2L)), Role.Definition))
    )
    val batch = SemanticBatch.assemble(Vector(d1, d2))
    val g1 = batch.refGroupOf(SymbolKey.local("local0", DocId(1L)))
    val g2 = batch.refGroupOf(SymbolKey.local("local0", DocId(2L)))
    assert(g1.isDefined && g2.isDefined)
    assertNotEquals(g1, g2)
    val p1 = batch.renameProfileOf(SymbolKey.local("local0", DocId(1L))).get
    assert(p1.isLocal)
    assertEquals(p1.unsafeReasonMask, 0L)

  test("override family flags both the overriding and the overridden group"):
    val base = "a/Animal#sound()."
    val impl = "a/Dog#sound()."
    val batch = SemanticBatch.assemble(
      Vector(
        doc(
          "a/Animals.scala",
          Vector(
            info("a/Animal#", SymKind.Trait),
            info("a/Dog#", SymKind.Class),
            info(base, SymKind.Method, SymProps.Abstract),
            info(impl, SymKind.Method, overridden = List(base))
          ),
          Vector(
            occ(gk("a/Animal#"), Role.Definition),
            occ(gk("a/Dog#"), Role.Definition),
            occ(gk(base), Role.Definition),
            occ(gk(impl), Role.Definition)
          )
        )
      )
    )
    assertNotEquals(batch.renameGroupOf(gk(base)), batch.renameGroupOf(gk(impl)))
    val baseProfile = batch.renameProfileOf(gk(base)).get
    val implProfile = batch.renameProfileOf(gk(impl)).get
    assert(baseProfile.hasOverrideFamily)
    assert(implProfile.hasOverrideFamily)
    assertEquals(baseProfile.unsafeReasonMask, UnsafeReason.OverrideFamily)
    assertEquals(implProfile.unsafeReasonMask, UnsafeReason.OverrideFamily)
    // classes themselves are unaffected
    assertEquals(batch.renameProfileOf(gk("a/Animal#")).get.unsafeReasonMask, 0L)

  test("reference-only symbols are external and unsafe"):
    val ext = "scala/Int#"
    val batch = SemanticBatch.assemble(
      Vector(doc("a/U.scala", Vector.empty, Vector(occ(gk(ext), Role.Reference))))
    )
    val profile = batch.renameProfileOf(gk(ext)).get
    assert(profile.isExternal)
    assertEquals(profile.unsafeReasonMask & UnsafeReason.External, UnsafeReason.External)

  test("doc facts drive generated/readonly/dependency bits and editable count"):
    val sym = "a/G."
    val d1 = doc(
      "a/G.scala",
      Vector(info(sym, SymKind.Object)),
      Vector(occ(gk(sym), Role.Definition), occ(gk(sym), Role.Reference))
    )
    val d2 = doc("gen/G.scala", Vector.empty, Vector(occ(gk(sym), Role.Reference)))
    val d3 = doc("ro/G.scala", Vector.empty, Vector(occ(gk(sym), Role.Reference)))
    val d4 = doc("dep/G.scala", Vector.empty, Vector(occ(gk(sym), Role.Reference)))
    val facts = Map(
      "gen/G.scala" -> DocFacts(generated = true, readonly = false, isDependencySource = false),
      "ro/G.scala" -> DocFacts(generated = false, readonly = true, isDependencySource = false),
      "dep/G.scala" -> DocFacts(generated = false, readonly = false, isDependencySource = true)
    )
    val batch = SemanticBatch.assemble(Vector(d1, d2, d3, d4), facts)
    val profile = batch.renameProfileOf(gk(sym)).get
    assert(profile.hasGeneratedOccurrences)
    assert(profile.hasReadonlyOccurrences)
    assertEquals(profile.editableOccurrenceCount, 2) // only the two in a/G.scala
    val expectedMask =
      UnsafeReason.GeneratedOccurrence | UnsafeReason.ReadonlyOccurrence |
        UnsafeReason.DependencySource
    assertEquals(profile.unsafeReasonMask, expectedMask)
    assert(!profile.isExternal)

  test("groups are deterministic across assemble calls"):
    val b1 = companionBatch
    val b2 = companionBatch
    assertEquals(b1.groups.refGroups, b2.groups.refGroups)
    assertEquals(b1.groups.renameGroups, b2.groups.renameGroups)
    assertEquals(b1.renameProfiles, b2.renameProfiles)
