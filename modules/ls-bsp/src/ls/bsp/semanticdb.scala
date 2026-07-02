package ls.bsp

import java.nio.file.Path

/** SemanticDB output configuration derived from a target's scalac options.
  * `semanticdbRoot` is the targetroot that will contain
  * `META-INF/semanticdb` output files; None when the target does not
  * generate SemanticDB at all.
  */
final case class SemanticdbConfig(semanticdbRoot: Option[Path], sourceroot: Path):
  def enabled: Boolean = semanticdbRoot.isDefined

/** Scala 3 SemanticDB flag extraction (plan section 4.2).
  *
  *   - `-Xsemanticdb` or `-Ysemanticdb` enables SemanticDB generation.
  *   - `-semanticdb-target:<path>` (or the two-token form
  *     `-semanticdb-target <path>`) overrides the targetroot; otherwise the
  *     targetroot is the class directory.
  *   - `-sourceroot:<path>` (or two-token form) sets the sourceroot;
  *     otherwise the workspace root is the sourceroot.
  *
  * Like scalac, the last occurrence of a flag wins and relative paths are
  * resolved against the workspace root.
  */
object SemanticdbFlags:
  private val EnableFlags = Set("-Xsemanticdb", "-Ysemanticdb")
  private val TargetFlag = "-semanticdb-target"
  private val SourcerootFlag = "-sourceroot"

  def extract(options: Vector[String], classDirectory: Path, workspaceRoot: Path): SemanticdbConfig =
    val enabled = options.exists(EnableFlags.contains)
    def resolve(value: String): Path = workspaceRoot.resolve(value).normalize()
    val targetroot =
      if enabled then Some(lastValue(options, TargetFlag).map(resolve).getOrElse(classDirectory))
      else None
    val sourceroot = lastValue(options, SourcerootFlag).map(resolve).getOrElse(workspaceRoot)
    SemanticdbConfig(targetroot, sourceroot)

  /** Last-wins scan over both `-flag:value` and `-flag value` spellings. */
  private def lastValue(options: Vector[String], flag: String): Option[String] =
    val colonPrefix = flag + ":"
    var result: Option[String] = None
    var i = 0
    while i < options.length do
      val opt = options(i)
      if opt.startsWith(colonPrefix) then
        val value = opt.substring(colonPrefix.length)
        if value.nonEmpty then result = Some(value)
      else if opt == flag && i + 1 < options.length then
        // Two-token form consumes the next argument, mirroring scalac.
        result = Some(options(i + 1))
        i += 1
      i += 1
    result
