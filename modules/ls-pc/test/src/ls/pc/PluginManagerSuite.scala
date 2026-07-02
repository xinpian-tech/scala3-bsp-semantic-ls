package ls.pc

import java.nio.file.{Files, Path}
import java.util.jar.{JarEntry, JarOutputStream}

import org.eclipse.lsp4j.{CompletionList, Diagnostic}

class PluginManagerSuite extends munit.FunSuite:

  private def newManager() =
    new PcPluginManager(PcPluginInitContext(None, Files.createTempDirectory("ls-pc-pm")))

  private def ctx = PcTargetContext("t", "3.8.4", Vector.empty, None)

  test("self-test: empty id is rejected and reported"):
    val manager = newManager()
    manager.register(new PcServicePlugin { def id = "" })
    assertEquals(manager.enabledPluginIds, Vector.empty)
    val report = manager.statusReport
    assert(report.servicePlugins.exists(s => !s.selfTestOk && s.selfTestDetail.contains("null or empty")))
    assert(report.disabled.nonEmpty)

  test("self-test: throwing initialize disables the plugin but registration survives"):
    val manager = newManager()
    manager.register(new PcServicePlugin {
      def id = "bad-init"
      override def initialize(ctx: PcPluginInitContext): Unit = throw new IllegalStateException("nope")
    })
    assertEquals(manager.enabledPluginIds, Vector.empty)
    val status = manager.statusReport.servicePlugins.find(_.id == "bad-init").get
    assert(!status.selfTestOk)
    assert(status.selfTestDetail.contains("initialize threw"))
    assert(manager.statusReport.disabled.exists(_.id == "bad-init"))
    assert(manager.disabledCause("bad-init").exists(_.isInstanceOf[IllegalStateException]))

  test("self-test: duplicate ids are rejected"):
    val manager = newManager()
    manager.register(new PcServicePlugin { def id = "dup" })
    manager.register(new PcServicePlugin { def id = "dup" })
    assertEquals(manager.enabledPluginIds, Vector("dup"))
    assert(manager.statusReport.disabled.exists(_.reason.contains("duplicate plugin id")))

  test("a hook that throws disables only that plugin; the chain proceeds as identity"):
    val manager = newManager()
    manager.register(new PcServicePlugin {
      def id = "adds-option"
      override def patchOptions(ctx: PcTargetContext, options: Vector[String]) = options :+ "-first"
    })
    manager.register(new PcServicePlugin {
      def id = "crashes"
      override def patchOptions(ctx: PcTargetContext, options: Vector[String]) =
        throw new RuntimeException("kaboom")
    })
    manager.register(new PcServicePlugin {
      def id = "adds-another"
      override def patchOptions(ctx: PcTargetContext, options: Vector[String]) = options :+ "-second"
    })
    val out = manager.patchOptions(ctx, Vector("-base"))
    assertEquals(out, Vector("-base", "-first", "-second"))
    assertEquals(manager.enabledPluginIds, Vector("adds-option", "adds-another"))
    assert(manager.statusReport.disabled.exists(d => d.id == "crashes" && d.reason.contains("patchOptions")))
    assert(manager.disabledCause("crashes").exists(_.getMessage == "kaboom"))
    // disabled plugin is skipped on later invocations, no re-throw
    assertEquals(manager.patchOptions(ctx, Vector.empty), Vector("-first", "-second"))

  test("a hook returning null is treated as identity"):
    val manager = newManager()
    manager.register(new PcServicePlugin {
      def id = "null-return"
      override def afterCompletion(req: PcRequest, result: CompletionList): CompletionList = null
    })
    val list = new CompletionList(false, new java.util.ArrayList())
    val req = PcRequest(PcRequestKind.Completion, "file:///x.scala", 0, 0, "t")
    assert(manager.afterCompletion(req, list) eq list)
    // returning null is not a crash: plugin stays enabled
    assertEquals(manager.enabledPluginIds, Vector("null-return"))

  test("filterPcDiagnostics pipeline filters and can crash-safely drop out"):
    val manager = newManager()
    manager.register(new PcServicePlugin {
      def id = "drop-all"
      override def filterPcDiagnostics(req: PcRequest, diagnostics: Vector[Diagnostic]) = Vector.empty
    })
    val req = PcRequest(PcRequestKind.Diagnostics, "file:///x.scala", 0, 0, "t")
    val diag = new Diagnostic()
    diag.setMessage("some error")
    assertEquals(manager.filterPcDiagnostics(req, Vector(diag)), Vector.empty)

  test("beforeRequest can rewrite the request"):
    val manager = newManager()
    manager.register(new PcServicePlugin {
      def id = "move-right"
      override def beforeRequest(req: PcRequest): PcRequest = req.copy(character = req.character + 1)
    })
    val req = PcRequest(PcRequestKind.Hover, "file:///x.scala", 3, 4, "t")
    assertEquals(manager.beforeRequest(req).character, 5)

  test("syntheticSources concatenates across plugins"):
    val manager = newManager()
    manager.register(new PcServicePlugin {
      def id = "gen-a"
      override def syntheticSources(ctx: PcTargetContext) = Vector(VirtualSource("A.scala", "object A"))
    })
    manager.register(new PcServicePlugin {
      def id = "gen-b"
      override def syntheticSources(ctx: PcTargetContext) =
        Vector(VirtualSource("B.scala", "object B", sticky = true))
    })
    val sources = manager.syntheticSources(ctx)
    assertEquals(sources.map(_.path), Vector("A.scala", "B.scala"))
    assertEquals(sources.map(_.sticky), Vector(false, true))

