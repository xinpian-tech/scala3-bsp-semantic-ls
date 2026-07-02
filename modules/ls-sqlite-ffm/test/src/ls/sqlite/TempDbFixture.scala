package ls.sqlite

import java.nio.file.{Files, Path}
import java.util.Comparator

/** Fresh temp-dir database per test, deleted (including -wal/-shm) after. */
trait TempDbFixture:
  self: munit.FunSuite =>

  val tempDir: FunFixture[Path] = FunFixture[Path](
    setup = test =>
      Files.createTempDirectory(
        "ls-sqlite-" + test.name.replaceAll("[^A-Za-z0-9]+", "-").take(40)
      ),
    teardown = dir =>
      if Files.exists(dir) then
        Files
          .walk(dir)
          .sorted(Comparator.reverseOrder[Path]())
          .forEach(p => Files.deleteIfExists(p))
  )
