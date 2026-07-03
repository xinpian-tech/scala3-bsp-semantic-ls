package ls.bsp

import java.io.PipedInputStream
import java.io.PipedOutputStream
import java.nio.file.Files
import java.nio.file.Path
import java.util.Comparator
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.Executors

import scala.concurrent.duration.*
import scala.jdk.CollectionConverters.*

import ch.epfl.scala.bsp4j.*
import org.eclipse.lsp4j.jsonrpc.Launcher

import ls.index.LsError

/** End-to-end tests against an in-process fake BSP server wired over piped
  * streams with the real lsp4j jsonrpc Launcher on both ends.
  */
class BspSessionTest extends munit.FunSuite:

  final class BspFixture(
      val workspaceRoot: Path,
      val fake: FakeBuildServer,
      val session: BspSession,
      val diagnostics: ConcurrentLinkedQueue[PublishDiagnosticsParams],
      val logs: ConcurrentLinkedQueue[LogMessageParams],
      val shows: ConcurrentLinkedQueue[ShowMessageParams],
      val didChanges: ConcurrentLinkedQueue[DidChangeBuildTarget]
  ):
    val aSourceDir: Path = workspaceRoot.resolve("a").resolve("src")
    val bSourceFile: Path = workspaceRoot.resolve("b").resolve("src").resolve("B.scala")
    val cSourceFile: Path = workspaceRoot.resolve("c").resolve("src").resolve("C.scala")
    def loadModel(): BspProjectModel = ProjectModelLoader.load(session)

  private def eventually(clue: String, timeoutMs: Long = 3000)(cond: => Boolean): Unit =
    val deadline = System.currentTimeMillis() + timeoutMs
    while !cond && System.currentTimeMillis() < deadline do Thread.sleep(10)
    assert(cond, s"condition not reached within ${timeoutMs}ms: $clue")

  private def deleteRecursively(root: Path): Unit =
    if Files.exists(root) then
      val stream = Files.walk(root)
      try stream.sorted(Comparator.reverseOrder()).forEach(p => Files.deleteIfExists(p))
      finally stream.close()

  private def withFixture[A](
      advertiseInverseSources: Boolean = true,
      advertiseDependencySources: Boolean = false,
      advertiseOutputPaths: Boolean = false
  )(body: BspFixture => A): A =
    val workspaceRoot = Files.createTempDirectory("ls-bsp-fake-ws")
    val serverExecutor = Executors.newCachedThreadPool { (r: Runnable) =>
      val t = new Thread(r, "fake-bsp-server")
      t.setDaemon(true)
      t
    }
    var session: BspSession = null
    try
      // Workspace sources: target a has a source *directory* (with a nested
      // subdir and a java file that must be ignored), b and c have files.
      val aSourceDir = workspaceRoot.resolve("a").resolve("src")
      Files.createDirectories(aSourceDir.resolve("nested"))
      Files.writeString(aSourceDir.resolve("A1.scala"), "class A1\n")
      Files.writeString(aSourceDir.resolve("A2.scala"), "class A2\n")
      Files.writeString(aSourceDir.resolve("nested").resolve("A3.scala"), "class A3\n")
      Files.writeString(aSourceDir.resolve("Ignored.java"), "class Ignored {}\n")
      val bSourceFile = workspaceRoot.resolve("b").resolve("src").resolve("B.scala")
      Files.createDirectories(bSourceFile.getParent)
      Files.writeString(bSourceFile, "class B\n")
      val cSourceFile = workspaceRoot.resolve("c").resolve("src").resolve("C.scala")
      Files.createDirectories(cSourceFile.getParent)
      Files.writeString(cSourceFile, "class C\n")
      val semanticdbOverride = workspaceRoot.resolve("out").resolve("a").resolve("semanticdb")

      val fake = new FakeBuildServer(
        workspaceRoot,
        aSourceDir,
        bSourceFile,
        cSourceFile,
        semanticdbOverride,
        advertiseInverseSources,
        advertiseDependencySources,
        advertiseOutputPaths
      )

      // client <- server pipe and client -> server pipe.
      val toClient = new PipedInputStream(1 << 20)
      val serverOut = new PipedOutputStream(toClient)
      val toServer = new PipedInputStream(1 << 20)
      val clientOut = new PipedOutputStream(toServer)

      val serverLauncher = new Launcher.Builder[BuildClient]()
        .setLocalService(fake)
        .setRemoteInterface(classOf[BuildClient])
        .setInput(toServer)
        .setOutput(serverOut)
        .setExecutorService(serverExecutor)
        .create()
      fake.client = serverLauncher.getRemoteProxy
      serverLauncher.startListening()

      val diagnostics = new ConcurrentLinkedQueue[PublishDiagnosticsParams]()
      val logs = new ConcurrentLinkedQueue[LogMessageParams]()
      val shows = new ConcurrentLinkedQueue[ShowMessageParams]()
      val didChanges = new ConcurrentLinkedQueue[DidChangeBuildTarget]()
      val handlers = BspClientHandlers(
        onDiagnostics = diagnostics.add(_),
        onLogMessage = logs.add(_),
        onShowMessage = shows.add(_),
        onDidChangeBuildTarget = didChanges.add(_)
      )

      session = BspSession.connect(
        workspaceRoot,
        toClient,
        clientOut,
        handlers,
        BspSessionConfig(requestTimeout = 10.seconds, shutdownTimeout = 2.seconds)
      )
      session.initialize()
      body(new BspFixture(workspaceRoot, fake, session, diagnostics, logs, shows, didChanges))
    finally
      if session != null then session.shutdown()
      serverExecutor.shutdownNow()
      deleteRecursively(workspaceRoot)

  test("initialize handshake exposes capabilities and notifies the server") {
    withFixture() { fx =>
      assert(fx.fake.initializeReceived.get())
      eventually("build/initialized received")(fx.fake.initializedNotified.get())
      val caps = fx.session.serverCapabilities
      assert(caps.isDefined)
      assertEquals(caps.get.getInverseSourcesProvider, java.lang.Boolean.TRUE)
      assertEquals(
        caps.get.getCompileProvider.getLanguageIds.asScala.toList,
        List("scala")
      )
    }
  }

  test("project model keeps only Scala 3 targets and assembles them sorted") {
    withFixture() { fx =>
      val model = fx.loadModel()
      assertEquals(
        model.targets.map(_.bspId),
        Vector(fx.fake.idOf("a"), fx.fake.idOf("b"), fx.fake.idOf("c"))
      )
      val a = model.targetFor(fx.fake.idOf("a")).get
      assertEquals(a.displayName, "a")
      assertEquals(a.scalaVersion, "3.8.4")
      assertEquals(a.classDirectory, fx.fake.classDirectoryOf("a"))
      assertEquals(a.directDeps, Vector.empty[String])
      val b = model.targetFor(fx.fake.idOf("b")).get
      assertEquals(b.directDeps, Vector(fx.fake.idOf("a")))
      val c = model.targetFor(fx.fake.idOf("c")).get
      assertEquals(c.directDeps, Vector(fx.fake.idOf("b")))
    }
  }

  test("semanticdb config: override, classDirectory default, disabled") {
    withFixture() { fx =>
      val model = fx.loadModel()
      val a = model.targetFor(fx.fake.idOf("a")).get
      // colon-form -semanticdb-target override
      assertEquals(a.semanticdbRoot, Some(fx.fake.semanticdbOverride))
      // two-token -sourceroot form
      assertEquals(a.sourceroot, Some(fx.workspaceRoot))
      val b = model.targetFor(fx.fake.idOf("b")).get
      // plain -Ysemanticdb: targetroot = classDirectory
      assertEquals(b.semanticdbRoot, Some(fx.fake.classDirectoryOf("b")))
      assertEquals(b.sourceroot, Some(fx.workspaceRoot))
      val c = model.targetFor(fx.fake.idOf("c")).get
      assertEquals(c.semanticdbRoot, None)
    }
  }

  test("unavailable target detection produces IndexUnavailable errors") {
    withFixture() { fx =>
      val model = fx.loadModel()
      assertEquals(model.indexableTargets.map(_.bspId), Vector(fx.fake.idOf("a"), fx.fake.idOf("b")))
      assertEquals(model.unavailableTargets.map(_.bspId), Vector(fx.fake.idOf("c")))
      model.unavailableErrors match
        case Vector(err @ LsError.IndexUnavailable(target)) =>
          assertEquals(target, fx.fake.idOf("c"))
          assert(err.message.contains(fx.fake.idOf("c")))
        case other => fail(s"expected one IndexUnavailable error, got $other")
    }
  }

  test("sources: directories expand to *.scala files, uriToTarget maps them") {
    withFixture() { fx =>
      val model = fx.loadModel()
      val a = model.targetFor(fx.fake.idOf("a")).get
      assertEquals(
        a.sources.map(_.getFileName.toString).sorted,
        Vector("A1.scala", "A2.scala", "A3.scala")
      )
      assert(a.sources.forall(p => Files.isRegularFile(p)))
      val b = model.targetFor(fx.fake.idOf("b")).get
      assertEquals(b.sources, Vector(fx.bSourceFile))

      val a1Uri = fx.aSourceDir.resolve("A1.scala").toUri.toString
      val a3Uri = fx.aSourceDir.resolve("nested").resolve("A3.scala").toUri.toString
      assertEquals(model.uriToTarget.get(a1Uri), Some(fx.fake.idOf("a")))
      assertEquals(model.uriToTarget.get(a3Uri), Some(fx.fake.idOf("a")))
      assertEquals(model.uriToTarget.get(fx.bSourceFile.toUri.toString), Some(fx.fake.idOf("b")))
      assertEquals(model.uriToTarget.get(fx.cSourceFile.toUri.toString), Some(fx.fake.idOf("c")))
      assertEquals(model.targetOfUri(a1Uri).map(_.bspId), Some(fx.fake.idOf("a")))
      // the ignored java file never enters the map
      val javaUri = fx.aSourceDir.resolve("Ignored.java").toUri.toString
      assertEquals(model.uriToTarget.get(javaUri), None)
    }
  }

  test("graph ops: dependenciesOf / dependentsOf / reverseDependencyClosure") {
    withFixture() { fx =>
      val model = fx.loadModel()
      val (a, b, c) = (fx.fake.idOf("a"), fx.fake.idOf("b"), fx.fake.idOf("c"))
      assertEquals(model.dependenciesOf(c), Vector(b))
      assertEquals(model.dependenciesOf(a), Vector.empty[String])
      assertEquals(model.dependentsOf(a), Vector(b))
      assertEquals(model.dependentsOf(c), Vector.empty[String])
      assertEquals(model.reverseDependencyClosure(a), Set(a, b, c))
      assertEquals(model.reverseDependencyClosure(b), Set(b, c))
      assertEquals(model.reverseDependencyClosure(c), Set(c))
    }
  }

  test("compile round-trip: Ok outcome, diagnostics and messages forwarded") {
    withFixture() { fx =>
      val outcome = fx.session.compile(Vector(fx.fake.idOf("a")), Some("origin-42"))
      assertEquals(outcome, BspCompileOutcome.Ok(Some("origin-42")))
      assert(outcome.isOk)
      // The fake emits notifications before answering the request, and the
      // client processes the stream in order, so they are already here.
      val diag = fx.diagnostics.peek()
      assert(diag != null, "expected a publishDiagnostics notification")
      assertEquals(diag.getTextDocument.getUri, fx.bSourceFile.toUri.toString)
      assertEquals(diag.getOriginId, "origin-42")
      assertEquals(diag.getDiagnostics.asScala.head.getMessage, "value unused in fake target")
      assert(fx.logs.asScala.exists(_.getMessage == "fake compile log"))
      assert(fx.shows.asScala.exists(_.getMessage == "fake compile show"))
      assert(fx.didChanges.asScala.nonEmpty)
    }
  }

  test("compile failure surfaces the statusCode") {
    withFixture() { fx =>
      val outcome = fx.session.compile(Vector(fx.fake.brokenId), Some("origin-err"))
      assertEquals(outcome, BspCompileOutcome.Failed(StatusCode.ERROR, Some("origin-err")))
      assert(!outcome.isOk)
    }
  }

  test("inverseSources uses the server when the capability is advertised") {
    withFixture() { fx =>
      val model = fx.loadModel()
      val uri = fx.bSourceFile.toUri.toString
      assertEquals(fx.session.inverseSources(uri, model), Vector(fx.fake.idOf("b")))
      assertEquals(fx.fake.inverseSourcesCalls.get(), 1)
    }
  }

  test("inverseSources falls back to uriToTarget without the capability") {
    withFixture(advertiseInverseSources = false) { fx =>
      val model = fx.loadModel()
      val uri = fx.cSourceFile.toUri.toString
      assertEquals(fx.session.inverseSources(uri, model), Vector(fx.fake.idOf("c")))
      assertEquals(fx.session.inverseSources("file:///nowhere/X.scala", model), Vector.empty[String])
      assertEquals(fx.fake.inverseSourcesCalls.get(), 0)
    }
  }

  test("dependencySources and outputPaths are attempted when advertised") {
    withFixture(advertiseDependencySources = true, advertiseOutputPaths = true) { fx =>
      val ids = Vector(fx.fake.idOf("a"), fx.fake.idOf("b"))
      val deps = fx.session.dependencySources(ids)
      assert(deps.isDefined, "dependencySources should be attempted when advertised")
      assertEquals(deps.get.length, 2)
      assertEquals(fx.fake.dependencySourcesCalls.get(), 1)
      val outputs = fx.session.outputPaths(ids)
      assert(outputs.isDefined, "outputPaths should be attempted when advertised")
      assertEquals(outputs.get.length, 2)
      assertEquals(fx.fake.outputPathsCalls.get(), 1)
    }
  }

  test("dependencySources and outputPaths are None (no crash) when not advertised") {
    withFixture() { fx =>
      val ids = Vector(fx.fake.idOf("a"))
      assertEquals(fx.session.dependencySources(ids), None)
      assertEquals(fx.session.outputPaths(ids), None)
      // The server is never called when the capability is absent.
      assertEquals(fx.fake.dependencySourcesCalls.get(), 0)
      assertEquals(fx.fake.outputPathsCalls.get(), 0)
      // Empty id sets are also None.
      assertEquals(fx.session.dependencySources(Vector.empty), None)
    }
  }

  test("shutdown is graceful and requests after close raise typed errors") {
    withFixture() { fx =>
      fx.session.shutdown()
      assert(fx.session.isClosed)
      assert(fx.fake.shutdownRequested.get())
      eventually("build/exit received")(fx.fake.exitReceived.get())
      val ex = intercept[BspException](fx.session.workspaceBuildTargets())
      ex.error match
        case BspError.SessionClosed(method) => assertEquals(method, "workspace/buildTargets")
        case other => fail(s"expected SessionClosed, got $other")
    }
  }
