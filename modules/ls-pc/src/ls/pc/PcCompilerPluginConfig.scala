package ls.pc

import java.nio.file.{Files, Path, Paths}

import scala.jdk.CollectionConverters.*

import com.google.gson.{GsonBuilder, JsonArray, JsonObject, JsonParser}

/** One PC-only compiler plugin: its jar(s) and its `-P:` options.
  *
  * @param jars    plugin jars, each becomes one `-Xplugin:<jar>` argument
  * @param options raw plugin options in scalac `-P:` payload form, i.e.
  *                `"<plugin>:<opt>"`; each becomes one `-P:<plugin>:<opt>` argument
  */
final case class CompilerPluginSpec(jars: Vector[Path], options: Vector[String]):
  /** Pure config -> scalac-argument mapping. No filesystem checks. */
  def toOptions: Vector[String] =
    jars.map(j => s"-Xplugin:$j") ++ options.map(o => s"-P:$o")

/** PC compiler plugin configuration (plan 14.2). These options are appended
  * to presentation-compiler options only — they never reach the build server
  * and never affect SemanticDB generation.
  */
final case class PcCompilerPluginConfig(plugins: Vector[CompilerPluginSpec]):
  def toOptions: Vector[String] = plugins.flatMap(_.toOptions)

object PcCompilerPluginConfig:
  val empty: PcCompilerPluginConfig = PcCompilerPluginConfig(Vector.empty)

/** Full contents of `.scala3-bsp-semantic-ls/pc-plugins.json`. */
final case class PcPluginConfig(
    compilerPlugins: PcCompilerPluginConfig,
    servicePluginJars: Vector[Path]
)

object PcPluginConfig:
  val empty: PcPluginConfig = PcPluginConfig(PcCompilerPluginConfig.empty, Vector.empty)

/** Loader/writer for the user plugin configuration file.
  *
  * Schema (see `resources/default-plugin-schema.json`):
  * {{{
  * {
  *   "compilerPlugins": [ { "jars": ["/path/plugin.jar"], "options": ["plugin:key:value"] } ],
  *   "servicePluginJars": ["/path/service-plugin.jar"]
  * }
  * }}}
  *
  * Parsing is lenient about missing fields (they default to empty). Jar
  * existence is *not* checked here: [[PcPluginManager]] validates jars when
  * the config is applied and records missing jars as failed self-tests
  * instead of crashing.
  */
object PcPluginConfigLoader:

  /** Conventional location of the config file inside a workspace. */
  def defaultPath(workspaceRoot: Path): Path =
    workspaceRoot.resolve(".scala3-bsp-semantic-ls").resolve("pc-plugins.json")

  def load(file: Path): PcPluginConfig =
    parse(Files.readString(file))

  def parse(json: String): PcPluginConfig =
    val rootElem = JsonParser.parseString(json)
    if !rootElem.isJsonObject then
      throw new IllegalArgumentException(s"pc-plugins config must be a JSON object, got: $rootElem")
    val root = rootElem.getAsJsonObject
    val compilerPlugins =
      arrayOf(root, "compilerPlugins").asScala.toVector.collect {
        case e if e.isJsonObject =>
          val obj = e.getAsJsonObject
          CompilerPluginSpec(
            jars = stringsOf(obj, "jars").map(Paths.get(_)),
            options = stringsOf(obj, "options")
          )
      }
    val serviceJars = stringsOf(root, "servicePluginJars").map(Paths.get(_))
    PcPluginConfig(PcCompilerPluginConfig(compilerPlugins), serviceJars)

  def toJson(config: PcPluginConfig): String =
    val root = new JsonObject
    val plugins = new JsonArray
    config.compilerPlugins.plugins.foreach { spec =>
      val obj = new JsonObject
      val jars = new JsonArray
      spec.jars.foreach(j => jars.add(j.toString))
      obj.add("jars", jars)
      val opts = new JsonArray
      spec.options.foreach(opts.add)
      obj.add("options", opts)
      plugins.add(obj)
    }
    root.add("compilerPlugins", plugins)
    val serviceJars = new JsonArray
    config.servicePluginJars.foreach(j => serviceJars.add(j.toString))
    root.add("servicePluginJars", serviceJars)
    new GsonBuilder().setPrettyPrinting().create().toJson(root)

  def write(config: PcPluginConfig, file: Path): Unit =
    if file.getParent != null then Files.createDirectories(file.getParent)
    Files.writeString(file, toJson(config))

  private def arrayOf(obj: JsonObject, field: String): JsonArray =
    val e = obj.get(field)
    if e == null || !e.isJsonArray then new JsonArray else e.getAsJsonArray

  private def stringsOf(obj: JsonObject, field: String): Vector[String] =
    arrayOf(obj, field).asScala.toVector.collect {
      case e if e.isJsonPrimitive && e.getAsJsonPrimitive.isString => e.getAsString
    }
