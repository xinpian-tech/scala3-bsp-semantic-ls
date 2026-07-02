package ls.doctor

import java.lang.management.ManagementFactory
import java.nio.file.{Files, Path}

import scala.jdk.CollectionConverters.*
import scala.util.control.NonFatal

/** Runtime section of the doctor report (plan section 19):
  *
  * {{{
  * Runtime:
  *   Java: 25.x
  *   Native access: enabled for ls.sqlite.ffm
  *   Compact Object Headers: enabled/disabled
  *   AOT cache: loaded/missing
  * }}}
  *
  * All fields are pre-rendered facts about the *current* JVM; gathering is
  * total and never throws (a failing probe becomes `unavailable: <reason>`).
  */
final case class RuntimeSection(
    javaVersion: String,
    /** Module names (or ALL-UNNAMED) native access is enabled for. */
    nativeAccessEnabledFor: Vector[String],
    /** "enabled" / "disabled" / "unavailable: <reason>". */
    compactObjectHeaders: String,
    /** "loaded (<path>)" / "missing (<why>)". */
    aotCache: String
)

object RuntimeSection:

  /** Gathers runtime facts from the current JVM. Total: every probe failure
    * degrades to an `unavailable: <reason>` field value.
    */
  def gather(): RuntimeSection =
    val inputArgs = jvmInputArguments()
    RuntimeSection(
      javaVersion = javaVersion(),
      nativeAccessEnabledFor = nativeAccessEnabledFor(inputArgs),
      compactObjectHeaders = compactObjectHeaders(),
      aotCache = aotCacheStatus(inputArgs)
    )

  private def javaVersion(): String =
    try Option(System.getProperty("java.version")).getOrElse(Runtime.version().toString)
    catch case NonFatal(t) => s"unavailable: ${Gather.describe(t)}"

  private def jvmInputArguments(): Vector[String] =
    try ManagementFactory.getRuntimeMXBean.getInputArguments.asScala.toVector
    catch case NonFatal(_) => Vector.empty

  /** Modules for which `--enable-native-access` is in effect: read from the
    * JVM input arguments (the java launcher keeps the flag visible there) and
    * from the `jdk.module.enable.native.access` system property when present.
    */
  private def nativeAccessEnabledFor(inputArgs: Vector[String]): Vector[String] =
    val flagPrefix = "--enable-native-access="
    val fromArgs = inputArgs
      .filter(_.startsWith(flagPrefix))
      .flatMap(_.stripPrefix(flagPrefix).split(',').toVector)
    val fromProp =
      try
        Option(System.getProperty("jdk.module.enable.native.access")).toVector
          .flatMap(_.split(',').toVector)
      catch case NonFatal(_) => Vector.empty
    (fromArgs ++ fromProp).map(_.trim).filter(_.nonEmpty).distinct

  /** Reads the UseCompactObjectHeaders VM option through
    * HotSpotDiagnosticMXBean; "unavailable" when the bean or the option does
    * not exist on this JVM.
    */
  private def compactObjectHeaders(): String =
    try
      val bean =
        ManagementFactory.getPlatformMXBean(classOf[com.sun.management.HotSpotDiagnosticMXBean])
      if bean == null then "unavailable: HotSpotDiagnosticMXBean is not present"
      else if bean.getVMOption("UseCompactObjectHeaders").getValue == "true" then "enabled"
      else "disabled"
    catch case NonFatal(t) => s"unavailable: ${Gather.describe(t)}"

  /** AOT cache status from the `-XX:AOTCache=<path>` input argument plus file
    * existence (the production wrapper is expected to pass the flag).
    */
  private def aotCacheStatus(inputArgs: Vector[String]): String =
    val prefix = "-XX:AOTCache="
    inputArgs.findLast(_.startsWith(prefix)) match
      case None => "missing (no -XX:AOTCache flag)"
      case Some(arg) =>
        val raw = arg.stripPrefix(prefix)
        try
          if Files.exists(Path.of(raw)) then s"loaded ($raw)"
          else s"missing ($raw does not exist)"
        catch case NonFatal(t) => s"missing ($raw: ${Gather.describe(t)})"

