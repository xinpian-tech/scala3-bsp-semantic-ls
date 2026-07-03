package ls.core

import java.nio.file.Files
import java.util.concurrent.TimeUnit

import org.eclipse.lsp4j.{InitializeParams, InitializedParams}

/** A `buildTarget/didChange` arriving before bootstrap publishes Ready must be
  * buffered and drained afterwards — never dropped, never a crash.
  */
class BuildTargetsChangeBufferingSuite extends munit.FunSuite:

  test("a build-targets change before bootstrap is buffered and drained after Ready"):
    val ws = Files.createTempDirectory("ls-buffer-")
    ws.toFile.deleteOnExit()
    val server = new ScalaLs(
      ScalaLs.Config(
        bootstrap = Bootstrap.Config(connectBsp = (_, _) => None, log = _ => ()),
        exitProcessOnExit = false
      )
    )
    try
      // Pre-bootstrap (state NotReady): the change is buffered, not dropped.
      server.onBuildTargetsChangedForTest()
      assert(server.pendingModelReloadForTest, "pre-ready change must be buffered")

      val init = new InitializeParams()
      init.setRootUri(Uris.toUri(ws))
      server.initialize(init).get(30, TimeUnit.SECONDS)
      server.initialized(new InitializedParams())
      assert(server.awaitBootstrap(60000L), "bootstrap did not finish")

      // Publishing Ready drains the buffered change.
      val deadline = System.currentTimeMillis() + 10000
      while server.pendingModelReloadForTest && System.currentTimeMillis() < deadline do Thread.sleep(20)
      assert(!server.pendingModelReloadForTest, "buffered change should be drained after Ready")
    finally
      server.shutdown().get(10, TimeUnit.SECONDS)

  test("a build-targets change while shutting down is ignored, not buffered"):
    val server = new ScalaLs(ScalaLs.Config(exitProcessOnExit = false))
    server.shutdown().get(10, TimeUnit.SECONDS)
    server.onBuildTargetsChangedForTest()
    assert(!server.pendingModelReloadForTest, "a change during shutdown must not be buffered")
