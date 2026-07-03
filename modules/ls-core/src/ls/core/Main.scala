package ls.core

import java.io.{FileDescriptor, FileOutputStream, PrintStream}
import java.nio.file.Path

import scala.util.control.NonFatal

import org.eclipse.lsp4j.launch.LSPLauncher

import ls.doctor.{Doctor, DoctorInput}

/** stdio LSP entry point.
  *
  * `System.out` is re-pointed at stderr before anything else runs (the same
  * guard as [[ls.pc.PcWorkerMain]]): stray prints from the compiler, user
  * plugins or libraries must never corrupt the JSON-RPC stream; the real
  * stdout is reserved for the protocol.
  *
  * Flags:
  * {{{
  *   --version             print the version and exit
  *   --doctor [<dir>]      print the offline doctor report and exit
  *   --aot-train <dir>     run the headless AOT training workload and exit
  *   --require-index       with --aot-train: require a real BSP-backed index
  *                         (compile + reindex, fail if the index stays empty)
  *   --in-process-pc       run the presentation compiler in this JVM (default)
  *   --forked-pc           run the presentation compiler in an isolated child JVM
  *                         (process isolation; opt-in for now)
  * }}}
  */
object Main:

  /** Resolve the PC backend mode from CLI flags. `--forked-pc` wins if both are
    * given; the default (and `--in-process-pc`) is in-process.
    */
  private[core] def pcBackendMode(args: Array[String]): PcBackendMode =
    if args.contains("--forked-pc") then PcBackendMode.Forked
    else PcBackendMode.InProcess

  private val knownFlags = Set("--in-process-pc", "--forked-pc")

  /** The value following `flag`, if present. */
  private def valueAfter(args: Array[String], flag: String): Option[String] =
    val i = args.indexOf(flag)
    if i >= 0 && i + 1 < args.length then Some(args(i + 1)) else None

  def main(args: Array[String]): Unit =
    if args.contains("--version") then
      println(s"${ScalaLs.ServerName} ${ScalaLs.ServerVersion}")
      return
    if args.contains("--doctor") then
      // Offline doctor: Runtime + Nix facts only (every subsystem section is
      // "unavailable: not connected"). The Runtime section reports the AOT
      // cache status from this JVM's own -XX:AOTCache flag.
      val root = valueAfter(args, "--doctor").map(Path.of(_)).getOrElse(Path.of(".")).toAbsolutePath.normalize
      println(Doctor.render(DoctorInput.offline(root)))
      return
    if args.contains("--aot-train") then
      valueAfter(args, "--aot-train") match
        case Some(dir) =>
          // --require-index forces the strict real-BSP workload (compile +
          // reindex + non-empty index queries); without it the run degrades
          // gracefully for a workspace that has no BSP connection. --skip-pc
          // skips the version-locked PC completion check (SemanticDB index
          // features stay asserted) — for a real repo on a mismatched compiler.
          sys.exit(
            AotTrain.run(
              Path.of(dir),
              requireIndex = args.contains("--require-index"),
              skipPc = args.contains("--skip-pc")
            )
          )
        case None =>
          System.err.println("--aot-train requires a workspace directory argument")
          sys.exit(2)
    val unknown = args.filterNot(knownFlags.contains)
    if unknown.nonEmpty then
      System.err.println(s"warning: ignoring unknown arguments: ${unknown.mkString(" ")}")

    val protocolOut = System.out
    System.setOut(new PrintStream(new FileOutputStream(FileDescriptor.err), true))

    val config = ScalaLs.Config(
      bootstrap = Bootstrap.Config(pcBackendMode = pcBackendMode(args))
    )
    val server = new ScalaLs(config)
    val launcher = LSPLauncher.createServerLauncher(server, System.in, protocolOut)
    server.connect(launcher.getRemoteProxy)
    try
      launcher.startListening().get()
      ()
    catch case NonFatal(_) => ()
