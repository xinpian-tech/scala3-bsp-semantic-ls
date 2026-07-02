package ls.bsp

import java.net.URI
import java.nio.file.Files
import java.nio.file.Path

import scala.jdk.CollectionConverters.*
import scala.util.Try
import scala.util.Using

import ch.epfl.scala.bsp4j.BuildTarget
import ch.epfl.scala.bsp4j.ScalaBuildTarget
import ch.epfl.scala.bsp4j.SourceItem
import ch.epfl.scala.bsp4j.SourceItemKind
import com.google.gson.Gson
import com.google.gson.JsonElement

/** Loads the [[BspProjectModel]] from a live [[BspSession]] (plan Phase 2):
  * workspace/buildTargets filtered down to Scala 3 targets, then
  * buildTarget/sources (directories expanded by walking *.scala files) and
  * buildTarget/scalacOptions, assembled with SemanticDB config extraction.
  */
object ProjectModelLoader:
  private val gson = new Gson()

  def load(session: BspSession): BspProjectModel =
    val workspaceRoot = session.workspaceRoot
    val scala3 = session
      .workspaceBuildTargets()
      .flatMap { t =>
        val languages = Option(t.getLanguageIds).map(_.asScala.toSet).getOrElse(Set.empty)
        if !languages.contains("scala") then None
        else
          parseScalaBuildTarget(t.getData)
            .filter(sbt => isScala3(sbt.getScalaVersion))
            .map(sbt => (t, sbt))
      }
      .sortBy(_._1.getId.getUri)

    if scala3.isEmpty then return BspProjectModel(Vector.empty, Map.empty)

    val ids = scala3.map(_._1.getId.getUri)
    val sourcesByTarget: Map[String, Vector[SourceItem]] =
      session
        .buildTargetSources(ids)
        .map { item =>
          val sources = Option(item.getSources).map(_.asScala.toVector).getOrElse(Vector.empty)
          item.getTarget.getUri -> sources
        }
        .toMap
    val optionsByTarget =
      session.buildTargetScalacOptions(ids).map(item => item.getTarget.getUri -> item).toMap

    val targets = scala3.map { (bt, sbt) =>
      val bspId = bt.getId.getUri
      val optionsItem = optionsByTarget.getOrElse(
        bspId,
        throw BspException(
          BspError.InvalidResponse("buildTarget/scalacOptions", s"missing item for target $bspId")
        )
      )
      val classDirectory = Option(optionsItem.getClassDirectory)
        .filter(_.nonEmpty)
        .map(uri => pathOfUri("buildTarget/scalacOptions", uri))
        .getOrElse(
          throw BspException(
            BspError.InvalidResponse("buildTarget/scalacOptions", s"missing classDirectory for $bspId")
          )
        )
      val options = Option(optionsItem.getOptions).map(_.asScala.toVector).getOrElse(Vector.empty)
      val semanticdb = SemanticdbFlags.extract(options, classDirectory, workspaceRoot)
      val sources = expandSources(sourcesByTarget.getOrElse(bspId, Vector.empty))
      val deps = Option(bt.getDependencies)
        .map(_.asScala.toVector.map(_.getUri))
        .getOrElse(Vector.empty)
        .distinct
        .sorted
      BspTarget(
        bspId = bspId,
        displayName = Option(bt.getDisplayName).filter(_.nonEmpty).getOrElse(bspId),
        scalaVersion = sbt.getScalaVersion,
        scalacOptions = options,
        classDirectory = classDirectory,
        semanticdbRoot = semanticdb.semanticdbRoot,
        sourceroot = Some(semanticdb.sourceroot),
        sources = sources,
        directDeps = deps
      )
    }

    // First target in bspId order wins for shared sources: deterministic.
    val uriToTarget = Map.newBuilder[String, String]
    val claimed = collection.mutable.Set.empty[String]
    for
      target <- targets
      source <- target.sources
      uri = source.toUri.toString
      if claimed.add(uri)
    do uriToTarget += uri -> target.bspId

    BspProjectModel(targets, uriToTarget.result())

  private[bsp] def isScala3(version: String): Boolean =
    version != null && (version == "3" || version.startsWith("3."))

  /** BuildTarget.data survives the jsonrpc round-trip as a gson JsonElement;
    * parse it back into ScalaBuildTarget. A parse only counts when it
    * carries a scalaVersion, so unrelated data kinds do not slip through.
    */
  private[bsp] def parseScalaBuildTarget(data: Object): Option[ScalaBuildTarget] =
    val parsed = data match
      case null => None
      case sbt: ScalaBuildTarget => Some(sbt)
      case element: JsonElement =>
        Try(gson.fromJson(element, classOf[ScalaBuildTarget])).toOption.flatMap(Option(_))
      case other =>
        Try(gson.fromJson(gson.toJson(other), classOf[ScalaBuildTarget])).toOption.flatMap(Option(_))
    parsed.filter(_.getScalaVersion != null)

  /** FILE items are kept when they are Scala sources; DIRECTORY items are
    * expanded by walking every *.scala file under them. Result is
    * deduplicated and sorted for determinism.
    */
  private[bsp] def expandSources(items: Vector[SourceItem]): Vector[Path] =
    val out = Vector.newBuilder[Path]
    for item <- items do
      val path = pathOfUri("buildTarget/sources", item.getUri)
      if item.getKind == SourceItemKind.DIRECTORY then
        if Files.isDirectory(path) then
          Using.resource(Files.walk(path)) { stream =>
            stream
              .iterator()
              .asScala
              .foreach(p => if Files.isRegularFile(p) && isScalaFile(p) then out += p)
          }
      else if isScalaFile(path) then out += path
    out.result().distinct.sortBy(_.toString)

  private def isScalaFile(path: Path): Boolean =
    path.getFileName != null && path.getFileName.toString.endsWith(".scala")

  private def pathOfUri(method: String, uri: String): Path =
    try Path.of(URI.create(uri))
    catch
      case e: IllegalArgumentException =>
        throw BspException(BspError.InvalidResponse(method, s"bad file uri '$uri': ${e.getMessage}"))
      case e: java.nio.file.FileSystemNotFoundException =>
        throw BspException(BspError.InvalidResponse(method, s"bad file uri '$uri': ${e.getMessage}"))
