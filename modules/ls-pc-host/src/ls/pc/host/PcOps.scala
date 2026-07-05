package ls.pc.host

import org.eclipse.lsp4j as l

import ls.pc.{DefinitionResult as SpiDefinitionResult, PcFacade, PcPluginStatusReport, PcTargetConfig}

/** The presentation-compiler operations the boundary op seam drives, as a thin
  * interface over [[ls.pc.PcFacade]]. Isolating the facade behind this seam lets
  * the op routing (decode → call → convert → encode) be unit-tested against a
  * stub without booting a real compiler; the live compiler behaviour itself is
  * exercised in the real-JVM integration.
  */
trait PcOps:
  def registerTarget(config: PcTargetConfig): Unit
  def didOpen(targetId: String, uri: String, text: String): Unit
  def didChange(uri: String, text: String): Unit
  def didClose(uri: String): Unit
  def completion(uri: String, line: Int, character: Int): l.CompletionList
  def completionItemResolve(targetId: String, item: l.CompletionItem, symbol: String): l.CompletionItem
  def hover(uri: String, line: Int, character: Int): Option[l.Hover]
  def signatureHelp(uri: String, line: Int, character: Int): l.SignatureHelp
  def definition(uri: String, line: Int, character: Int): SpiDefinitionResult
  def typeDefinition(uri: String, line: Int, character: Int): SpiDefinitionResult
  def prepareRename(uri: String, line: Int, character: Int): Option[l.Range]
  def pluginStatus: PcPluginStatusReport
  def restartInstances(): Unit
  def shutdown(): Unit

/** Adapts the retained in-process [[PcFacade]] to [[PcOps]]. `restartInstances`
  * disposes every live target instance (each lazily recreated on its next
  * request) without clearing registered targets or open buffers, so the Rust
  * generation-recovery replay stays coherent.
  */
final class FacadePcOps(facade: PcFacade) extends PcOps:
  def registerTarget(config: PcTargetConfig): Unit = facade.registerTarget(config)
  def didOpen(targetId: String, uri: String, text: String): Unit = facade.didOpen(targetId, uri, text)
  def didChange(uri: String, text: String): Unit = facade.didChange(uri, text)
  def didClose(uri: String): Unit = facade.didClose(uri)
  def completion(uri: String, line: Int, character: Int): l.CompletionList =
    TestFault.maybeWedgeCompletion(uri)
    facade.completion(uri, line, character)
  def completionItemResolve(targetId: String, item: l.CompletionItem, symbol: String): l.CompletionItem =
    facade.completionItemResolve(targetId, item, symbol)
  def hover(uri: String, line: Int, character: Int): Option[l.Hover] = facade.hover(uri, line, character)
  def signatureHelp(uri: String, line: Int, character: Int): l.SignatureHelp =
    facade.signatureHelp(uri, line, character)
  def definition(uri: String, line: Int, character: Int): SpiDefinitionResult =
    facade.definition(uri, line, character)
  def typeDefinition(uri: String, line: Int, character: Int): SpiDefinitionResult =
    facade.typeDefinition(uri, line, character)
  def prepareRename(uri: String, line: Int, character: Int): Option[l.Range] =
    facade.prepareRename(uri, line, character)
  def pluginStatus: PcPluginStatusReport = facade.pluginStatus
  def restartInstances(): Unit = facade.activeTargets.foreach(facade.restartTarget)
  def shutdown(): Unit = facade.shutdown()

/** Test-only dispatch-lane fault injection, controlled by the
  * `ls.pc.host.testFault` JVM system property (unset in production → a no-op).
  *
  * When set to `busyCompletion`, a completion whose URI carries the wedge marker
  * enters a bounded, non-cooperative busy loop that ignores interrupts and a PC
  * restart, so the Rust watchdog cannot free the dispatch lane cooperatively and
  * must recover it by loaning a fresh dispatch generation. It exists to exercise
  * that recovery path over the real embedded-JVM boundary; it is inert unless the
  * property is explicitly set.
  */
object TestFault:
  private final val Property = "ls.pc.host.testFault"
  private final val BusyCompletion = "busyCompletion"
  private final val WedgeUriMarker = "wedge"
  // Long enough to outlast the watchdog's recovery window, bounded so a leaked
  // (abandoned) dispatch thread cannot hang a test process forever.
  private final val BusyLoopMillis = 60000L

  def maybeWedgeCompletion(uri: String): Unit =
    if BusyCompletion == System.getProperty(Property) && uri.toLowerCase.contains(WedgeUriMarker) then
      val deadline = System.currentTimeMillis() + BusyLoopMillis
      while System.currentTimeMillis() < deadline do
        try Thread.sleep(20L)
        catch case _: InterruptedException => () // non-cooperative: ignore interrupts