/** Nix section of the doctor report (plan section 19):
  *
  * {{{
  * Nix:
  *   flake detected: yes
  *   mill-ivy-fetcher input: yes
  *   ivy lock: nix/ivy-lock.nix
  *   lock status: fresh/stale
  * }}}
  *
  * `lockStatus` is `fresh`/`stale`/`unknown: <reason>`. The doctor never runs
  * `mif` (regenerating the lock is far too heavy for a diagnostics command):
  * a missing lock file is definitely stale, otherwise the doctor reports a
  * cheap mtime heuristic against build.mill when `mif` is runnable and
  * `unknown` when it is not. The *authoritative* staleness check is owned by
  * CI (`scripts/check-ivy-lock.sh`, plan 15.3/15.5).
  */
final case class NixSection(
    flakeDetected: Boolean,
    millIvyFetcherInput: Boolean,
    /** Workspace-relative lock path, always `nix/ivy-lock.nix`. */
    ivyLockPath: String,
    ivyLockExists: Boolean,
    /** "fresh (...)" / "stale (...)" / "unknown: ...". */
    lockStatus: String
)

object NixSection:
  val IvyLockRelPath = "nix/ivy-lock.nix"

  /** Gathers Nix workspace facts from the filesystem. Total. */
  def gather(workspaceRoot: Path): NixSection =
    val flakeFile = workspaceRoot.resolve("flake.nix")
    val flakeDetected =
      try Files.isRegularFile(flakeFile)
      catch case NonFatal(_) => false
    val fetcherInput =
      flakeDetected && {
        try Files.readString(flakeFile).contains("mill-ivy-fetcher")
        catch case NonFatal(_) => false
      }
    val lockFile = workspaceRoot.resolve(IvyLockRelPath)
    val lockExists =
      try Files.isRegularFile(lockFile)
      catch case NonFatal(_) => false
    NixSection(
      flakeDetected = flakeDetected,
      millIvyFetcherInput = fetcherInput,
      ivyLockPath = IvyLockRelPath,
      ivyLockExists = lockExists,
      lockStatus = lockStatus(workspaceRoot, flakeDetected, lockExists, lockFile)
    )

  private def lockStatus(
      workspaceRoot: Path,
      flakeDetected: Boolean,
      lockExists: Boolean,
      lockFile: Path
  ): String =
    try
      if !flakeDetected then s"unknown: no flake.nix under $workspaceRoot"
      else if !lockExists then
        s"stale ($IvyLockRelPath does not exist; run `mif run -p . -o $IvyLockRelPath`)"
      else if !mifRunnable() then
        "unknown: mif is not runnable from this process; " +
          "CI (scripts/check-ivy-lock.sh) owns the authoritative staleness check"
      else
        val buildMill = workspaceRoot.resolve("build.mill")
        if !Files.isRegularFile(buildMill) then "unknown: build.mill not found next to the lock"
        else
          val lockTime = Files.getLastModifiedTime(lockFile)
          val buildTime = Files.getLastModifiedTime(buildMill)
          if lockTime.compareTo(buildTime) >= 0 then
            s"fresh (heuristic: $IvyLockRelPath is not older than build.mill; authoritative check runs in CI)"
          else
            s"stale (build.mill modified after $IvyLockRelPath; run `mif run -p . -o $IvyLockRelPath`)"
    catch case NonFatal(t) => s"unknown: ${Gather.describe(t)}"

  /** Cheap runnability probe: an executable named `mif` on PATH. The doctor
    * deliberately never executes it.
    */
  private def mifRunnable(): Boolean =
    try
      sys.env
        .get("PATH")
        .toVector
        .flatMap(_.split(java.io.File.pathSeparatorChar).toVector)
        .filter(_.nonEmpty)
        .exists { dir =>
          val candidate = Path.of(dir).resolve("mif")
          Files.isRegularFile(candidate) && Files.isExecutable(candidate)
        }
    catch case NonFatal(_) => false
