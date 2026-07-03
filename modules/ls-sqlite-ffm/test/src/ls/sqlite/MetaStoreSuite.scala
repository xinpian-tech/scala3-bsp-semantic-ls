package ls.sqlite

import java.nio.file.Path

import ls.index.{DocId, SymKind, SymbolId, Span, TargetId, UnsafeReason}

class MetaStoreSuite extends munit.FunSuite with TempDbFixture:

  private def open(dir: Path): MetaStore = MetaStore.open(dir.resolve("meta.sqlite"))

  private def newTarget(store: MetaStore, bspId: String = "bsp://ws/target?id=a"): TargetId =
    store.upsertTarget(
      bspId = bspId,
      scalaVersion = "3.8.4",
      classpathHash = "cp0",
      optionsHash = "opt0",
      semanticdbRoot = "/ws/out/semanticdb",
      sourceroot = "/ws",
      active = true
    )

  // --- targets ---

  tempDir.test("upsertTarget inserts then updates in place, keeping the id") { dir =>
    val store = open(dir)
    try
      val id1 = newTarget(store)
      val id2 = store.upsertTarget(
        bspId = "bsp://ws/target?id=a",
        scalaVersion = "3.8.4",
        classpathHash = "cp1",
        optionsHash = "opt1",
        semanticdbRoot = "/ws/out/semanticdb",
        sourceroot = "/ws",
        active = false
      )
      assertEquals(id1.value, id2.value)
      val row = store.targetByBspId("bsp://ws/target?id=a").get
      assertEquals(row.classpathHash, "cp1")
      assertEquals(row.optionsHash, "opt1")
      assertEquals(row.active, false)
      assertEquals(store.allTargets().length, 1)
      val other = newTarget(store, "bsp://ws/target?id=b")
      assert(other.value != id1.value)
      assertEquals(store.allTargets().length, 2)
    finally store.close()
  }

  // --- documents ---

  tempDir.test("upsertDocument bumps epoch exactly when md5 or mtime change") { dir =>
    val store = open(dir)
    try
      val target = newTarget(store)
      val uri = "file:///ws/src/Main.scala"
      val (doc1, e1) =
        store.upsertDocument(target, uri, "/sdb/Main.scala.semanticdb", 1000L, "md5-a", false, false)
      assertEquals(e1, 1L)
      // unchanged: same md5 + mtime -> same epoch
      val (doc2, e2) =
        store.upsertDocument(target, uri, "/sdb/Main.scala.semanticdb", 1000L, "md5-a", false, false)
      assertEquals(doc2, doc1)
      assertEquals(e2, 1L)
      // md5 changed -> epoch bump
      val (doc3, e3) =
        store.upsertDocument(target, uri, "/sdb/Main.scala.semanticdb", 1000L, "md5-b", false, false)
      assertEquals(doc3, doc1)
      assertEquals(e3, 2L)
      // mtime changed -> epoch bump
      val (doc4, e4) =
        store.upsertDocument(target, uri, "/sdb/Main.scala.semanticdb", 2000L, "md5-b", true, true)
      assertEquals(doc4, doc1)
      assertEquals(e4, 3L)
      val row = store.document(target, uri).get
      assertEquals(row.epoch, 3L)
      assertEquals(row.generated, true)
      assertEquals(row.readonly, true)
      assertEquals(store.documentsByUri(uri).map(_.docId), Vector(doc1))
      // same uri in another target is a separate document with its own epoch
      val target2 = newTarget(store, "bsp://ws/target?id=b")
      val (doc5, e5) =
        store.upsertDocument(target2, uri, "/sdb2/Main.scala.semanticdb", 1000L, "md5-a", false, false)
      assert(doc5 != doc1)
      assertEquals(e5, 1L)
      assertEquals(store.documentsByUri(uri).length, 2)
    finally store.close()
  }

  // --- symbol interning ---

  tempDir.test("internSymbols is idempotent and never duplicates rows") { dir =>
    val store = open(dir)
    try
      val target = newTarget(store)
      val (doc, _) =
        store.upsertDocument(target, "file:///ws/A.scala", "/sdb/A", 1L, "m", false, false)
      val batch = Seq(
        SymbolInternRow(1L, "com/example/Foo#", None, 11L),
        SymbolInternRow(1L, "com/example/Foo#bar().", None, 12L),
        SymbolInternRow(1L, "local0", Some(doc), 13L),
        // duplicate inside the batch must not create a second row
        SymbolInternRow(1L, "com/example/Foo#", None, 11L)
      )
      val first = store.internSymbols(batch)
      assertEquals(first.size, 3)
      assertEquals(store.symbolCount(), 3L)
      val again = store.internSymbols(batch)
      assertEquals(again, first)
      assertEquals(store.symbolCount(), 3L)
      // same symbol text: global vs local vs other universe are distinct
      val (doc2, _) =
        store.upsertDocument(target, "file:///ws/B.scala", "/sdb/B", 1L, "m", false, false)
      val more = store.internSymbols(
        Seq(
          SymbolInternRow(1L, "local0", Some(doc2), 14L),
          SymbolInternRow(2L, "com/example/Foo#", None, 11L)
        )
      )
      assertEquals(store.symbolCount(), 5L)
      val allIds = (first.values ++ more.values).map(_.value).toSet
      assertEquals(allIds.size, 5)
    finally store.close()
  }

  // --- symbol metadata ---

  tempDir.test("replaceSymbolMetadata replaces a document's rows and reads back") { dir =>
    val store = open(dir)
    try
      val target = newTarget(store)
      val (doc, _) =
        store.upsertDocument(target, "file:///ws/A.scala", "/sdb/A", 1L, "m", false, false)
      val ids = store.internSymbols(
        Seq(
          SymbolInternRow(1L, "com/example/Foo#", None, 1L),
          SymbolInternRow(1L, "com/example/Foo#bar().", None, 2L)
        )
      )
      val foo = ids(SymbolInternRow(1L, "com/example/Foo#", None, 1L))
      val bar = ids(SymbolInternRow(1L, "com/example/Foo#bar().", None, 2L))
      val rows = Seq(
        SymbolMetadataRow(foo, target, "Foo", None, Some("com.example"), SymKind.Class, 0x8, Some(99L), Some(Span(3, 6, 3, 9))),
        SymbolMetadataRow(bar, target, "bar", Some("Foo"), Some("com.example"), SymKind.Method, 0, None, None)
      )
      store.replaceSymbolMetadata(doc, rows)
      assertEquals(store.symbolMetadataFor(doc).toSet, rows.toSet)
      // replace drops the old rows entirely
      val replacement = Seq(
        SymbolMetadataRow(foo, target, "Foo", None, Some("com.example"), SymKind.Trait, 0, None, Some(Span(1, 0, 1, 3)))
      )
      store.replaceSymbolMetadata(doc, replacement)
      assertEquals(store.symbolMetadataFor(doc), replacement.toVector)
    finally store.close()
  }

  // --- groups ---

  tempDir.test("ref/rename groups assign, reassign and expose unsafe masks") { dir =>
    val store = open(dir)
    try
      val ids = store.internSymbols(
        Seq(
          SymbolInternRow(1L, "com/example/Foo#", None, 1L),
          SymbolInternRow(1L, "com/example/Foo.", None, 2L),
          SymbolInternRow(1L, "com/example/Foo#`<init>`().", None, 3L)
        )
      )
      val Seq(cls, obj, ctor) = Seq(
        SymbolInternRow(1L, "com/example/Foo#", None, 1L),
        SymbolInternRow(1L, "com/example/Foo.", None, 2L),
        SymbolInternRow(1L, "com/example/Foo#`<init>`().", None, 3L)
      ).map(ids(_))

      val refG = store.newRefGroup()
      store.assignRefGroups(Map(cls -> refG, obj -> refG, ctor -> refG))
      assertEquals(store.refGroupOf(cls), Some(refG))
      assertEquals(store.refGroupOf(obj), Some(refG))
      assertEquals(store.refGroupOf(SymbolId(9999L)), None)

      val mask = UnsafeReason.OverrideFamily | UnsafeReason.GeneratedOccurrence
      val renameSafe = store.newRenameGroup()
      val renameUnsafe = store.newRenameGroup(mask)
      assert(renameSafe.value != renameUnsafe.value)
      store.assignRenameGroups(Map(cls -> renameSafe, obj -> renameSafe, ctor -> renameUnsafe))
      assertEquals(store.renameGroupOf(ctor), Some(renameUnsafe))
      assertEquals(store.renameGroupUnsafeMask(renameUnsafe), Some(mask))
      assertEquals(store.renameGroupUnsafeMask(renameSafe), Some(0L))

      // reassignment overwrites
      store.assignRenameGroups(Map(ctor -> renameSafe))
      assertEquals(store.renameGroupOf(ctor), Some(renameSafe))
      store.setRenameGroupUnsafeMask(renameSafe, UnsafeReason.External)
      assertEquals(store.renameGroupUnsafeMask(renameSafe), Some(UnsafeReason.External))
      intercept[IllegalArgumentException] {
        store.setRenameGroupUnsafeMask(ls.index.RenameGroupId(424242L), 1L)
      }
    finally store.close()
  }

  // --- workspace symbols / FTS ---

  private def wsRow(
      name: String,
      owner: Option[String],
      pkg: Option[String],
      kind: SymKind,
      target: TargetId,
      sym: SymbolId
  ) = WorkspaceSymbolRow(name, owner, pkg, kind, target, sym)

  /** Interns a symbol and writes metadata + workspace row for it, the way the
    * ingest pipeline does (contentless FTS cannot store the names itself).
    */
  private def indexSymbol(
      store: MetaStore,
      target: TargetId,
      doc: DocId,
      universe: Long,
      semantic: String,
      name: String,
      owner: Option[String],
      pkg: Option[String],
      kind: SymKind
  ): (SymbolId, WorkspaceSymbolRow, SymbolMetadataRow) =
    val row = SymbolInternRow(universe, semantic, None, name.hashCode.toLong)
    val sym = store.internSymbols(Seq(row))(row)
    (sym, wsRow(name, owner, pkg, kind, target, sym),
      SymbolMetadataRow(sym, target, name, owner, pkg, kind, 0, None, None))

  tempDir.test("concurrent readers run FTS queries with correct results via the reader pool") { dir =>
    val store = open(dir)
    try
      val target = newTarget(store)
      val (doc, _) = store.upsertDocument(target, "file:///ws/A.scala", "/sdb/A", 1L, "m", false, false)
      val (fooSym, fooWs, fooMeta) = indexSymbol(
        store, target, doc, 1L, "com/example/FooBuilder#", "FooBuilder", Some("example"), Some("com.example"), SymKind.Class)
      val (barSym, barWs, barMeta) = indexSymbol(
        store, target, doc, 1L, "com/example/BarWriter#", "BarWriter", Some("example"), Some("com.example"), SymKind.Class)
      store.replaceSymbolMetadata(doc, Seq(fooMeta, barMeta))
      store.replaceWorkspaceSymbols(doc, Seq(fooWs, barWs))

      // more threads than the pool has connections, so borrowers queue; each
      // query still returns the correct FTS results on its borrowed reader
      val threadCount = 8
      val start = new java.util.concurrent.CountDownLatch(1)
      val errors = new java.util.concurrent.ConcurrentLinkedQueue[String]()
      val threads = (1 to threadCount).map { _ =>
        val t = new Thread(() => {
          start.await()
          var i = 0
          while i < 25 do
            val foo = store.workspaceSymbolSearch("Foo", 10).map(_.symbolId).toSet
            if foo != Set(fooSym) then errors.add(s"Foo -> $foo")
            val bar = store.workspaceSymbolSearch("BarWriter", 10).map(_.symbolId)
            if bar != Vector(barSym) then errors.add(s"BarWriter -> $bar")
            i += 1
        })
        t.setDaemon(true)
        t
      }
      threads.foreach(_.start())
      start.countDown()
      threads.foreach(_.join(30000))
      assert(errors.isEmpty, s"concurrent FTS results diverged: ${errors.toArray.mkString("; ")}")
    finally store.close()
  }

  tempDir.test("workspaceSymbolSearch supports prefix and multi-token queries") { dir =>
    val store = open(dir)
    try
      val target = newTarget(store)
      val (doc, _) = store.upsertDocument(target, "file:///ws/A.scala", "/sdb/A", 1L, "m", false, false)
      val (fooSym, fooWs, fooMeta) = indexSymbol(
        store, target, doc, 1L, "com/example/FooBuilder#", "FooBuilder", Some("example"), Some("com.example"), SymKind.Class)
      val (barSym, barWs, barMeta) = indexSymbol(
        store, target, doc, 1L, "com/example/BarWriter#", "BarWriter", Some("example"), Some("com.example"), SymKind.Class)
      val (bazSym, bazWs, bazMeta) = indexSymbol(
        store, target, doc, 1L, "org/other/FooReader#", "FooReader", Some("other"), Some("org.other"), SymKind.Trait)
      store.replaceSymbolMetadata(doc, Seq(fooMeta, barMeta, bazMeta))
      store.replaceWorkspaceSymbols(doc, Seq(fooWs, barWs, bazWs))

      // prefix search
      val foo = store.workspaceSymbolSearch("Foo", 10)
      assertEquals(foo.map(_.symbolId).toSet, Set(fooSym, bazSym))
      val hit = foo.find(_.symbolId == fooSym).get
      assertEquals(hit.displayName, "FooBuilder")
      assertEquals(hit.ownerName, Some("example"))
      assertEquals(hit.packageName, Some("com.example"))
      assertEquals(hit.kind, SymKind.Class)
      assertEquals(hit.docId, doc)
      assertEquals(hit.targetId, target)

      // full-token search
      assertEquals(store.workspaceSymbolSearch("BarWriter", 10).map(_.symbolId), Vector(barSym))
      // multi-token: both tokens must match (display + package here)
      assertEquals(
        store.workspaceSymbolSearch("Foo com.example", 10).map(_.symbolId),
        Vector(fooSym)
      )
      assertEquals(store.workspaceSymbolSearch("Foo org", 10).map(_.symbolId), Vector(bazSym))
      // no match
      assertEquals(store.workspaceSymbolSearch("Quux", 10), Vector.empty)
      // blank query
      assertEquals(store.workspaceSymbolSearch("   ", 10), Vector.empty)
      // limit
      assertEquals(store.workspaceSymbolSearch("Foo", 1).length, 1)
      // FTS syntax characters are neutralized by quoting: no parse error, and
      // the tokenizer strips punctuation so this behaves like the "Foo" prefix
      assertEquals(
        store.workspaceSymbolSearch("Foo\"(*)", 10).map(_.symbolId).toSet,
        Set(fooSym, bazSym)
      )
      // pure punctuation yields no tokens and therefore no matches, no error
      assertEquals(store.workspaceSymbolSearch("(*)", 10), Vector.empty)
    finally store.close()
  }

  tempDir.test("camel-hump fuzzy fallback ranks the hump-aligned match first (FTS underfill)") { dir =>
    val store = open(dir)
    try
      val target = newTarget(store)
      val (doc, _) = store.upsertDocument(target, "file:///ws/Fuzzy.scala", "/sdb/Fuzzy", 1L, "m", false, false)
      val (wsSym, wsWs, wsMeta) =
        indexSymbol(store, target, doc, 1L, "a/workspaceSymbol#", "workspaceSymbol", None, None, SymKind.Class)
      val (whSym, whWs, whMeta) =
        indexSymbol(store, target, doc, 1L, "a/whimsy#", "whimsy", None, None, SymKind.Class)
      store.replaceSymbolMetadata(doc, Seq(wsMeta, whMeta))
      store.replaceWorkspaceSymbols(doc, Seq(wsWs, whWs))

      // FTS prefix cannot match the camel-hump query "wSy"; the fuzzy fallback
      // finds both and ranks hump-aligned workspaceSymbol above plain whimsy.
      assertEquals(store.workspaceSymbolSearch("wSy", 10).map(_.symbolId), Vector(wsSym, whSym))
      // exact/prefix stays the primary FTS path
      assertEquals(store.workspaceSymbolSearch("workspace", 10).map(_.symbolId), Vector(wsSym))
    finally store.close()
  }

  tempDir.test("MetaStore.open migrates a pre-populated v1 database and fuzzy search then works") { dir =>
    val path = dir.resolve("meta.sqlite")
    // Hand-build a schema-v1 database with one workspace-symbol row (no sidecar).
    val v1 = Db.open(path)
    try
      v1.withWriteTransaction {
        Schema.tables.foreach(v1.exec)
        Schema.indexes.foreach(v1.exec)
        v1.exec("PRAGMA user_version=1")
      }
      v1.exec(
        "INSERT INTO symbol_metadata (symbol_id, target_id, doc_id, display_name, kind, properties) VALUES (5, 1, 1, 'workspaceSymbol', 0, 0)"
      )
      v1.exec(
        "INSERT INTO workspace_symbol_rows (rowid, symbol_id, target_id, doc_id, kind) VALUES (1, 5, 1, 1, 0)"
      )
    finally v1.close()

    val store = MetaStore.open(path) // triggers the v1 -> v2 migration + backfill
    try
      assertEquals(Schema.userVersion(store.db), 2)
      assertEquals(store.workspaceSymbolSearch("wSy", 10).map(_.symbolId), Vector(SymbolId(5)))
    finally store.close()
  }

  tempDir.test("fuzzy fallback pulls a bounded candidate set (cap 5000) on a large corpus") { dir =>
    val store = open(dir)
    try
      val n = MetaStore.FuzzyCandidateCap + 500 // more sidecar rows than the cap
      store.db.withWriteTransaction {
        val insRow = store.db.prepare(
          "INSERT INTO workspace_symbol_rows (rowid, symbol_id, target_id, doc_id, kind) VALUES (?, ?, 1, 1, 0)"
        )
        val insFz = store.db.prepare(
          "INSERT INTO workspace_symbol_fuzzy (rowid, normalized_name, initials) VALUES (?, ?, ?)"
        )
        var i = 1
        while i <= n do
          insRow.bindLong(1, i.toLong).bindLong(2, i.toLong).run()
          insFz.bindLong(1, i.toLong).bindText(2, s"zeta$i").bindText(3, "z").run()
          i += 1
      }
      // "zq" prefix-matches nothing in FTS, forcing the fuzzy fallback; every
      // sidecar row shares the first character 'z', so the pull is capped.
      val hits = store.workspaceSymbolSearch("zq", 10)
      assert(hits.length <= 10, hits.length.toString)
      assertEquals(store.lastFuzzyCandidateCount, MetaStore.FuzzyCandidateCap)
    finally store.close()
  }

  tempDir.test("replaceWorkspaceSymbols drops a document's old FTS entries") { dir =>
    val store = open(dir)
    try
      val target = newTarget(store)
      val (docA, _) = store.upsertDocument(target, "file:///ws/A.scala", "/sdb/A", 1L, "m", false, false)
      val (docB, _) = store.upsertDocument(target, "file:///ws/B.scala", "/sdb/B", 1L, "m", false, false)
      val (oldSym, oldWs, oldMeta) = indexSymbol(
        store, target, docA, 1L, "com/example/OldName#", "OldName", None, Some("com.example"), SymKind.Class)
      val (keepSym, keepWs, keepMeta) = indexSymbol(
        store, target, docB, 1L, "com/example/Keeper#", "Keeper", None, Some("com.example"), SymKind.Object)
      store.replaceSymbolMetadata(docA, Seq(oldMeta))
      store.replaceSymbolMetadata(docB, Seq(keepMeta))
      store.replaceWorkspaceSymbols(docA, Seq(oldWs))
      store.replaceWorkspaceSymbols(docB, Seq(keepWs))
      assertEquals(store.workspaceSymbolSearch("OldName", 10).map(_.symbolId), Vector(oldSym))

      // re-ingest doc A with a different symbol set
      val (newSym, newWs, newMeta) = indexSymbol(
        store, target, docA, 1L, "com/example/NewName#", "NewName", None, Some("com.example"), SymKind.Class)
      store.replaceSymbolMetadata(docA, Seq(newMeta))
      store.replaceWorkspaceSymbols(docA, Seq(newWs))

      assertEquals(store.workspaceSymbolSearch("OldName", 10), Vector.empty)
      assertEquals(store.workspaceSymbolSearch("NewName", 10).map(_.symbolId), Vector(newSym))
      // other documents are untouched
      assertEquals(store.workspaceSymbolSearch("Keeper", 10).map(_.symbolId), Vector(keepSym))
      // rows table stays in sync with fts
      val rowCount = store.db
        .prepare("SELECT count(*) FROM workspace_symbol_rows")
        .queryOne(_.columnLong(0))
      assertEquals(rowCount, Some(2L))
    finally store.close()
  }

  tempDir.test("workspace symbols with CJK and emoji round-trip through FTS") { dir =>
    val store = open(dir)
    try
      val target = newTarget(store)
      val (doc, _) = store.upsertDocument(target, "file:///ws/多语言.scala", "/sdb/多语言", 1L, "m", false, false)
      val name = "数据库索引器🚀"
      val (sym, ws, meta) = indexSymbol(
        store, target, doc, 1L, "com/example/数据库索引器#", name, Some("例子"), Some("com.例子"), SymKind.Class)
      store.replaceSymbolMetadata(doc, Seq(meta))
      store.replaceWorkspaceSymbols(doc, Seq(ws))
      val hits = store.workspaceSymbolSearch("数据库", 10)
      assertEquals(hits.map(_.symbolId), Vector(sym))
      assertEquals(hits.head.displayName, name)
      assertEquals(hits.head.ownerName, Some("例子"))
      assertEquals(store.symbolMetadataFor(doc).head.displayName, name)
    finally store.close()
  }

  // --- segment manifest ---

  tempDir.test("segment manifest activation swaps atomically to one active segment") { dir =>
    val store = open(dir)
    try
      assertEquals(store.activeSegment(), None)
      val s1 = store.insertSegment("postings/segment-000001", 111L, 1L, 5L, 0xdeadL)
      val s2 = store.insertSegment("postings/segment-000002", 222L, 1L, 9L, 0xbeefL)
      assert(s1 != s2)
      assertEquals(store.activeSegment(), None) // inserted inactive
      store.activateSegment(s1)
      assertEquals(store.activeSegment().map(_.segmentId), Some(s1))
      store.activateSegment(s2)
      val all = store.allSegments()
      assertEquals(all.length, 2)
      assertEquals(all.filter(_.active).map(_.segmentId), Vector(s2))
      val active = store.activeSegment().get
      assertEquals(active.path, "postings/segment-000002")
      assertEquals(active.createdAtMs, 222L)
      assertEquals(active.minEpoch, 1L)
      assertEquals(active.maxEpoch, 9L)
      assertEquals(active.checksum, 0xbeefL)
      // re-activating the already-active segment is a no-op
      store.activateSegment(s2)
      assertEquals(store.allSegments().count(_.active), 1)
      intercept[IllegalArgumentException](store.activateSegment(31337L))
      // failed activation left the previous active segment in place
      assertEquals(store.activeSegment().map(_.segmentId), Some(s2))
    finally store.close()
  }

  // --- transactionality across the DAO ---

  tempDir.test("a failing ingest transaction rolls back every DAO write") { dir =>
    val store = open(dir)
    try
      val target = newTarget(store)
      val (doc, _) = store.upsertDocument(target, "file:///ws/A.scala", "/sdb/A", 1L, "m", false, false)
      intercept[RuntimeException] {
        store.db.withWriteTransaction {
          val row = SymbolInternRow(1L, "com/example/Doomed#", None, 5L)
          val sym = store.internSymbols(Seq(row))(row)
          store.replaceSymbolMetadata(
            doc,
            Seq(SymbolMetadataRow(sym, target, "Doomed", None, None, SymKind.Class, 0, None, None))
          )
          store.replaceWorkspaceSymbols(
            doc,
            Seq(wsRow("Doomed", None, None, SymKind.Class, target, sym))
          )
          val seg = store.insertSegment("postings/segment-000009", 9L, 1L, 1L, 9L)
          store.activateSegment(seg)
          throw new RuntimeException("ingest failed after all writes")
        }
      }
      assertEquals(store.symbolCount(), 0L)
      assertEquals(store.symbolMetadataFor(doc), Vector.empty)
      assertEquals(store.workspaceSymbolSearch("Doomed", 10), Vector.empty)
      assertEquals(store.allSegments(), Vector.empty)
      // document row from before the failed transaction is still there
      assertEquals(store.document(target, "file:///ws/A.scala").map(_.docId), Some(doc))
    finally store.close()
  }

  tempDir.test("store reopens with data intact (persistence smoke)") { dir =>
    val path = dir.resolve("meta.sqlite")
    val store = MetaStore.open(path)
    val target = newTarget(store)
    val (doc, _) = store.upsertDocument(target, "file:///ws/A.scala", "/sdb/A", 1L, "m", false, false)
    val (sym, ws, meta) = indexSymbol(
      store, target, doc, 1L, "com/example/Persisted#", "Persisted", None, Some("com.example"), SymKind.Class)
    store.replaceSymbolMetadata(doc, Seq(meta))
    store.replaceWorkspaceSymbols(doc, Seq(ws))
    store.close()

    val reopened = MetaStore.open(path)
    try
      assertEquals(reopened.workspaceSymbolSearch("Persist", 10).map(_.symbolId), Vector(sym))
      val again = reopened.internSymbols(Seq(SymbolInternRow(1L, "com/example/Persisted#", None, "Persisted".hashCode.toLong)))
      assertEquals(again.values.head, sym)
    finally reopened.close()
  }
