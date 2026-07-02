package ls.pc

import java.nio.file.{Files, Path, Paths}
import java.util.jar.{JarEntry, JarOutputStream}

class CompilerPluginConfigSuite extends munit.FunSuite:

  private val tempDir = FunFixture[Path](
    setup = _ => Files.createTempDirectory("ls-pc-cfg"),
    teardown = _ => ()
  )

  private def emptyJar(dir: Path, name: String): Path =
    val jar = dir.resolve(name)
    val out = new JarOutputStream(Files.newOutputStream(jar))
    out.putNextEntry(new JarEntry("marker.txt"))
    out.write("test".getBytes)
    out.closeEntry()
    out.close()
    jar

  private def newManager(dir: Path) =
    new PcPluginManager(PcPluginInitContext(None, dir.resolve("gen")))

  test("CompilerPluginSpec.toOptions maps jars to -Xplugin and options to -P (pure)"):
    val spec = CompilerPluginSpec(
      jars = Vector(Paths.get("/a/one.jar"), Paths.get("/b/two.jar")),
      options = Vector("myPlugin:verbose:true", "myPlugin:mode:fast")
    )
    assertEquals(
      spec.toOptions,
      Vector(
        "-Xplugin:/a/one.jar",
        "-Xplugin:/b/two.jar",
        "-P:myPlugin:verbose:true",
        "-P:myPlugin:mode:fast"
      )
    )
    // no filesystem check: nonexistent paths still map (validation happens in the manager)
    assertEquals(
      PcCompilerPluginConfig(Vector(spec)).toOptions.count(_.startsWith("-Xplugin:")),
      2
    )

  tempDir.test("config file JSON round-trips through the gson loader"): dir =>
    val config = PcPluginConfig(
      PcCompilerPluginConfig(
        Vector(
          CompilerPluginSpec(Vector(Paths.get("/plugins/a.jar")), Vector("a:opt1")),
          CompilerPluginSpec(Vector(Paths.get("/plugins/b.jar"), Paths.get("/plugins/b2.jar")), Vector.empty)
        )
      ),
      servicePluginJars = Vector(Paths.get("/plugins/svc.jar"))
    )
    val json = PcPluginConfigLoader.toJson(config)
    assertEquals(PcPluginConfigLoader.parse(json), config)
    // and through a real file
    val file = dir.resolve("pc-plugins.json")
    PcPluginConfigLoader.write(config, file)
    assertEquals(PcPluginConfigLoader.load(file), config)

  test("parse is lenient about missing fields and rejects non-objects"):
    assertEquals(PcPluginConfigLoader.parse("{}"), PcPluginConfig.empty)
    assertEquals(
      PcPluginConfigLoader.parse("""{"compilerPlugins": []}"""),
      PcPluginConfig.empty
    )
    intercept[IllegalArgumentException](PcPluginConfigLoader.parse("[1,2]"))

  tempDir.test("existing jar: -Xplugin/-P args materialize in manager options"): dir =>
    val jar = emptyJar(dir, "dummy-plugin.jar")
    val manager = newManager(dir)
    manager.setCompilerPluginConfig(
      PcCompilerPluginConfig(Vector(CompilerPluginSpec(Vector(jar), Vector("dummy:enabled:true"))))
    )
    assertEquals(
      manager.compilerPluginOptions,
      Vector(s"-Xplugin:$jar", "-P:dummy:enabled:true")
    )
    val report = manager.statusReport
    assertEquals(report.compilerPlugins.map(_.loaded), Vector(true))
    assert(report.disabled.isEmpty)

  tempDir.test("missing jar: recorded as failed self-test, contributes no options"): dir =>
    val jar = emptyJar(dir, "real.jar")
    val missing = dir.resolve("nope.jar")
    val manager = newManager(dir)
    manager.setCompilerPluginConfig(
      PcCompilerPluginConfig(
        Vector(
          CompilerPluginSpec(Vector(jar), Vector("real:on")),
          CompilerPluginSpec(Vector(missing), Vector("gone:on"))
        )
      )
    )
    // only the valid spec contributes options
    assertEquals(manager.compilerPluginOptions, Vector(s"-Xplugin:$jar", "-P:real:on"))
    val report = manager.statusReport
    assertEquals(report.compilerPlugins.map(_.loaded), Vector(true, false))
    val failed = report.compilerPlugins.find(!_.loaded).get
    assert(failed.detail.contains("missing jar"), failed.detail)
    assert(report.disabled.exists(_.reason.contains("missing jar")))

  tempDir.test("full pc-plugins.json config applies: compiler opts + missing service jar failure"): dir =>
    val jar = emptyJar(dir, "cp.jar")
    val missingService = dir.resolve("service-nope.jar")
    val file = dir.resolve("pc-plugins.json")
    PcPluginConfigLoader.write(
      PcPluginConfig(
        PcCompilerPluginConfig(Vector(CompilerPluginSpec(Vector(jar), Vector("cp:x")))),
        Vector(missingService)
      ),
      file
    )
    val manager = newManager(dir)
    manager.applyConfig(PcPluginConfigLoader.load(file))
    assertEquals(manager.compilerPluginOptions, Vector(s"-Xplugin:$jar", "-P:cp:x"))
    val report = manager.statusReport
    val svcFailure = report.servicePlugins.find(_.id == missingService.toString)
    assert(svcFailure.isDefined)
    assert(!svcFailure.get.selfTestOk)
    assert(svcFailure.get.selfTestDetail.contains("does not exist"))
    assert(report.disabled.exists(_.id == missingService.toString))
