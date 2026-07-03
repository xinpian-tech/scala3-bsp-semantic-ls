package ls.sqlite

/** Runtime guard: this module binds SQLite through the Java 25 Foreign Function
  * & Memory API, so its tests must run on a Java 25 runtime. A wrong-JDK
  * environment fails this module loudly here rather than deep inside an FFM
  * downcall.
  */
class Jdk25GuardSuite extends munit.FunSuite:

  test("tests run on a Java 25 runtime"):
    assertEquals(Runtime.version().feature(), 25, s"expected JDK 25, got ${Runtime.version()}")
