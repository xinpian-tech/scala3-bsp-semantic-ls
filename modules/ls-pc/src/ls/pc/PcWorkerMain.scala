package ls.pc

import java.io.PrintStream
import java.nio.file.{Files, Path, Paths}
import java.util.concurrent.Executors

import scala.util.control.NonFatal

import org.eclipse.lsp4j.jsonrpc.Launcher

/** Standalone PC worker JVM entry point (plan 5.2). Serves [[PcWorkerApi]]
  * over stdin/stdout with the lsp4j jsonrpc launcher.
  *
  * `System.out` is re-pointed at stderr before anything else runs so stray
  * prints (from the compiler or user plugins) can never corrupt the protocol
  * stream; the real stdout is kept exclusively for JSON-RPC.
  *
  * Args:
  * {{{
  *   --workspace <dir>          workspace root (optional)
  *   --generated-sources <dir>  synthetic-source dir (optional)
  *   --plugin-config <file>     pc-plugins.json (optional)
  *   --max-instances <n>        live PC instance cap (default 4)
  *   --timeout-ms <n>           per-request budget in ms (default 15000)
  * }}}
  */
object PcWorkerMain:

  def main(args: Array[String]): Unit =
    val protocolOut = System.out
    System.setOut(new PrintStream(new java.io.FileOutputStream(java.io.FileDescriptor.err), true))

    val opts = parseArgs(args)
    val workspaceRoot = opts.get("workspace").map(Paths.get(_))
    val generatedSources = opts
      .get("generated-sources")
      .map(Paths.get(_))
      .orElse(workspaceRoot.map(r =>
        r.resolve(".scala3-bsp-semantic-ls").resolve("pc").resolve("generated-sources")
      ))
      .getOrElse(Files.createTempDirectory("ls-pc-generated-sources"))

    val settings = PcSettings(
      workspaceRoot = workspaceRoot,
      generatedSourcesRoot = generatedSources,
      maxLiveInstances = opts.get("max-instances").map(_.toInt).getOrElse(4),
      requestTimeoutMillis = opts.get("timeout-ms").map(_.toLong).getOrElse(15000L)
    )

    val pluginManager = new PcPluginManager(
      PcPluginInitContext(workspaceRoot, generatedSources, msg => System.err.println(s"[pc-plugin] $msg"))
    )
    opts.get("plugin-config").foreach { cfgPath =>
      try pluginManager.applyConfig(PcPluginConfigLoader.load(Paths.get(cfgPath)))
      catch
        case NonFatal(t) =>
          System.err.println(s"[pc-worker] failed to load plugin config $cfgPath: $t")
    }

    val facade = new PcFacade(pluginManager, settings)
    val exitRunnable: Runnable = () =>
      try Thread.sleep(200)
      catch case _: InterruptedException => ()
      System.exit(0)
    val worker = new InProcessPcWorker(
      facade,
      onShutdown = () =>
        // reply first, then exit: give the response a moment to flush
        val t = new Thread(exitRunnable, "ls-pc-worker-exit")
        t.setDaemon(true)
        t.start()
    )

    val executor = Executors.newCachedThreadPool { r =>
      val t = new Thread(r, "ls-pc-worker-jsonrpc")
      t.setDaemon(true)
      t
    }
    val launcher = new Launcher.Builder[PcWorkerClient]()
      .setLocalService(worker)
      .setRemoteInterface(classOf[PcWorkerClient])
      .setInput(System.in)
      .setOutput(protocolOut)
      .setExecutorService(executor)
      .create()

    // Blocks until the parent closes our stdin (or shutdown exits the JVM).
    try launcher.startListening().get()
    catch case NonFatal(_) => ()
    facade.shutdown()

  private def parseArgs(args: Array[String]): Map[String, String] =
    val out = Map.newBuilder[String, String]
    var i = 0
    while i < args.length do
      val a = args(i)
      if a.startsWith("--") && i + 1 < args.length then
        out += (a.stripPrefix("--") -> args(i + 1))
        i += 2
      else i += 1
    out.result()
