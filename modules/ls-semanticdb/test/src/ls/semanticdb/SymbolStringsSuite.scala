package ls.semanticdb

import ls.semanticdb.SymbolStrings.Descriptor

class SymbolStringsSuite extends munit.FunSuite:

  test("isLocal / isGlobal"):
    assert(SymbolStrings.isLocal("local0"))
    assert(SymbolStrings.isLocal("local12345"))
    assert(!SymbolStrings.isLocal("locale/")) // package named locale
    assert(!SymbolStrings.isLocal("localizer.")) // term starting with local
    assert(!SymbolStrings.isLocal("a/b/C#"))
    assert(!SymbolStrings.isLocal(""))
    assert(SymbolStrings.isGlobal("a/b/C#"))
    assert(SymbolStrings.isGlobal("_root_/"))
    assert(!SymbolStrings.isGlobal("local1"))
    assert(!SymbolStrings.isGlobal(""))

  test("splitLast on terms, types, packages"):
    assertEquals(
      SymbolStrings.splitLast("scala/Predef."),
      Some(("scala/", Descriptor.Term("Predef")))
    )
    assertEquals(SymbolStrings.splitLast("a/b/C#"), Some(("a/b/", Descriptor.Type("C"))))
    assertEquals(SymbolStrings.splitLast("a/b/"), Some(("a/", Descriptor.Package("b"))))
    assertEquals(SymbolStrings.splitLast("_root_/"), Some(("", Descriptor.Package("_root_"))))

  test("splitLast on methods with disambiguators"):
    assertEquals(
      SymbolStrings.splitLast("a/b/C#f()."),
      Some(("a/b/C#", Descriptor.Method("f", "()")))
    )
    assertEquals(
      SymbolStrings.splitLast("a/b/C#f(+2)."),
      Some(("a/b/C#", Descriptor.Method("f", "(+2)")))
    )
    assertEquals(
      SymbolStrings.splitLast("a/b/C#`<init>`()."),
      Some(("a/b/C#", Descriptor.Method("<init>", "()")))
    )
    assertEquals(
      SymbolStrings.splitLast("a/A.`+`(+1)."),
      Some(("a/A.", Descriptor.Method("+", "(+1)")))
    )

  test("splitLast on parameters and type parameters"):
    assertEquals(
      SymbolStrings.splitLast("a/b/C#m().(x)"),
      Some(("a/b/C#m().", Descriptor.Parameter("x")))
    )
    assertEquals(
      SymbolStrings.splitLast("a/b/C#[T]"),
      Some(("a/b/C#", Descriptor.TypeParameter("T")))
    )

  test("splitLast on backticked names containing descriptor characters"):
    assertEquals(SymbolStrings.splitLast("a/`x y`."), Some(("a/", Descriptor.Term("x y"))))
    assertEquals(
      SymbolStrings.splitLast("a/B#`x_=`()."),
      Some(("a/B#", Descriptor.Method("x_=", "()")))
    )
    assertEquals(
      SymbolStrings.splitLast("a/`weird(name)`."),
      Some(("a/", Descriptor.Term("weird(name)")))
    )

  test("splitLast rejects locals, empty and malformed symbols"):
    assertEquals(SymbolStrings.splitLast("local3"), None)
    assertEquals(SymbolStrings.splitLast(""), None)
    assertEquals(SymbolStrings.splitLast("#"), None)
    assertEquals(SymbolStrings.splitLast("no-terminator"), None)

  test("displayName"):
    assertEquals(SymbolStrings.displayName("a/b/C#"), Some("C"))
    assertEquals(SymbolStrings.displayName("a/b/C#`<init>`()."), Some("<init>"))
    assertEquals(SymbolStrings.displayName("_root_/"), Some("_root_"))
    assertEquals(SymbolStrings.displayName("local9"), None)

  test("ownerChain nested objects and methods"):
    assertEquals(
      SymbolStrings.ownerChain("a/b/C#m()."),
      List("a/", "a/b/", "a/b/C#", "a/b/C#m().")
    )
    assertEquals(
      SymbolStrings.ownerChain("a/Outer.Inner.f()."),
      List("a/", "a/Outer.", "a/Outer.Inner.", "a/Outer.Inner.f().")
    )
    assertEquals(SymbolStrings.ownerChain("local2"), List("local2"))

  test("owner"):
    assertEquals(SymbolStrings.owner("a/b/C#"), Some("a/b/"))
    assertEquals(SymbolStrings.owner("a/"), None)
    assertEquals(SymbolStrings.owner("local1"), None)

  test("packageName"):
    assertEquals(SymbolStrings.packageName("a/b/C#D#m()."), Some("a.b"))
    assertEquals(SymbolStrings.packageName("_empty_/X#"), None)
    assertEquals(SymbolStrings.packageName("scala/Predef.println()."), Some("scala"))
    assertEquals(SymbolStrings.packageName("local1"), None)
    assertEquals(SymbolStrings.packageName("a/"), None)

  test("ownerName is the nearest enclosing non-package declaration"):
    assertEquals(SymbolStrings.ownerName("a/b/C#D#"), Some("C"))
    assertEquals(SymbolStrings.ownerName("a/b/C#"), None)
    assertEquals(SymbolStrings.ownerName("a/b/C#m()."), Some("C"))
    assertEquals(SymbolStrings.ownerName("a/b/C#m().(x)"), Some("m"))
    assertEquals(SymbolStrings.ownerName("local4"), None)

  test("companion pair detection"):
    assertEquals(SymbolStrings.companion("a/b/C#"), Some("a/b/C."))
    assertEquals(SymbolStrings.companion("a/b/C."), Some("a/b/C#"))
    assertEquals(SymbolStrings.companion("a/b/C#m()."), None)
    assertEquals(SymbolStrings.companion("a/b/"), None)
    assertEquals(SymbolStrings.companion("a/`x y`#"), Some("a/`x y`."))
    assert(SymbolStrings.isCompanionPair("a/b/C#", "a/b/C."))
    assert(SymbolStrings.isCompanionPair("a/b/C.", "a/b/C#"))
    assert(!SymbolStrings.isCompanionPair("a/b/C#", "a/b/D."))

  test("constructor detection"):
    assert(SymbolStrings.isConstructor("a/B#`<init>`()."))
    assert(SymbolStrings.isConstructor("a/B#`<init>`(+1)."))
    assert(!SymbolStrings.isConstructor("a/B#init()."))
    assert(!SymbolStrings.isConstructor("a/B#"))

  test("setter detection"):
    assert(SymbolStrings.isSetter("a/B#`value_=`()."))
    assertEquals(SymbolStrings.setterTargetName("a/B#`value_=`()."), Some("value"))
    assert(!SymbolStrings.isSetter("a/B#value()."))
    assert(!SymbolStrings.isSetter("a/B#`_=`().")) // no base name
    assertEquals(SymbolStrings.setterTargetName("a/B#value()."), None)

  test("enclosingTopLevel"):
    assertEquals(SymbolStrings.enclosingTopLevel("a/b/C#D#m()."), Some("a/b/C#"))
    assertEquals(SymbolStrings.enclosingTopLevel("a/b/C#"), Some("a/b/C#"))
    assertEquals(SymbolStrings.enclosingTopLevel("a/b/Obj.f()."), Some("a/b/Obj."))
    assertEquals(SymbolStrings.enclosingTopLevel("a/b/"), None)
    assertEquals(SymbolStrings.enclosingTopLevel("local7"), None)

  test("encodeName"):
    assertEquals(SymbolStrings.encodeName("plain"), "plain")
    assertEquals(SymbolStrings.encodeName("x_="), "`x_=`")
    assertEquals(SymbolStrings.encodeName("<init>"), "`<init>`")
    assertEquals(SymbolStrings.encodeName("x y"), "`x y`")
    assertEquals(SymbolStrings.encodeName(""), "``")

  test("isPackage"):
    assert(SymbolStrings.isPackage("a/b/"))
    assert(!SymbolStrings.isPackage("a/b/C#"))
    assert(!SymbolStrings.isPackage(""))
