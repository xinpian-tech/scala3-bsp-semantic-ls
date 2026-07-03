package ls.sqlite

class FuzzyRankSuite extends munit.FunSuite:

  test("normalize lowercases and keeps only letters/digits"):
    assertEquals(FuzzyRank.normalize("workspaceSymbol"), "workspacesymbol")
    assertEquals(FuzzyRank.normalize("Foo_Bar-42.Baz"), "foobar42baz")
    assertEquals(FuzzyRank.normalize("(*)"), "")
    assertEquals(FuzzyRank.normalize("数据库"), "数据库")

  test("initials picks camel-hump, separator, and digit boundaries"):
    assertEquals(FuzzyRank.initials("workspaceSymbol"), "ws")
    assertEquals(FuzzyRank.initials("FooBarBaz"), "fbb")
    assertEquals(FuzzyRank.initials("snake_case_name"), "scn")
    assertEquals(FuzzyRank.initials("user2Name"), "u2n")

  test("score ranks exact above prefix above subsequence"):
    val exact = FuzzyRank.score("Core", "Core").get
    val prefix = FuzzyRank.score("Cor", "CoreThing").get
    val subseq = FuzzyRank.score("ce", "CoreEngine").get
    assert(exact > prefix, s"$exact !> $prefix")
    assert(prefix > subseq, s"$prefix !> $subseq")

  test("score: a camel-hump-aligned subsequence beats a plain one"):
    // "wSy" lands on the w+S humps of workspaceSymbol (2 hump hits); against
    // "whimsy" only the leading 'w' is a hump (1).
    val strong = FuzzyRank.score("wSy", "workspaceSymbol").get
    val weak = FuzzyRank.score("wSy", "whimsy").get
    assert(strong > weak, s"$strong !> $weak")

  test("score: a shorter name wins the tie at the same tier"):
    val short = FuzzyRank.score("ab", "aXb").get
    val long = FuzzyRank.score("ab", "aXXXXXb").get
    assert(short > long, s"$short !> $long")

  test("score: non-subsequence and empty query are not matches"):
    assertEquals(FuzzyRank.score("xyz", "workspaceSymbol"), None)
    assertEquals(FuzzyRank.score("", "anything"), None)
    assertEquals(FuzzyRank.score("abc", "(*)"), None)
