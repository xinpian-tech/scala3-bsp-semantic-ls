package ls.rename

import ls.index.{DocId, SymbolKey}
import ls.rename.ingest.{TargetSpec, WorkspaceTargets}
import ls.semanticdb.DocFacts

class IdentifierSuite extends munit.FunSuite:

  test("plain identifiers pass verbatim"):
    assertEquals(ScalaIdentifiers.encode("foo"), Right("foo"))
    assertEquals(ScalaIdentifiers.encode("Foo42"), Right("Foo42"))
    assertEquals(ScalaIdentifiers.encode("_bar"), Right("_bar"))
    assertEquals(ScalaIdentifiers.encode("++"), Right("++"))
    assertEquals(ScalaIdentifiers.encode("map_+"), Right("map_+"))

  test("keywords and non-identifiers are backtick-quoted"):
    assertEquals(ScalaIdentifiers.encode("type"), Right("`type`"))
    assertEquals(ScalaIdentifiers.encode("match"), Right("`match`"))
    assertEquals(ScalaIdentifiers.encode("my name"), Right("`my name`"))
    assertEquals(ScalaIdentifiers.encode("42abc"), Right("`42abc`"))

  test("impossible names are rejected"):
    assert(ScalaIdentifiers.encode("").isLeft)
    assert(ScalaIdentifiers.encode("a`b").isLeft)
    assert(ScalaIdentifiers.encode("two\nlines").isLeft)

class SymbolEncodingSuite extends munit.FunSuite:

  test("globals round-trip verbatim"):
    val key = SymbolKey.global("pkga/Item#")
    assertEquals(SymbolEncoding.encode(key), "pkga/Item#")
    assertEquals(SymbolEncoding.toKey("pkga/Item#"), key)

  test("locals are doc-qualified and reversible"):
    val key = SymbolKey.local("local3", DocId(41L))
    val enc = SymbolEncoding.encode(key)
    assertEquals(enc, "local3@41")
    assertEquals(SymbolEncoding.decode(enc), ("local3", Some(41L)))
    assertEquals(SymbolEncoding.toKey(enc), key)

  test("global symbols starting with 'local' are not mangled"):
    val sym = "a/localizer/Locale#"
    assertEquals(SymbolEncoding.encode(sym, None), sym)
    assertEquals(SymbolEncoding.decode(sym), (sym, None))

class WorkspaceTargetsSuite extends munit.FunSuite:

  private val ws = WorkspaceTargets(
    Vector(
      TargetSpec("a", java.nio.file.Path.of("/x/a"), java.nio.file.Path.of("/x")),
      TargetSpec(
        "b",
        java.nio.file.Path.of("/x/b"),
        java.nio.file.Path.of("/x"),
        directDeps = Vector("a")
      ),
      TargetSpec(
        "c",
        java.nio.file.Path.of("/x/c"),
        java.nio.file.Path.of("/x"),
        directDeps = Vector("b")
      ),
      TargetSpec("d", java.nio.file.Path.of("/x/d"), java.nio.file.Path.of("/x"))
    )
  )

  test("reverse dependency closure is exact and transitive"):
    assertEquals(ws.reverseDependencyClosure("a"), Set("a", "b", "c"))
    assertEquals(ws.reverseDependencyClosure("b"), Set("b", "c"))
    assertEquals(ws.reverseDependencyClosure("d"), Set("d"))
    assertEquals(ws.reverseDependencyClosure("nope"), Set.empty[String])

  test("docFacts default to plain workspace sources"):
    assertEquals(ws.factsFor("a", "any/uri.scala"), DocFacts.workspaceSource)
    assertEquals(ws.factsFor("unknown", "any/uri.scala"), DocFacts.workspaceSource)

  test("duplicate bsp ids are refused"):
    intercept[IllegalArgumentException] {
      WorkspaceTargets(
        Vector(
          TargetSpec("a", java.nio.file.Path.of("/x"), java.nio.file.Path.of("/x")),
          TargetSpec("a", java.nio.file.Path.of("/y"), java.nio.file.Path.of("/y"))
        )
      )
    }
