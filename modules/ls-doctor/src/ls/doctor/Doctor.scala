package ls.doctor

/** Renders the doctor report (plan section 19) for
  * `workspace/executeCommand: scala3SemanticLs.doctor`.
  *
  * [[render]] produces the human-readable text layout with the eight plan-19
  * headings and indented `key: value` lines; [[renderJson]] produces a
  * structured JSON object (hand-rolled, no dependencies) for clients that
  * want machine-readable doctor output.
  */
object Doctor:

  private val Indent = "  "
  private val SubIndent = "    "
  private val ListCap = 20

  // --- text rendering --------------------------------------------------------

  def render(input: DoctorInput): String =
    val sections = Vector(
      section("Runtime", runtimeLines(input.runtime)),
      section("Nix", nixLines(input.nix)),
      sectionOf("BSP", input.bsp)(bspLines),
      sectionOf("SemanticDB", input.semanticdb)(semanticdbLines),
      sectionOf("SQLite", input.sqlite)(sqliteLines),
      sectionOf("Postings", input.postings)(postingsLines),
      sectionOf("PC", input.pc)(pcLines),
      sectionOf("PC Plugins", input.pcPlugins)(pcPluginsLines)
    )
    sections.mkString("\n")

  private def section(name: String, lines: Vector[String]): String =
    (s"$name:" +: lines.map(Indent + _)).mkString("", "\n", "\n")

  private def sectionOf[A](name: String, state: SectionState[A])(f: A => Vector[String]): String =
    section(name, state.fold(reason => Vector(s"unavailable: $reason"))(f))

  private def runtimeLines(r: RuntimeSection): Vector[String] =
    val nativeAccess =
      if r.nativeAccessEnabledFor.isEmpty then "not enabled for any module"
      else s"enabled for ${r.nativeAccessEnabledFor.mkString(", ")}"
    Vector(
      s"Java: ${r.javaVersion}",
      s"Native access: $nativeAccess",
      s"Compact Object Headers: ${r.compactObjectHeaders}",
      s"AOT cache: ${r.aotCache}"
    )

  private def nixLines(n: NixSection): Vector[String] =
    Vector(
      s"flake detected: ${yesNo(n.flakeDetected)}",
      s"mill-ivy-fetcher input: ${yesNo(n.millIvyFetcherInput)}",
      s"ivy lock: ${n.ivyLockPath} (${if n.ivyLockExists then "exists" else "missing"})",
      s"lock status: ${n.lockStatus}"
    )

  private def bspLines(b: BspSection): Vector[String] =
    val server = (b.serverName, b.serverVersion) match
      case (Some(name), Some(version)) => s"$name $version"
      case (Some(name), None) => name
      case (None, Some(version)) => s"unknown server $version"
      case (None, None) => "unknown (initialize result not provided)"
    Vector(
      s"server: $server",
      s"targets: ${b.targetCount}",
      s"Scala 3 targets: ${countAndList(b.scala3Targets)}",
      s"IndexUnavailable targets: ${noneOrList(b.indexUnavailableTargets)}"
    )

  private def semanticdbLines(s: SemanticdbSection): Vector[String] =
    val rootLines = s.roots.map { r =>
      val status =
        if r.exists then s"exists, ${r.semanticdbFileCount} semanticdb files" else "missing"
      s"${Indent}${r.bspId}: ${r.semanticdbRoot} ($status)"
    }
    val freshnessLines = s.freshness match
      case None => Vector("doc freshness: unavailable: not computed yet")
      case Some(f) =>
        val uris =
          if f.uris.isEmpty then Vector.empty
          else Vector(s"stale/missing uris: ${f.uris.mkString(", ")}")
        Vector(
          s"fresh docs: ${f.fresh}",
          s"stale docs (md5 mismatch): ${f.stale}",
          s"missing docs: ${f.missing}"
        ) ++ uris
    (s"semanticdb roots: ${s.roots.length}" +: rootLines) ++ freshnessLines

  private def sqliteLines(s: SqliteSection): Vector[String] =
    val manifest = (s.activeSegmentId, s.activeSegmentPath) match
      case (Some(id), Some(path)) => s"segment $id ($path)"
      case (Some(id), None) => s"segment $id"
      case _ => "none (no active segment)"
    Vector(
      s"database: ${s.databasePath}",
      s"WAL: ${enabledDisabled(s.walEnabled)} (journal_mode=${s.journalMode})",
      s"FTS: ${enabledDisabled(s.ftsEnabled)} (workspace_symbols_fts ${if s.ftsEnabled then "present" else "absent"})",
      s"manifest generation: $manifest",
      s"documents: ${s.documentCount}",
      s"generated source status: ${s.generatedDocumentCount}",
      s"stale targets: ${noneOrList(s.staleTargets)}",
      s"symbols: ${s.symbolCount}",
      s"wal size: ${s.walSizeBytes} bytes"
    )

  private def postingsLines(p: PostingsSection): Vector[String] =
    val segmentLines = p.segments.map { s =>
      val marker = if s.active then "active" else "superseded"
      s"${Indent}segment ${s.segmentId}: ${s.path} ($marker)"
    }
    val pendingDirs =
      if p.compactionPendingDirs.isEmpty then Vector.empty
      else p.compactionPendingDirs.take(ListCap).map(d => s"${Indent}pending: $d")
    (s"active segments: ${p.activeSegments.length} of ${p.segments.length}" +: segmentLines) ++
      Vector(
        s"snapshot id: ${p.snapshotId.map(_.toString).getOrElse("none published")}",
        s"snapshot docs: ${p.snapshotDocCount.map(_.toString).getOrElse("n/a")}",
        s"snapshot occurrences: ${p.snapshotOccurrenceCount.map(_.toString).getOrElse("n/a")}",
        s"compaction pending: ${p.compactionPending}"
      ) ++ pendingDirs

  private def pcLines(p: PcSection): Vector[String] =
    Vector(
      s"worker status: ${p.workerStatus}",
      s"active targets: ${noneOrList(p.activeTargets)}",
      s"registered targets: ${noneOrList(p.registeredTargets)}"
    )

  private def pcPluginsLines(p: PcPluginsSection): Vector[String] =
    val compilerLoaded = p.compilerPlugins.count(_.loaded)
    val compilerLines = p.compilerPlugins.map { c =>
      val jars = if c.jars.isEmpty then "(no jars)" else c.jars.mkString(", ")
      s"$Indent$jars: ${c.detail}"
    }
    val serviceEnabled = p.servicePlugins.count(_.enabled)
    val serviceLines = p.servicePlugins.map { s =>
      s"$Indent${s.id} (${s.source}): ${if s.enabled then "enabled" else "disabled"}"
    }
    val selfTestLines =
      if p.servicePlugins.isEmpty then Vector(s"${Indent}none")
      else
        p.servicePlugins.map { s =>
          s"$Indent${s.id}: ${if s.selfTestOk then "ok" else s.selfTestDetail}"
        }
    val disabledLines =
      if p.disabled.isEmpty then Vector.empty
      else p.disabled.map(d => s"$Indent${d.id}: ${d.reason}")
    (s"compiler plugins loaded: $compilerLoaded of ${p.compilerPlugins.length}" +: compilerLines) ++
      (s"service plugins loaded: $serviceEnabled of ${p.servicePlugins.length}" +: serviceLines) ++
      ("self-test results:" +: selfTestLines) ++
      (s"disabled plugins: ${if p.disabled.isEmpty then "none" else p.disabled.length.toString}" +: disabledLines)

  private def yesNo(b: Boolean): String = if b then "yes" else "no"
  private def enabledDisabled(b: Boolean): String = if b then "enabled" else "disabled"

  private def countAndList(items: Vector[String]): String =
    if items.isEmpty then "0" else s"${items.length} (${capped(items)})"

  private def noneOrList(items: Vector[String]): String =
    if items.isEmpty then "none" else s"${items.length} (${capped(items)})"

  private def capped(items: Vector[String]): String =
    if items.length <= ListCap then items.mkString(", ")
    else items.take(ListCap).mkString(", ") + s", ... (+${items.length - ListCap} more)"

  // --- JSON rendering ----------------------------------------------------------

  /** Minimal hand-rolled JSON object for structured executeCommand replies.
    * Section keys: runtime, nix, bsp, semanticdb, sqlite, postings, pc,
    * pcPlugins. Unavailable sections encode as `{"unavailable": "<reason>"}`.
    */
  def renderJson(input: DoctorInput): String =
    obj(
      "runtime" -> runtimeJson(input.runtime),
      "nix" -> nixJson(input.nix),
      "bsp" -> stateJson(input.bsp)(bspJson),
      "semanticdb" -> stateJson(input.semanticdb)(semanticdbJson),
      "sqlite" -> stateJson(input.sqlite)(sqliteJson),
      "postings" -> stateJson(input.postings)(postingsJson),
      "pc" -> stateJson(input.pc)(pcJson),
      "pcPlugins" -> stateJson(input.pcPlugins)(pcPluginsJson)
    )

  private def stateJson[A](state: SectionState[A])(f: A => String): String =
    state.fold(reason => obj("unavailable" -> str(reason)))(f)

  private def runtimeJson(r: RuntimeSection): String =
    obj(
      "javaVersion" -> str(r.javaVersion),
      "nativeAccessEnabledFor" -> arr(r.nativeAccessEnabledFor.map(str)),
      "compactObjectHeaders" -> str(r.compactObjectHeaders),
      "aotCache" -> str(r.aotCache)
    )

  private def nixJson(n: NixSection): String =
    obj(
      "flakeDetected" -> bool(n.flakeDetected),
      "millIvyFetcherInput" -> bool(n.millIvyFetcherInput),
      "ivyLockPath" -> str(n.ivyLockPath),
      "ivyLockExists" -> bool(n.ivyLockExists),
      "lockStatus" -> str(n.lockStatus)
    )

  private def bspJson(b: BspSection): String =
    obj(
      "serverName" -> optStr(b.serverName),
      "serverVersion" -> optStr(b.serverVersion),
      "targetCount" -> num(b.targetCount),
      "scala3Targets" -> arr(b.scala3Targets.map(str)),
      "indexUnavailableTargets" -> arr(b.indexUnavailableTargets.map(str))
    )

  private def semanticdbJson(s: SemanticdbSection): String =
    val roots = s.roots.map { r =>
      obj(
        "bspId" -> str(r.bspId),
        "semanticdbRoot" -> str(r.semanticdbRoot),
        "exists" -> bool(r.exists),
        "semanticdbFileCount" -> num(r.semanticdbFileCount)
      )
    }
    val freshness = s.freshness match
      case None => "null"
      case Some(f) =>
        obj(
          "fresh" -> num(f.fresh),
          "stale" -> num(f.stale),
          "missing" -> num(f.missing),
          "uris" -> arr(f.uris.map(str))
        )
    obj("roots" -> arr(roots), "freshness" -> freshness)

  private def sqliteJson(s: SqliteSection): String =
    obj(
      "databasePath" -> str(s.databasePath),
      "walEnabled" -> bool(s.walEnabled),
      "journalMode" -> str(s.journalMode),
      "ftsEnabled" -> bool(s.ftsEnabled),
      "activeSegmentId" -> s.activeSegmentId.map(num).getOrElse("null"),
      "activeSegmentPath" -> optStr(s.activeSegmentPath),
      "documentCount" -> num(s.documentCount),
      "generatedDocumentCount" -> num(s.generatedDocumentCount),
      "staleTargets" -> arr(s.staleTargets.map(str)),
      "symbolCount" -> num(s.symbolCount),
      "walSizeBytes" -> num(s.walSizeBytes)
    )

  private def postingsJson(p: PostingsSection): String =
    val segments = p.segments.map { s =>
      obj("segmentId" -> num(s.segmentId), "path" -> str(s.path), "active" -> bool(s.active))
    }
    obj(
      "segments" -> arr(segments),
      "snapshotId" -> p.snapshotId.map(num).getOrElse("null"),
      "snapshotDocCount" -> p.snapshotDocCount.map(c => num(c.toLong)).getOrElse("null"),
      "snapshotOccurrenceCount" -> p.snapshotOccurrenceCount.map(num).getOrElse("null"),
      "compactionPending" -> num(p.compactionPending),
      "compactionPendingDirs" -> arr(p.compactionPendingDirs.map(str))
    )

  private def pcJson(p: PcSection): String =
    obj(
      "workerStatus" -> str(p.workerStatus),
      "activeTargets" -> arr(p.activeTargets.map(str)),
      "registeredTargets" -> arr(p.registeredTargets.map(str))
    )

  private def pcPluginsJson(p: PcPluginsSection): String =
    val compiler = p.compilerPlugins.map { c =>
      obj(
        "jars" -> arr(c.jars.map(str)),
        "options" -> arr(c.options.map(str)),
        "loaded" -> bool(c.loaded),
        "detail" -> str(c.detail)
      )
    }
    val service = p.servicePlugins.map { s =>
      obj(
        "id" -> str(s.id),
        "source" -> str(s.source),
        "enabled" -> bool(s.enabled),
        "selfTestOk" -> bool(s.selfTestOk),
        "selfTestDetail" -> str(s.selfTestDetail)
      )
    }
    val disabled = p.disabled.map(d => obj("id" -> str(d.id), "reason" -> str(d.reason)))
    obj(
      "compilerPlugins" -> arr(compiler),
      "servicePlugins" -> arr(service),
      "disabled" -> arr(disabled)
    )

  // --- JSON primitives -----------------------------------------------------------

  private def obj(fields: (String, String)*): String =
    fields.map((k, v) => s"${str(k)}: $v").mkString("{", ", ", "}")

  private def arr(items: Vector[String]): String = items.mkString("[", ", ", "]")

  private def num(v: Long): String = v.toString
  private def bool(v: Boolean): String = if v then "true" else "false"
  private def optStr(v: Option[String]): String = v.map(str).getOrElse("null")

  private def str(v: String): String =
    val sb = new StringBuilder(v.length + 2)
    sb.append('"')
    v.foreach {
      case '"' => sb.append("\\\"")
      case '\\' => sb.append("\\\\")
      case '\n' => sb.append("\\n")
      case '\r' => sb.append("\\r")
      case '\t' => sb.append("\\t")
      case c if c < 0x20 => sb.append(f"\\u${c.toInt}%04x")
      case c => sb.append(c)
    }
    sb.append('"')
    sb.toString
