package ls.semanticdb

import java.nio.file.{Files, Path}
import scala.jdk.CollectionConverters.*

/** Locates `.semanticdb` files under one targetroot and maps between
  * source-relative paths (SemanticDB `TextDocument.uri` convention: relative
  * to the sourceroot, forward slashes) and their `.semanticdb` files.
  *
  * scalac layout: `<targetroot>/META-INF/semanticdb/<source-rel-path>.semanticdb`.
  */
final class SemanticdbLocator(val targetroot: Path):
  import SemanticdbLocator.Suffix

  val semanticdbRoot: Path = targetroot.resolve("META-INF").resolve("semanticdb")

  /** All `*.semanticdb` files under the targetroot, sorted for determinism.
    * Empty when the root does not exist (target without SemanticDB output).
    */
  def listSemanticdbFiles(): Vector[Path] =
    if !Files.isDirectory(semanticdbRoot) then Vector.empty
    else
      val stream = Files.walk(semanticdbRoot)
      try
        stream
          .iterator()
          .asScala
          .filter(p => p.getFileName.toString.endsWith(Suffix) && Files.isRegularFile(p))
          .toVector
          .sortBy(_.toString)
      finally stream.close()

  /** Expected `.semanticdb` file for a source-relative path such as
    * `src/main/scala/a/B.scala`.
    */
  def semanticdbFileFor(sourceRelativePath: String): Path =
    require(
      sourceRelativePath.nonEmpty && !sourceRelativePath.startsWith("/"),
      s"source path must be relative: $sourceRelativePath"
    )
    val resolved = semanticdbRoot.resolve(sourceRelativePath + Suffix).normalize()
    require(
      resolved.startsWith(semanticdbRoot.normalize()),
      s"source path escapes the semanticdb root: $sourceRelativePath"
    )
    resolved

  /** Inverse mapping: source-relative path (forward slashes) for a
    * `.semanticdb` file, or None when the file is not under this targetroot
    * or lacks the suffix.
    */
  def sourceRelativePathFor(semanticdbFile: Path): Option[String] =
    val abs = semanticdbFile.toAbsolutePath.normalize()
    val root = semanticdbRoot.toAbsolutePath.normalize()
    if !abs.startsWith(root) || abs == root then None
    else
      val rel = root.relativize(abs).toString.replace(java.io.File.separatorChar, '/')
      if rel.endsWith(Suffix) && rel.length > Suffix.length then
        Some(rel.dropRight(Suffix.length))
      else None

object SemanticdbLocator:
  final val Suffix = ".semanticdb"
