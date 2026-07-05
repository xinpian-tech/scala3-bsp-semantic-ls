package ls.pc.host

import java.nio.file.{Files, Path}

import ls.pc.{PcPluginConfigLoader, PcPluginManager}

/** Loads the per-workspace PC plugin config into the plugin manager during
  * island boot, matching the retained worker's behaviour. The config lives at
  * `<workspaceRoot>/.scala3-bsp-semantic-ls/pc-plugins.json`; it is optional,
  * and a malformed config is logged rather than fatal, so the island still
  * boots with whatever plugins loaded.
  */
object PcHostConfig:

  /** Applies the workspace plugin config to `pluginManager` when the file
    * exists, returning true when a config was applied. Any load/parse failure
    * is logged via `log` and swallowed — island boot must never fail on a bad
    * plugin config.
    */
  def applyWorkspacePlugins(pluginManager: PcPluginManager, workspaceRoot: Path, log: String => Unit): Boolean =
    val configPath = PcPluginConfigLoader.defaultPath(workspaceRoot)
    if !Files.exists(configPath) then false
    else
      try
        pluginManager.applyConfig(PcPluginConfigLoader.load(configPath))
        true
      catch
        case t: Throwable =>
          log(s"failed to load plugin config $configPath: $t")
          false
