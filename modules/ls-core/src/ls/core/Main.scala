package ls.core

import java.io.{FileDescriptor, FileOutputStream, PrintStream}

import scala.util.control.NonFatal

import org.eclipse.lsp4j.launch.LSPLauncher

/** stdio LSP entry point.
  *
  * `System.out` is re-pointed at stderr before anything else runs (the same
  * guard as [[ls.pc.PcWorkerMain]]): stray prints from the compiler, user
  * plugins or libraries must never corrupt the JSON-RPC stream; the real
  * stdout is reserved for the protocol.
  *
  * Flags:
  * {{{
  *   --version         print the version and exit
  *   --in-process-pc   run the presentation compiler in this JVM (default)
  *   --forked-pc       run the presentation compiler in an isolated child JVM
  *                     (plan 5.2 process isolation; opt-in for now)
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

  def main(args: Array[String]): Unit =
    if args.contains("--version") then
      println(s"${ScalaLs.ServerName} ${ScalaLs.ServerVersion}")
      return
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