class ServiceLoaderSuite extends munit.FunSuite:

  /** Build a jar containing only a ServiceLoader provider-configuration file
    * naming a plugin class that the parent loader can resolve.
    */
  private def servicesJar(dir: Path, providerClass: String): Path =
    val jar = dir.resolve("service-plugin.jar")
    val out = new JarOutputStream(Files.newOutputStream(jar))
    out.putNextEntry(new JarEntry("META-INF/services/ls.pc.PcServicePlugin"))
    out.write((providerClass + "\n").getBytes("UTF-8"))
    out.closeEntry()
    out.close()
    jar

  test("service plugins load via ServiceLoader from configured jars"):
    val dir = Files.createTempDirectory("ls-pc-sl")
    val jar = servicesJar(dir, classOf[ServiceLoadedTestPlugin].getName)
    val manager = new PcPluginManager(PcPluginInitContext(None, dir.resolve("gen")))
    manager.loadServicePluginJars(Vector(jar))
    assertEquals(manager.enabledPluginIds, Vector("service-loaded-test"))
    val status = manager.statusReport.servicePlugins.find(_.id == "service-loaded-test").get
    assert(status.enabled)
    assert(status.selfTestOk)
    assertEquals(status.source, jar.toString)

  test("a broken provider entry is recorded, not thrown"):
    val dir = Files.createTempDirectory("ls-pc-sl-bad")
    val jar = servicesJar(dir, "ls.pc.DoesNotExistPlugin")
    val manager = new PcPluginManager(PcPluginInitContext(None, dir.resolve("gen")))
    manager.loadServicePluginJars(Vector(jar))
    assertEquals(manager.enabledPluginIds, Vector.empty)
    assert(manager.statusReport.disabled.nonEmpty)

  test("missing service plugin jar is a failed self-test, not a crash"):
    val dir = Files.createTempDirectory("ls-pc-sl-missing")
    val missing = dir.resolve("absent.jar")
    val manager = new PcPluginManager(PcPluginInitContext(None, dir.resolve("gen")))
    manager.loadServicePluginJars(Vector(missing))
    val report = manager.statusReport
    assert(report.servicePlugins.exists(s => s.id == missing.toString && !s.selfTestOk))
    assert(report.disabled.exists(_.id == missing.toString))
