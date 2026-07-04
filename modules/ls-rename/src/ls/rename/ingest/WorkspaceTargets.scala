package ls.rename.ingest

import java.nio.file.Path

import scala.collection.mutable

import ls.bsp.BspProjectModel
import ls.semanticdb.DocFacts

/** One indexable build target as the ingest pipeline sees it.
  *
  * `semanticdbRoot` is the SemanticDB *targetroot* (the locator appends
  * `META-INF/semanticdb` itself); `sourceroot` is the root that
  * `TextDocument.uri` values are relative to. `docFacts` supplies the
  * per-document generated/readonly/dependency-source knowledge, keyed by the
  * SemanticDB uri (sourceroot-relative, forward slashes).
  */
final case class TargetSpec(
    bspId: String,
    semanticdbRoot: Path,
    sourceroot: Path,
    directDeps: Vector[String] = Vector.empty,
    scalaVersion: String = "3",
    classpathHash: String = "",
    optionsHash: String = "",
    docFacts: String => DocFacts = _ => DocFacts.workspaceSource
)

/** The workspace description consumed by [[IngestPipeline]]: the indexable
  * targets in a deterministic order plus the dependency edges between them
  * (`directDeps`, restricted to targets present here).
  */
final case class WorkspaceTargets(targets: Vector[TargetSpec]):
  require(
    targets.map(_.bspId).distinct.length == targets.length,
    "duplicate bspId in workspace targets"
  )

  private lazy val byId: Map[String, TargetSpec] =
    targets.iterator.map(t => t.bspId -> t).toMap

  /** Reverse dependency edges: dep -> targets that directly depend on it. */
  private lazy val reverseEdges: Map[String, Vector[String]] =
    val acc = mutable.Map.empty[String, Vector[String]]
    for
      t <- targets
      dep <- t.directDeps.distinct
      if byId.contains(dep)
    do acc.updateWith(dep)(prev => Some(prev.getOrElse(Vector.empty) :+ t.bspId))
    acc.view.mapValues(_.sorted).toMap

  def specOf(bspId: String): Option[TargetSpec] = byId.get(bspId)

  def dependentsOf(bspId: String): Vector[String] =
    reverseEdges.getOrElse(bspId, Vector.empty)

  /** `bspId` plus every target that transitively depends on it: the exact
    * upper bound of targets that can reference a symbol defined in `bspId`
    * (plan 12.2). Empty for unknown ids.
    */
  def reverseDependencyClosure(bspId: String): Set[String] =
    if !byId.contains(bspId) then Set.empty
    else
      val seen = mutable.Set(bspId)
      val queue = mutable.Queue(bspId)
      while queue.nonEmpty do
        val current = queue.dequeue()
        for dependent <- dependentsOf(current) if seen.add(dependent) do
          queue.enqueue(dependent)
      seen.toSet

  /** `bspId` plus every target it transitively depends on (via `directDeps`):
    * the exact set of targets a source in `bspId` can SEE, i.e. where its
    * cross-file go-to definitions may legitimately live. Restricting PC
    * definition results to this set stops a disconnected target that happens to
    * reuse the same symbol name from leaking an unrelated declaration. Empty for
    * unknown ids.
    */
  def forwardDependencyClosure(bspId: String): Set[String] =
    if !byId.contains(bspId) then Set.empty
    else
      val seen = mutable.Set(bspId)
      val queue = mutable.Queue(bspId)
      while queue.nonEmpty do
        val current = queue.dequeue()
        for dep <- byId.get(current).map(_.directDeps).getOrElse(Vector.empty) if byId.contains(dep) && seen.add(dep) do
          queue.enqueue(dep)
      seen.toSet

  /** DocFacts for `uri` in target `bspId`; workspace-source default when the
    * target is unknown.
    */
  def factsFor(bspId: String, uri: String): DocFacts =
    byId.get(bspId).fold(DocFacts.workspaceSource)(_.docFacts(uri))

object WorkspaceTargets:
  val empty: WorkspaceTargets = WorkspaceTargets(Vector.empty)

  /** Builds the workspace description from a BSP project model: every
    * indexable target (SemanticDB root and sourceroot both known) with its
    * dependency edges. `docFacts` lets the caller inject build knowledge
    * about generated/readonly/dependency sources (default: everything is a
    * plain editable workspace source).
    */
  def fromBsp(
      model: BspProjectModel,
      docFacts: (String, String) => DocFacts = (_, _) => DocFacts.workspaceSource
  ): WorkspaceTargets =
    val specs =
      for
        t <- model.indexableTargets
        sdbRoot <- t.semanticdbRoot
        srcRoot <- t.sourceroot
      yield TargetSpec(
        bspId = t.bspId,
        semanticdbRoot = sdbRoot,
        sourceroot = srcRoot,
        directDeps = t.directDeps,
        scalaVersion = t.scalaVersion,
        docFacts = uri => docFacts(t.bspId, uri)
      )
    WorkspaceTargets(specs)
