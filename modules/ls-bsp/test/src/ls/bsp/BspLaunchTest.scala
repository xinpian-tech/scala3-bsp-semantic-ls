package ls.bsp

import java.nio.file.Files
import java.util.concurrent.ConcurrentLinkedQueue

import scala.concurrent.duration.*

import ch.epfl.scala.bsp4j.BspConnectionDetails

/** Process-launch behavior against deliberately unresponsive commands: no
  * real build tool involved, only argv/cwd handling, request timeouts, and
  * bounded process termination.
  */
class BspLaunchTest extends munit.FunSuite:

  private def details(argv: String*): BspConnectionDetails =
    new BspConnectionDetails(
      "unresponsive",
      java.util.List.of(argv*),
      "1.0.0",
      "2.1.1",
      java.util.List.of("scala")
    )

  private val fastConfig =
    BspSessionConfig(requestTimeout = 400.millis, shutdownTimeout = 300.millis)

  test("requests against an unresponsive server time out with a typed error") {
    val ws = Files.createTempDirectory("ls-bsp-launch")
    val session = BspSession.launch(ws, details("sleep", "30"), config = fastConfig)
    try
      assertEquals(session.serverProcessAlive, Some(true))
      val ex = intercept[BspException](session.initialize())
      ex.error match
        case BspError.RequestTimeout(method, millis) =>
          assertEquals(method, "build/initialize")
          assertEquals(millis, 400L)
        case other => fail(s"expected RequestTimeout, got $other")
    finally session.shutdown()
    // graceful shutdown timed out, so the process must have been terminated
    assertEquals(session.serverProcessAlive, Some(false))
    assert(session.isClosed)
  }

  test("server stderr is forwarded to the handler") {
    val ws = Files.createTempDirectory("ls-bsp-launch")
    val stderrLines = new ConcurrentLinkedQueue[String]()
    val handlers = BspClientHandlers(onServerStderr = stderrLines.add(_))
    val session = BspSession.launch(
      ws,
      details("sh", "-c", "echo fake-stderr-line >&2; exec sleep 30"),
      handlers,
      fastConfig
    )
    try
      val deadline = System.currentTimeMillis() + 3000
      while stderrLines.isEmpty && System.currentTimeMillis() < deadline do Thread.sleep(10)
      assertEquals(stderrLines.peek(), "fake-stderr-line")
    finally session.shutdown()
  }

  test("launch with empty argv fails with LaunchFailed") {
    val ws = Files.createTempDirectory("ls-bsp-launch")
    val bad = new BspConnectionDetails(
      "empty-argv",
      java.util.List.of(),
      "1.0.0",
      "2.1.1",
      java.util.List.of("scala")
    )
    val ex = intercept[BspException](BspSession.launch(ws, bad, config = fastConfig))
    ex.error match
      case BspError.LaunchFailed(server, _) => assertEquals(server, "empty-argv")
      case other => fail(s"expected LaunchFailed, got $other")
  }

  test("launch with a nonexistent binary fails with LaunchFailed") {
    val ws = Files.createTempDirectory("ls-bsp-launch")
    val ex = intercept[BspException](
      BspSession.launch(ws, details("/nonexistent/bsp-server-binary"), config = fastConfig)
    )
    ex.error match
      case BspError.LaunchFailed(server, detail) =>
        assertEquals(server, "unresponsive")
        assert(detail.nonEmpty)
      case other => fail(s"expected LaunchFailed, got $other")
  }
