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
  *   --in-process-pc   run the presentation compiler in-process (default and
  *                     only mode in v1; accepted for forward compatibility)
  * }}}
  */
object Main:

  def main(args: Array[String]): Unit =
    if args.contains("--version") then
      println(s"${ScalaLs.ServerName} ${ScalaLs.ServerVersion}")
      return
    val unknown = args.filterNot(a => a == "--in-process-pc")
    if unknown.nonEmpty then
      System.err.println(s"warning: ignoring unknown arguments: ${unknown.mkString(" ")}")

    val protocolOut = System.out
    System.setOut(new PrintStream(new FileOutputStream(FileDescriptor.err), true))

    val server = new ScalaLs()
    val launcher = LSPLauncher.createServerLauncher(server, System.in, protocolOut)
    server.connect(launcher.getRemoteProxy)
    try
      launcher.startListening().get()
      ()
    catch case NonFatal(_) => ()
