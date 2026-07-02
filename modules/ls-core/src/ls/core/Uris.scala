package ls.core

import java.net.URI
import java.nio.file.{Files, Path}

import ls.rename.QueryOrchestrator

/** `file://` URI <-> absolute [[Path]] <-> SemanticDB uri conversions.
  *
  * SemanticDB uris are sourceroot-relative with forward slashes (the form
  * every lower module speaks); LSP speaks absolute `file://` URIs. This is
  * the single place where the two worlds meet (plan: the LSP core owns the
  * conversion). Linux-first but written against the `java.nio` URI/Path APIs
  * so Windows paths degrade gracefully rather than corrupting.
  */
object Uris:

  /** Absolute filesystem path -> `file://` URI string. */
  def toUri(path: Path): String = path.toUri.toString

  /** `file://` URI -> absolute path. Throws on non-file / malformed uris. */
  def toPath(uri: String): Path = Path.of(URI.create(uri))

  def isFileUri(uri: String): Boolean = uri.startsWith("file:")

  /** Canonical URI form used for map keys: round-trip through [[Path]] so
    * equivalent spellings (percent-encoding, `file:/` vs `file:///`, `..`
    * segments) collapse to one key. Non-file uris pass through unchanged.
    */
  def normalize(uri: String): String =
    try toUri(toPath(uri).toAbsolutePath.normalize)
    catch case _: Exception => uri

  /** SemanticDB uri of `absolute` under `sourceroot`, or None when the path
    * is not inside the sourceroot. Forward slashes always.
    */
  def sdbUri(sourceroot: Path, absolute: Path): Option[String] =
    val root = sourceroot.toAbsolutePath.normalize
    val abs = absolute.toAbsolutePath.normalize
    if abs.startsWith(root) && abs != root then
      Some(root.relativize(abs).toString.replace(java.io.File.separatorChar, '/'))
    else None

  /** Absolute path of a SemanticDB uri under a sourceroot. */
  def fromSdbUri(sourceroot: Path, sdbUri: String): Path =
    sourceroot.resolve(sdbUri).toAbsolutePath.normalize

/** Workspace-aware conversions between `file://` URIs and SemanticDB uris.
  *
  * `toSdbUri` prefers the deepest sourceroot containing the path and, among
  * ambiguous roots, one whose relative uri is actually known to the metadata
  * store. `toFileUri` asks the orchestrator (metadata truth) first and falls
  * back to probing the sourceroots for an existing file.
  */
final class WorkspaceUris(sourceroots: Vector[Path], orchestrator: QueryOrchestrator):

  private val roots: Vector[Path] =
    sourceroots.map(_.toAbsolutePath.normalize).distinct.sortBy(-_.getNameCount)

  def toSdbUri(fileUri: String): Option[String] =
    val path =
      try Uris.toPath(fileUri)
      catch case _: Exception => return None
    val candidates = roots.flatMap(root => Uris.sdbUri(root, path))
    candidates
      .find(u => orchestrator.primaryRowOf(u).isDefined)
      .orElse(candidates.headOption)

  def toFileUri(sdbUri: String): Option[String] =
    orchestrator
      .absoluteSourcePath(sdbUri)
      .map(p => Uris.toUri(p.toAbsolutePath.normalize))
      .orElse {
        roots.iterator
          .map(root => Uris.fromSdbUri(root, sdbUri))
          .find(Files.isRegularFile(_))
          .map(Uris.toUri)
      }
