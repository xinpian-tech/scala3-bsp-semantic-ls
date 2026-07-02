package ls.pc

import java.nio.file.Path

import org.eclipse.lsp4j.{CompletionList, Diagnostic, Hover, Location}

/** One-time context handed to [[PcServicePlugin.initialize]] when the plugin
  * is loaded inside the PC worker.
  *
  * @param workspaceRoot        workspace root, when the LS runs inside one
  * @param generatedSourcesRoot directory where plugin synthetic sources are
  *                             materialized (`.scala3-bsp-semantic-ls/pc/generated-sources`)
  * @param log                  diagnostic logger; output lands on the PC worker's stderr
  */
final case class PcPluginInitContext(
    workspaceRoot: Option[Path],
    generatedSourcesRoot: Path,
    log: String => Unit = _ => ()
)

/** Per-build-target context for instance-creation hooks. */
final case class PcTargetContext(
    bspId: String,
    scalaVersion: String,
    classpath: Vector[Path],
    workspaceRoot: Option[Path]
)

/** A PC-only synthetic source contributed by a service plugin.
  *
  * @param path    path relative to the target's generated-sources directory
  * @param content Scala source text
  * @param sticky  if true, the materialized file survives PC instance
  *                disposal/eviction; non-sticky files are deleted with the
  *                instance. Synthetic sources are visible only to the PC
  *                (plan 14.5): symbols defined only here are PC-only symbols
  *                and never enter SQLite or postings.
  */
final case class VirtualSource(
    path: String,
    content: String,
    sticky: Boolean = false
)

/** Kind of PC request a hook is observing. */
enum PcRequestKind:
  case Completion, CompletionResolve, Hover, SignatureHelp, Definition, TypeDefinition,
    PrepareRename, Diagnostics

/** An in-flight PC request as seen by plugin hooks. Positions use LSP
  * semantics (zero-based line, UTF-16 code-unit character). The string offset
  * used for the compiler is derived from `(line, character)` against the
  * dirty-buffer text *after* `beforeRequest` hooks run, so a hook that moves
  * the position never leaves a stale offset behind.
  */
final case class PcRequest(
    kind: PcRequestKind,
    uri: String,
    line: Int,
    character: Int,
    targetId: String
)

/** Where a definition location came from (plan 14.4: definition — "yes, but
  * mark the source").
  */
enum DefinitionOrigin:
  /** Ordinary compiler-resolved location in real sources. */
  case Workspace

  /** Location inside a plugin-provided synthetic source. */
  case Synthetic

  /** Location added or redirected by a service plugin hook. */
  case Plugin

/** One definition location plus its origin marking. */
final case class DefinitionLocation(location: Location, origin: DefinitionOrigin)

/** Definition result carried through the plugin pipeline. Locations keep an
  * origin mark so downstream consumers can distinguish compiler truth from
  * plugin/synthetic contributions (PC-only symbols must never feed global
  * references/rename).
  */
final case class DefinitionResult(symbol: String, locations: Vector[DefinitionLocation]):
  def isEmpty: Boolean = locations.isEmpty

  def lspLocations: java.util.List[Location] =
    val out = new java.util.ArrayList[Location](locations.size)
    locations.foreach(dl => out.add(dl.location))
    out

  def hasSyntheticHits: Boolean =
    locations.exists(dl => dl.origin == DefinitionOrigin.Synthetic || dl.origin == DefinitionOrigin.Plugin)

object DefinitionResult:
  val empty: DefinitionResult = DefinitionResult("", Vector.empty)

/** Stable PC service plugin SPI (plan 14.3, docs/plugin-spi.md).
  *
  * Every hook has an identity default so a plugin overrides only what it
  * needs. Hooks affect PC request results only: plugins have no access to
  * SQLite, mmap postings, or any persistent index (the `pc` module does not
  * even depend on those modules). A hook that throws disables the plugin for
  * subsequent requests and the request proceeds as if the hook were identity.
  */
trait PcServicePlugin:
  def id: String
  def initialize(ctx: PcPluginInitContext): Unit = ()
  def patchOptions(ctx: PcTargetContext, options: Vector[String]): Vector[String] = options
  def patchSourcePath(ctx: PcTargetContext, sourcePath: Vector[Path]): Vector[Path] = sourcePath
  def syntheticSources(ctx: PcTargetContext): Vector[VirtualSource] = Vector.empty
  def beforeRequest(req: PcRequest): PcRequest = req
  def afterCompletion(req: PcRequest, result: CompletionList): CompletionList = result
  def afterHover(req: PcRequest, result: Option[Hover]): Option[Hover] = result
  def afterDefinition(req: PcRequest, result: DefinitionResult): DefinitionResult = result
  def filterPcDiagnostics(req: PcRequest, diagnostics: Vector[Diagnostic]): Vector[Diagnostic] =
    diagnostics
