package ls.core

import java.nio.file.Files

/** Startup wiring: bootstrap runs the segment janitor after recovery, so a
  * store carrying orphan segment directories and writer debris from a previous
  * process is cleaned on the next boot.
  */
class BootstrapJanitorSuite extends munit.FunSuite:

  test("bootstrap removes orphan segment dirs and tmp debris when no active segment exists"):
    val ws = Files.createTempDirectory("ls-boot-janitor-")
    ws.toFile.deleteOnExit()
    val postings = Bootstrap.storageRootOf(ws).resolve("postings")
    val orphanSeg = postings.resolve("segments").resolve("segment-000001")
    val tmpDebris = postings.resolve("tmp-junk")
    Files.createDirectories(orphanSeg)
    Files.createDirectories(tmpDebris)

    val docs = new DocumentStore
    val overlay = new PcOverlay(docs)
    val state = Bootstrap.run(
      ws,
      Bootstrap.Config(connectBsp = (_, _) => None, log = _ => ()),
      docs,
      overlay
    )
    try
      assert(state.ready.isDefined, state.statusLine)
      assert(!Files.exists(orphanSeg), "orphan segment dir should be removed at startup")
      assert(!Files.exists(tmpDebris), "tmp debris should be removed at startup")
    finally state.ready.foreach(_.close())
