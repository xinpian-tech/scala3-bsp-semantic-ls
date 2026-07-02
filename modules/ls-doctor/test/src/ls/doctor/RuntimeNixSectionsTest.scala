package ls.doctor

import java.nio.file.Files

import DoctorTestSupport.findRepoRoot

/** Runtime facts probed on the actual (forked, Java 25,
  * `--enable-native-access=ALL-UNNAMED`) test JVM, and Nix facts probed
  * against the real repository root.
  */
class RuntimeNixSectionsTest extends munit.FunSuite:

  test("RuntimeSection: java 25 and native access on the test JVM"):
    val r = RuntimeSection.gather()
    assert(r.javaVersion.startsWith("25"), s"expected a Java 25 runtime, got '${r.javaVersion}'")
    assert(
      r.nativeAccessEnabledFor.contains("ALL-UNNAMED"),
      s"test JVM forks with --enable-native-access=ALL-UNNAMED, got ${r.nativeAccessEnabledFor}"
    )
    assert(
      r.compactObjectHeaders == "enabled" || r.compactObjectHeaders == "disabled",
      s"UseCompactObjectHeaders must be readable on JDK 25, got '${r.compactObjectHeaders}'"
    )
    // no AOT cache is configured for the test JVM
    assert(r.aotCache.startsWith("loaded") || r.aotCache.startsWith("missing"), r.aotCache)

  test("NixSection: real repo root has flake + mill-ivy-fetcher input; lock reported from the filesystem"):
    val root = findRepoRoot().getOrElse(fail("could not locate the repo root (no flake.nix upwards of cwd)"))
    val n = NixSection.gather(root)
    assert(n.flakeDetected, s"flake.nix not detected at $root")
    assert(n.millIvyFetcherInput, "mill-ivy-fetcher input not found in flake.nix")
    assertEquals(n.ivyLockPath, "nix/ivy-lock.nix")
    // truthful filesystem report, whether or not the lock has been generated yet
    assertEquals(n.ivyLockExists, Files.isRegularFile(root.resolve("nix/ivy-lock.nix")))
    if !n.ivyLockExists then
      assert(n.lockStatus.startsWith("stale"), s"missing lock must be stale, got '${n.lockStatus}'")
      assert(n.lockStatus.contains("mif run"), n.lockStatus)
    else
      assert(
        n.lockStatus.startsWith("fresh") || n.lockStatus.startsWith("stale") ||
          n.lockStatus.startsWith("unknown"),
        n.lockStatus
      )

  test("NixSection: non-flake directory degrades without throwing"):
    val dir = DoctorTestSupport.tempRoot("noflake")
    val n = NixSection.gather(dir)
    assert(!n.flakeDetected)
    assert(!n.millIvyFetcherInput)
    assert(!n.ivyLockExists)
    assert(n.lockStatus.startsWith("unknown"), n.lockStatus)
