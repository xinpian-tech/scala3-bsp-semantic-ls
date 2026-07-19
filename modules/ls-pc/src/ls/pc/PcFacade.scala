package ls.pc

import scala.collection.concurrent.TrieMap

import org.eclipse.lsp4j.{CompletionItem, CompletionList, Diagnostic, Hover, Position, Range, SignatureHelp}

/** The single PC entry point for the LSP core (plan 4.3, 14).
  *
  * Owns the per-target worker manager, the plugin manager, and the
  * dirty-buffer text store. Queries take `(uri, LSP position)`; the text is
  * read from the buffer store, the position is converted to a UTF-16
  * code-unit offset against that text, and results are lsp4j model objects
  * threaded through the service-plugin hooks
  * (`beforeRequest`/`afterCompletion`/`afterHover`/`afterDefinition`/
  * `filterPcDiagnostics`).
  *
  * PC results are editing-time only and NEVER touch the persistent index
  * store (plan 4.3): this class — and the whole `pc` module — has no
  * reference to the index/store layers (they live in the Rust host), so the
  * boundary is enforced structurally, not by convention.
  */
final class PcFacade(
    val pluginManager: PcPluginManager,
    val settings: PcSettings,
    /** Cross-file definition lookup handed down to the worker manager's PC
      * instances; defaults to a no-op (cross-file go-to stays empty).
      */
    resolver: PcDefinitionResolver = PcDefinitionResolver.Empty
):

  def this(pluginManager: PcPluginManager) = this(pluginManager, PcSettings.ephemeral())

  private val manager = new PcWorkerManager(pluginManager, settings, resolver)

  /** Exposed for tests and the doctor; not part of the LSP-facing surface. */
  private[pc] def workerManager: PcWorkerManager = manager
  private val targets = TrieMap.empty[String, PcTargetConfig]
  private final case class Buffer(targetId: String, text: String)
  private val buffers = TrieMap.empty[String, Buffer]

  // --- target + dirty buffer lifecycle ---------------------------------------

  /** Register (or update) the PC configuration for a build target. A changed
    * config causes the target's PC instance to be recreated on next use.
    */
  def registerTarget(config: PcTargetConfig): Unit =
    targets.put(config.bspId, config)

  /** Open a dirty buffer for `uri`, served by target `targetId`. */
  def didOpen(targetId: String, uri: String, text: String): Unit =
    require(targets.contains(targetId), s"didOpen for unregistered target '$targetId'")
    buffers.put(uri, Buffer(targetId, text))

  /** Replace the full text of an open buffer. */
  def didChange(uri: String, text: String): Unit =
    val buf = buffer(uri)
    buffers.put(uri, buf.copy(text = text))

  /** Close a buffer and tell the target's PC (if live) to drop it. */
  def didClose(uri: String): Unit =
    buffers.remove(uri).foreach { buf =>
      // only notify a live instance; closing must not force instance creation
      if manager.activeTargets.contains(buf.targetId) then
        targets.get(buf.targetId).foreach { config =>
          try manager.run(config)(_.didClose(uri))
          catch case scala.util.control.NonFatal(_) => ()
        }
    }

  def openBuffers: Vector[String] = buffers.keySet.toVector

  def bufferText(uri: String): Option[String] = buffers.get(uri).map(_.text)

  // --- queries ---------------------------------------------------------------

  def completion(uri: String, line: Int, character: Int): CompletionList =
    val (req, text, config) = prepare(PcRequestKind.Completion, uri, line, character)
    val raw = manager.run(config)(_.complete(req.uri, text, req.line, req.character))
    pluginManager.afterCompletion(req, raw)

  def completionItemResolve(targetId: String, item: CompletionItem, symbol: String): CompletionItem =
    manager.run(configOf(targetId))(_.completionItemResolve(item, symbol))

  def hover(uri: String, line: Int, character: Int): Option[Hover] =
    val (req, text, config) = prepare(PcRequestKind.Hover, uri, line, character)
    val raw = manager.run(config)(_.hover(req.uri, text, req.line, req.character))
    pluginManager.afterHover(req, raw)

  def signatureHelp(uri: String, line: Int, character: Int): SignatureHelp =
    val (req, text, config) = prepare(PcRequestKind.SignatureHelp, uri, line, character)
    manager.run(config)(_.signatureHelp(req.uri, text, req.line, req.character))

  def definition(uri: String, line: Int, character: Int): DefinitionResult =
    definitionLike(PcRequestKind.Definition, uri, line, character)(_.definition(_, _, _, _))

  def typeDefinition(uri: String, line: Int, character: Int): DefinitionResult =
    definitionLike(PcRequestKind.TypeDefinition, uri, line, character)(_.typeDefinition(_, _, _, _))

  def prepareRename(uri: String, line: Int, character: Int): Option[Range] =
    val (req, text, config) = prepare(PcRequestKind.PrepareRename, uri, line, character)
    manager.run(config)(_.prepareRename(req.uri, text, req.line, req.character))

  // --- ABI v2 op stubs (transport wave W3a) ----------------------------------
  //
  // Each of the six ops below is a typed stub: the boundary slot, payload
  // codecs, and dispatch routing exist end-to-end, but the presentation-
  // compiler provider lands with the feature task. Until then every call
  // throws [[PcNotYetSupported]], which the boundary runtime maps to the
  // distinct `STATUS_NOT_YET` status (`pc_diagnostics` has no stub here: its
  // provider will route through [[diagnostics]] below).

  /** Inlay hints for `range` of the open buffer `uri`; `flags` is the boundary
    * hint-category bitset (opaque to the transport).
    */
  def inlayHints(uri: String, range: Range, flags: Int): Vector[PcInlayHint] =
    throw PcNotYetSupported("inlayHints")

  /** Semantic tokens of the open buffer `uri`, as offset-based nodes. */
  def semanticTokens(uri: String): Vector[PcSemanticNode] =
    throw PcNotYetSupported("semanticTokens")

  /** Per query position, the chain of enclosing selection ranges, innermost
    * first.
    */
  def selectionRanges(uri: String, positions: Vector[Position]): Vector[Vector[Range]] =
    throw PcNotYetSupported("selectionRanges")

  /** Run the PC-backed code action `actionId` (the boundary's action-id enum)
    * at `position`; `extractionEnd` is extract-method's selection end,
    * `argIndices` convert-to-named-arguments' argument list. A refusal the
    * editor should surface comes back as data on the result, not as a thrown
    * error.
    */
  def codeAction(
      uri: String,
      actionId: Int,
      position: Position,
      extractionEnd: Option[Position],
      argIndices: Option[Vector[Int]]
  ): PcCodeActionResult =
    throw PcNotYetSupported("codeAction")

  /** Auto-import candidates for `name` at `position` (`isExtension` requests
    * extension-method imports), best first.
    */
  def autoImports(
      uri: String,
      position: Position,
      name: String,
      isExtension: Boolean
  ): Vector[PcAutoImport] =
    throw PcNotYetSupported("autoImports")

  /** Folding ranges of the open buffer `uri`. */
  def foldingRanges(uri: String): Vector[PcFoldingRange] =
    throw PcNotYetSupported("foldingRanges")

  /** Push the current buffer text through the PC and return its (secondary)
    * diagnostics, filtered by plugin `filterPcDiagnostics` hooks. Build
    * diagnostics from BSP remain the primary diagnostics.
    */
  def diagnostics(uri: String): Vector[Diagnostic] =
    val buf = buffer(uri)
    val req = pluginManager.beforeRequest(PcRequest(PcRequestKind.Diagnostics, uri, 0, 0, buf.targetId))
    val text = buffer(req.uri).text
    val raw = manager.run(configOf(req.targetId))(_.didChange(req.uri, text))
    pluginManager.filterPcDiagnostics(req, raw)

  // --- lifecycle / doctor ------------------------------------------------------

  def pluginStatus: PcPluginStatusReport = pluginManager.statusReport

  /** Targets with a live PC instance (doctor: "PC: active targets"). */
  def activeTargets: Vector[String] = manager.activeTargets

  def registeredTargets: Vector[String] = targets.keySet.toVector

  /** Dispose the target's PC instance; lazily recreated on next request. */
  def restartTarget(targetId: String): Boolean = manager.restartTarget(targetId)

  def shutdown(): Unit =
    manager.shutdownAll()
    buffers.clear()

  // --- internals ---------------------------------------------------------------

  private def buffer(uri: String): Buffer =
    buffers.getOrElse(
      uri,
      throw new IllegalStateException(s"no open dirty buffer for '$uri' (didOpen it first)")
    )

  private def configOf(targetId: String): PcTargetConfig =
    targets.getOrElse(
      targetId,
      throw new IllegalStateException(s"target '$targetId' is not registered with the PC facade")
    )

  /** Run `beforeRequest` hooks, then resolve buffer text and target config for
    * the (possibly rewritten) request. The compiler offset is derived from the
    * post-hook position against the post-hook buffer text.
    */
  private def prepare(
      kind: PcRequestKind,
      uri: String,
      line: Int,
      character: Int
  ): (PcRequest, String, PcTargetConfig) =
    val buf = buffer(uri)
    val req = pluginManager.beforeRequest(PcRequest(kind, uri, line, character, buf.targetId))
    val text = buffer(req.uri).text
    (req, text, configOf(req.targetId))

  private def definitionLike(
      kind: PcRequestKind,
      uri: String,
      line: Int,
      character: Int
  )(query: (PcInstance, String, String, Int, Int) => DefinitionResult): DefinitionResult =
    val (req, text, config) = prepare(kind, uri, line, character)
    val (base, syntheticUris) = manager.run(config) { instance =>
      (query(instance, req.uri, text, req.line, req.character), instance.syntheticUris)
    }
    val after = pluginManager.afterDefinition(req, base)
    markPluginAdditions(base, after, syntheticUris)

  /** Enforce plan 14.4 "definition: yes, but mark the source": any location a
    * plugin added (or that points into a synthetic source) keeps a non-
    * Workspace origin, even if the plugin claimed otherwise.
    */
  private def markPluginAdditions(
      base: DefinitionResult,
      after: DefinitionResult,
      syntheticUris: Set[String]
  ): DefinitionResult =
    def key(dl: DefinitionLocation): (String, String) =
      (dl.location.getUri, String.valueOf(dl.location.getRange))
    val baseKeys = base.locations.map(key).toSet
    val marked = after.locations.map { dl =>
      if syntheticUris.contains(dl.location.getUri) then dl.copy(origin = DefinitionOrigin.Synthetic)
      else if dl.origin == DefinitionOrigin.Workspace && !baseKeys.contains(key(dl)) then
        dl.copy(origin = DefinitionOrigin.Plugin)
      else dl
    }
    after.copy(locations = marked)
