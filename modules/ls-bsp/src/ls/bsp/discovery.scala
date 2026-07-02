package ls.bsp

import java.io.IOException
import java.nio.file.Files
import java.nio.file.Path

import scala.jdk.CollectionConverters.*
import scala.util.Using

import ch.epfl.scala.bsp4j.BspConnectionDetails
import com.google.gson.Gson
import com.google.gson.JsonParseException

/** One parsed `.bsp/<name>.json` connection file. */
final case class BspConnectionFile(path: Path, details: BspConnectionDetails)

/** All connection files found under `<workspace>/.bsp`, valid ones sorted
  * deterministically by server name (ties broken by file name), plus the
  * files that failed to parse or validate.
  */
final case class BspDiscoveryResult(
    candidates: Vector[BspConnectionFile],
    invalid: Vector[BspError.InvalidConnectionFile]
):
  /** Deterministic pick: first candidate in name order. */
  def preferred: Option[BspConnectionFile] = candidates.headOption

object BspDiscovery:
  private val gson = new Gson()

  def bspDir(workspaceRoot: Path): Path = workspaceRoot.resolve(".bsp")

  /** Finds and parses every `.bsp/<name>.json` connection file under the
    * workspace root. Never throws for per-file problems: malformed or
    * incomplete files are reported in [[BspDiscoveryResult.invalid]].
    */
  def discover(workspaceRoot: Path): BspDiscoveryResult =
    val dir = bspDir(workspaceRoot)
    if !Files.isDirectory(dir) then BspDiscoveryResult(Vector.empty, Vector.empty)
    else
      val jsonFiles = Using.resource(Files.list(dir)) { stream =>
        stream
          .iterator()
          .asScala
          .filter(p => Files.isRegularFile(p) && p.getFileName.toString.endsWith(".json"))
          .toVector
      }.sortBy(_.getFileName.toString)

      val candidates = Vector.newBuilder[BspConnectionFile]
      val invalid = Vector.newBuilder[BspError.InvalidConnectionFile]
      for path <- jsonFiles do
        parseFile(path) match
          case Right(details) => candidates += BspConnectionFile(path, details)
          case Left(err) => invalid += err
      BspDiscoveryResult(
        candidates.result().sortBy(f => (f.details.getName, f.path.getFileName.toString)),
        invalid.result()
      )

  /** Deterministic pick over [[discover]]: candidate with the first name in
    * lexicographic order, or None when the workspace has no valid file.
    */
  def pick(workspaceRoot: Path): Option[BspConnectionFile] =
    discover(workspaceRoot).preferred

  /** Like [[pick]] but fails with a typed error when nothing usable exists. */
  def required(workspaceRoot: Path): BspConnectionFile =
    pick(workspaceRoot).getOrElse(
      throw BspException(BspError.NoConnectionFile(workspaceRoot.toString))
    )

  private def parseFile(path: Path): Either[BspError.InvalidConnectionFile, BspConnectionDetails] =
    def bad(detail: String) = Left(new BspError.InvalidConnectionFile(path.toString, detail))
    try
      val text = Files.readString(path)
      val details = gson.fromJson(text, classOf[BspConnectionDetails])
      if details == null then bad("file is empty")
      else if details.getName == null || details.getName.isEmpty then
        bad("missing required field 'name'")
      else if details.getArgv == null || details.getArgv.isEmpty then
        bad("missing required field 'argv'")
      else Right(details)
    catch
      case e: JsonParseException =>
        bad(s"malformed JSON: ${Option(e.getMessage).getOrElse(e.getClass.getSimpleName)}")
      case e: IOException =>
        bad(s"unreadable: ${Option(e.getMessage).getOrElse(e.getClass.getSimpleName)}")
