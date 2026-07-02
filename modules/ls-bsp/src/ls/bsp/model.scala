package ls.bsp

import java.nio.file.Path

import scala.collection.mutable

import ls.index.LsError

/** One Scala 3 build target as this LS sees it. `semanticdbRoot` is the
  * SemanticDB targetroot; None marks the target IndexUnavailable (no global
  * workspace-symbol / references / rename for it, plan section 4.2).
  */
final case class BspTarget(
    bspId: String,
    displayName: String,
    scalaVersion: String,
    scalacOptions: Vector[String],
    classDirectory: Path,
    semanticdbRoot: Option[Path],
    sourceroot: Option[Path],
    sources: Vector[Path],
    directDeps: Vector[String]
):
  def indexable: Boolean = semanticdbRoot.isDefined

/** The assembled BSP project model: Scala 3 targets, the source-file-uri ->
  * bspId map, and exact dependency-graph queries. `directDeps` may mention
  * targets that were filtered out (non-Scala-3); graph queries only traverse
  * targets present in the model.
  */
final case class BspProjectModel(
    targets: Vector[BspTarget],
    uriToTarget: Map[String, String]
):
  private lazy val byId: Map[String, BspTarget] =
    targets.iterator.map(t => t.bspId -> t).toMap

  private lazy val reverseEdges: Map[String, Vector[String]] =
    val acc = mutable.Map.empty[String, Vector[String]]
    for
      t <- targets
      dep <- t.directDeps.distinct
      if byId.contains(dep)
    do acc.updateWith(dep)(prev => Some(prev.getOrElse(Vector.empty) :+ t.bspId))
    acc.view.mapValues(_.sorted).toMap

  def targetFor(bspId: String): Option[BspTarget] = byId.get(bspId)

  def targetOfUri(uri: String): Option[BspTarget] =
    uriToTarget.get(uri).flatMap(byId.get)

  /** Direct dependencies restricted to targets known to the model, sorted. */
  def dependenciesOf(bspId: String): Vector[String] =
    byId.get(bspId) match
      case Some(t) => t.directDeps.filter(byId.contains).distinct.sorted
      case None => Vector.empty

  /** Direct dependents (targets that list `bspId` as a dependency), sorted. */
  def dependentsOf(bspId: String): Vector[String] =
    reverseEdges.getOrElse(bspId, Vector.empty)

  /** `bspId` plus every target that transitively depends on it: the exact
    * upper bound of targets that can reference a symbol defined in `bspId`.
    * Exact BFS over the reverse edges; empty for unknown ids.
    */
  def reverseDependencyClosure(bspId: String): Set[String] =
    if !byId.contains(bspId) then Set.empty
    else
      val seen = mutable.Set(bspId)
      val queue = mutable.Queue(bspId)
      while queue.nonEmpty do
        val current = queue.dequeue()
        for dependent <- dependentsOf(current) if seen.add(dependent) do queue.enqueue(dependent)
      seen.toSet

  /** Targets that produce SemanticDB and participate in the global index. */
  def indexableTargets: Vector[BspTarget] = targets.filter(_.indexable)

  /** Targets without SemanticDB output; global features are disabled there. */
  def unavailableTargets: Vector[BspTarget] = targets.filterNot(_.indexable)

  /** IndexUnavailable errors for every non-indexable target. */
  def unavailableErrors: Vector[LsError] =
    unavailableTargets.map(t => LsError.IndexUnavailable(t.bspId))
