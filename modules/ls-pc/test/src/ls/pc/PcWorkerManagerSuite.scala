package ls.pc

import java.nio.file.Files

import scala.concurrent.duration.*

/** Instance lifecycle tests. `getOrCreate` without issuing requests is cheap
  * (the compiler driver is created lazily), so LRU/eviction tests do not pay
  * the compile cost.
  */
class PcWorkerManagerSuite extends munit.FunSuite:
  override def munitTimeout: Duration = 2.minutes

  private def newManager(maxInstances: Int): PcWorkerManager =
    val gen = Files.createTempDirectory("ls-pc-mgr-gen")
    new PcWorkerManager(
      new PcPluginManager(PcPluginInitContext(None, gen)),
      PcSettings(None, gen, maxLiveInstances = maxInstances, requestTimeoutMillis = 60000)
    )

  private def config(id: String, options: Vector[String] = Vector.empty): PcTargetConfig =
    PcTargetConfig(id, SharedPc.libraryClasspath, options)

  test("getOrCreate caches per target and is LRU-capped with eviction shutdown"):
    val manager = newManager(maxInstances = 2)
    val a = manager.getOrCreate(config("t-a"))
    val b = manager.getOrCreate(config("t-b"))
    assert(manager.getOrCreate(config("t-a")) eq a) // cached
    // touching t-a made t-b least recently used; adding t-c evicts t-b
    val c = manager.getOrCreate(config("t-c"))
    assertEquals(manager.activeTargets.toSet, Set("t-a", "t-c"))
    assert(manager.getOrCreate(config("t-b")) ne b) // recreated after eviction
    manager.shutdownAll()

  test("a changed config recreates the instance"):
    val manager = newManager(maxInstances = 4)
    val v1 = manager.getOrCreate(config("t-x"))
    val v2 = manager.getOrCreate(config("t-x", Vector("-deprecation")))
    assert(v1 ne v2)
    assert(v2.effectiveOptions.contains("-deprecation"))
    assertEquals(manager.activeTargets, Vector("t-x"))
    manager.shutdownAll()

  test("invalidate/restartTarget dispose the instance; shutdownAll rejects further use"):
    val manager = newManager(maxInstances = 4)
    manager.getOrCreate(config("t-y"))
    assert(manager.invalidate("t-y"))
    assert(!manager.invalidate("t-y"))
    manager.getOrCreate(config("t-y"))
    assert(manager.restartTarget("t-y"))
    assertEquals(manager.activeTargets, Vector.empty)
    manager.shutdownAll()
    intercept[IllegalArgumentException](manager.getOrCreate(config("t-z")))

  test("compiler-plugin config options are appended to instance options before service-plugin patching"):
    val gen = Files.createTempDirectory("ls-pc-mgr-cp")
    val jar = gen.resolve("dummy.jar")
    Files.write(jar, Array[Byte](0x50, 0x4b, 0x05, 0x06) ++ new Array[Byte](18)) // empty zip
    val pluginManager = new PcPluginManager(PcPluginInitContext(None, gen))
    pluginManager.setCompilerPluginConfig(
      PcCompilerPluginConfig(Vector(CompilerPluginSpec(Vector(jar), Vector("dummy:on"))))
    )
    pluginManager.register(new PcServicePlugin {
      def id = "sees-compiler-plugin-options"
      override def patchOptions(ctx: PcTargetContext, options: Vector[String]) =
        // service plugins run after compiler-plugin injection and can see it
        assert(options.contains(s"-Xplugin:$jar"))
        options :+ "-feature"
    })
    val manager = new PcWorkerManager(
      pluginManager,
      PcSettings(None, gen, maxLiveInstances = 2, requestTimeoutMillis = 60000)
    )
    // no request is issued: instance creation with a bogus plugin jar is legal
    // because the driver is lazy; we assert the option plumbing only.
    val instance = manager.getOrCreate(config("t-cp"))
    assert(instance.effectiveOptions.contains(s"-Xplugin:$jar"), instance.effectiveOptions.toString)
    assert(instance.effectiveOptions.contains("-P:dummy:on"), instance.effectiveOptions.toString)
    assert(instance.effectiveOptions.contains("-feature"), instance.effectiveOptions.toString)
    manager.shutdownAll()
