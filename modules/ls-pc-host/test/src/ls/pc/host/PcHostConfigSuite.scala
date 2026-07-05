package ls.pc.host

import java.lang.foreign.{Arena, MemorySegment, ValueLayout}
import java.nio.file.{Files, Path}

import ls.pc.{CompilerPluginSpec, PcCompilerPluginConfig, PcFacade}
import ls.pc.{PcPluginConfig, PcPluginConfigLoader, PcPluginInitContext, PcPluginManager, PcSettings}
import ls.pc.host.boundary.{boundary_h, LsBuf}
import ls.pc.host.codec.Payloads

/** The embedded host loads the per-workspace `pc-plugins.json` (compiler +
  * service plugins) into the plugin manager, matching the retained worker, so
  * the doctor `plugin_status` and PC option patching see the same config. No
  * live compiler is needed — status/options are read straight off the manager.
  */
class PcHostConfigSuite extends munit.FunSuite:

  private def withWorkspace(body: Path => Unit): Unit =
    val ws = Files.createTempDirectory("pchost-ws")
    try body(ws)
    finally Files.walk(ws).sorted(java.util.Comparator.reverseOrder()).forEach(p => { Files.deleteIfExists(p); () })

  private def manager(ws: Path): PcPluginManager =
    PcPluginManager(PcPluginInitContext(Some(ws), ws.resolve("gen")))

  private def writeConfig(ws: Path, config: PcPluginConfig): Unit =
    PcPluginConfigLoader.write(config, PcPluginConfigLoader.defaultPath(ws))

  test("a configured compiler plugin loads: status + PC option patching"):
    withWorkspace { ws =>
      val jar = Files.createFile(ws.resolve("myplugin.jar"))
      writeConfig(
        ws,
        PcPluginConfig(
          PcCompilerPluginConfig(Vector(CompilerPluginSpec(Vector(jar), Vector("myplugin:key:value")))),
          Vector.empty
        )
      )
      val pm = manager(ws)
      assert(PcHostConfig.applyWorkspacePlugins(pm, ws, _ => ()))
      val cp = pm.statusReport.compilerPlugins
      assertEquals(cp.length, 1)
      assert(cp.head.loaded, cp.head.detail)
      assertEquals(cp.head.jars, Vector(jar.toString))
      // PC option patching: the plugin's -Xplugin/-P args are what the worker
      // manager appends to each target's PC options.
      assertEquals(pm.compilerPluginOptions, Vector(s"-Xplugin:$jar", "-P:myplugin:key:value"))
    }

  test("a configured service-plugin jar's status reaches plugin_status"):
    withWorkspace { ws =>
      val svc = ws.resolve("service.jar") // absent → recorded as a failed self-test
      writeConfig(ws, PcPluginConfig(PcCompilerPluginConfig.empty, Vector(svc)))
      val pm = manager(ws)
      assert(PcHostConfig.applyWorkspacePlugins(pm, ws, _ => ()))
      val svcStatus = pm.statusReport.servicePlugins
      assertEquals(svcStatus.map(_.id), Vector(svc.toString))
      assert(!svcStatus.head.selfTestOk)
    }

  test("the plugin_status boundary op reports the configured compiler plugin"):
    withWorkspace { ws =>
      val jar = Files.createFile(ws.resolve("p.jar"))
      writeConfig(
        ws,
        PcPluginConfig(PcCompilerPluginConfig(Vector(CompilerPluginSpec(Vector(jar), Vector.empty))), Vector.empty)
      )
      val pm = manager(ws)
      PcHostConfig.applyWorkspacePlugins(pm, ws, _ => ())
      val facade = PcFacade(pm, PcSettings.forWorkspace(ws))
      val arena = Arena.ofConfined()
      try
        val host = PcHost(PcHostRuntime(size => arena.allocate(size.toLong)), FacadePcOps(facade), _ => ())
        val out = LsBuf.allocate(arena)
        assertEquals(host.pluginStatus(out), boundary_h.STATUS_OK())
        val bytes = LsBuf.ptr(out).reinterpret(LsBuf.len(out).toLong).toArray(ValueLayout.JAVA_BYTE)
        assertEquals(Payloads.PluginStatus.decode(bytes).compilerPlugins.map(_.jars), Seq(Seq(jar.toString)))
      finally arena.close()
    }

  test("a malformed plugin config is logged, not fatal, and boot survives"):
    withWorkspace { ws =>
      val configPath = PcPluginConfigLoader.defaultPath(ws)
      Files.createDirectories(configPath.getParent)
      Files.writeString(configPath, "[1, 2, 3]") // valid JSON, but not the object schema
      val pm = manager(ws)
      var logged = List.empty[String]
      assert(!PcHostConfig.applyWorkspacePlugins(pm, ws, m => logged = m :: logged))
      assert(logged.nonEmpty, "expected a load-failure log line")
      assertEquals(pm.statusReport.compilerPlugins, Vector.empty)
    }

  test("no plugin config file is a no-op"):
    withWorkspace { ws =>
      assert(!PcHostConfig.applyWorkspacePlugins(manager(ws), ws, _ => ()))
    }
