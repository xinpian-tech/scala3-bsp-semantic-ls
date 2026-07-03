package ls.postings

import java.nio.charset.StandardCharsets
import java.nio.file.{Files, Path, StandardCopyOption}

/** The `snapshots/current.json` record: the published segment's identity, when
  * it was published, and a monotonic publish generation. Written atomically on
  * every publish (tmp file + `ATOMIC_MOVE`) and cross-checked against the SQLite
  * manifest at recovery / by the doctor. `segmentId` is the postings segment id
  * (`PostingsSnapshot.snapshotId`); `path` is the segment directory — the key
  * the manifest cross-check uses, since the manifest's own `segment_id` is a
  * separate auto-increment primary key.
  */
final case class CurrentSnapshotFile(
    segmentId: Long,
    path: String,
    publishedAtMs: Long,
    generation: Long
)

object CurrentSnapshotFile:

  /** `<root>/snapshots/current.json`, where `root` is the storage root (the
    * sibling of `postings/`), not the postings segment root.
    */
  def pathIn(root: Path): Path = root.resolve("snapshots").resolve("current.json")

  /** Hand-rolled JSON (ls-postings has no JSON dependency); stable key order. */
  def render(f: CurrentSnapshotFile): String =
    val sb = new StringBuilder(96)
    sb.append("{\"segmentId\": ").append(f.segmentId)
    sb.append(", \"path\": ").append(quote(f.path))
    sb.append(", \"publishedAtMs\": ").append(f.publishedAtMs)
    sb.append(", \"generation\": ").append(f.generation)
    sb.append("}")
    sb.toString

  /** Atomically (re)writes `<root>/snapshots/current.json`: a complete temp
    * file is written and moved into place, so a reader never sees a partial
    * file and a crash leaves at most a `.tmp`.
    */
  def writeAtomic(root: Path, f: CurrentSnapshotFile): Unit =
    val dir = root.resolve("snapshots")
    Files.createDirectories(dir)
    val dest = dir.resolve("current.json")
    val tmp = dir.resolve("current.json.tmp")
    Files.write(tmp, render(f).getBytes(StandardCharsets.UTF_8))
    Files.move(tmp, dest, StandardCopyOption.ATOMIC_MOVE)

  /** Reads and parses the file, or None if it is absent or unparseable. */
  def read(root: Path): Option[CurrentSnapshotFile] =
    val p = pathIn(root)
    if !Files.isRegularFile(p) then None
    else
      try parse(new String(Files.readAllBytes(p), StandardCharsets.UTF_8))
      catch case scala.util.control.NonFatal(_) => None

  private def parse(json: String): Option[CurrentSnapshotFile] =
    for
      segmentId <- longField(json, "segmentId")
      path <- stringField(json, "path")
      publishedAtMs <- longField(json, "publishedAtMs")
      generation <- longField(json, "generation")
    yield CurrentSnapshotFile(segmentId, path, publishedAtMs, generation)

  private def longField(json: String, key: String): Option[Long] =
    val m = raw"""(?s)"${java.util.regex.Pattern.quote(key)}"\s*:\s*(-?\d+)""".r
    m.findFirstMatchIn(json).flatMap(_.group(1).toLongOption)

  private def stringField(json: String, key: String): Option[String] =
    val m = raw"""(?s)"${java.util.regex.Pattern.quote(key)}"\s*:\s*"((?:\\.|[^"\\])*)"""".r
    m.findFirstMatchIn(json).map(mt => unquoteBody(mt.group(1)))

  private def quote(s: String): String =
    val sb = new StringBuilder(s.length + 2)
    sb.append('"')
    s.foreach {
      case '"' => sb.append("\\\"")
      case '\\' => sb.append("\\\\")
      case '\n' => sb.append("\\n")
      case '\r' => sb.append("\\r")
      case '\t' => sb.append("\\t")
      case c => sb.append(c)
    }
    sb.append('"')
    sb.toString

  private def unquoteBody(body: String): String =
    val sb = new StringBuilder(body.length)
    var i = 0
    while i < body.length do
      val c = body.charAt(i)
      if c == '\\' && i + 1 < body.length then
        body.charAt(i + 1) match
          case '"' => sb.append('"'); i += 2
          case '\\' => sb.append('\\'); i += 2
          case 'n' => sb.append('\n'); i += 2
          case 'r' => sb.append('\r'); i += 2
          case 't' => sb.append('\t'); i += 2
          case other => sb.append(other); i += 2
      else
        sb.append(c); i += 1
    sb.toString
