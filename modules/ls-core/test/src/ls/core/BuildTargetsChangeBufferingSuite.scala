package ls.core

import java.io.{PipedInputStream, PipedOutputStream}
import java.util.concurrent.{Executors, TimeUnit}

import scala.concurrent.duration.{Duration, DurationInt}
import scala.jdk.CollectionConverters.*

import ch.epfl.scala.bsp4j.BuildClient
import org.eclipse.lsp4j.jsonrpc.Launcher
import org.eclipse.lsp4j.{InitializeParams, InitializedParams}

import ls.bsp.{BspSession, BspSessionConfig}

/** A `buildTarget/didChange` arriving before bootstrap publishes Ready must be
  * buffered and then APPLIED after Ready — a real model refetch and re-ingest,
  * not merely a flag flip.
  */
class BuildTargetsChangeBufferingSuite extends munit.FunSuite:

  override def munitTimeout: Duration = 600.seconds

  private def eventually(clue: String, timeoutMs: Long = 60000)(cond: => Boolean): Unit =
    val deadline = System.currentTimeMillis() + timeoutMs
    while !cond && System.currentTimeMillis() < deadline do Thread.sleep(25)
    assert(cond, s"condition not reached within ${timeoutMs}ms: $clue")

  test("a pre-ready buildTarget/didChange is buffered and applied (refetch + re-ingest) after Ready"):
    val ws = E2eFixture.freshCopy()
    val fake = new ls.bsp.FakeBuildServer(
      ws.root,
      ws.aSourceDir,
      ws.bSourceFile,
      ws.cSourceFile,
      ws.semanticdbOverride,
      advertiseInverseSources = true
    )
    val bspServer = new ClasspathAugmentingServer(
      fake,
      {
        case "a" => E2eFixture.libraryClasspath
        case "b" => E2eFixture.libraryClasspath :+ ws.classDirOf("a")
        case _ => E2eFixture.libraryClasspath
      }
    )
    val executor = Executors.newCachedThreadPool { (r: Runnable) =>
      val t = new Thread(r, "buffer-fake-bsp"); t.setDaemon(true); t
    }
    val bspToClient = new PipedInputStream(1 << 20)
    val bspServerOut = new PipedOutputStream(bspToClient)
    val bspToServer = new PipedInputStream(1 << 20)
    val bspClientOut = new PipedOutputStream(bspToServer)
    val bspLauncher = new Launcher.Builder[BuildClient]()
      .setLocalService(bspServer)
      .setRemoteInterface(classOf[BuildClient])
      .setInput(bspToServer)
      .setOutput(bspServerOut)
      .setExecutorService(executor)
      .create()
    fake.client = bspLauncher.getRemoteProxy
    bspLauncher.startListening()

    val server = new ScalaLs(
      ScalaLs.Config(
        bootstrap = Bootstrap.Config(
          connectBsp = (root, handlers) =>
            Some(
              BspSession.connect(root, bspToClient, bspClientOut, handlers, BspSessionConfig(requestTimeout = 60.seconds))
            ),
          pcRequestTimeoutMillis = 120000L,
          log = _ => ()
        ),
        debounceMillis = 100L,
        exitProcessOnExit = false
      )
    )
    try
      // Fire the change BEFORE `initialized`: state is NotReady, so it must buffer.
      server.onBuildTargetsChangedForTest()
      assert(server.pendingModelReloadForTest, "a pre-ready change must be buffered, not dropped")

      val init = new InitializeParams()
      init.setRootUri(Uris.toUri(ws.root))
      server.initialize(init).get(60, TimeUnit.SECONDS)
      server.initialized(new InitializedParams())
      assert(server.awaitBootstrap(180000L), "bootstrap did not finish")

      // After Ready, the buffered change is drained: the model is refetched
      // (a second workspaceBuildTargets call beyond the bootstrap load) and a
      // background re-ingest completes.
      eventually("buffered change refetched the build targets")(fake.workspaceBuildTargetsCalls.get >= 2)
      eventually("buffered change triggered a completed re-ingest")(server.completedIngests >= 1)
      eventually("pending flag drained")(!server.pendingModelReloadForTest)
    finally
      try server.shutdown().get(30, TimeUnit.SECONDS)
      catch case _: Exception => ()
      executor.shutdownNow()

  test("a build-targets change while shutting down is ignored, not buffered"):
    val server = new ScalaLs(ScalaLs.Config(exitProcessOnExit = false))
    server.shutdown().get(10, TimeUnit.SECONDS)
    server.onBuildTargetsChangedForTest()
    assert(!server.pendingModelReloadForTest, "a change during shutdown must not be buffered")
