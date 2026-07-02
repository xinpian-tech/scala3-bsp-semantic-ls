package ls.bsp

import ch.epfl.scala.bsp4j.*

/** Pluggable callbacks for server-initiated BSP notifications and for the
  * launched server's stderr. Task progress and run-print notifications are
  * deliberately dropped: this LS consumes diagnostics, log/show messages and
  * build-target-change events only (plan section 4.1).
  */
final case class BspClientHandlers(
    onDiagnostics: PublishDiagnosticsParams => Unit = _ => (),
    onLogMessage: LogMessageParams => Unit = _ => (),
    onShowMessage: ShowMessageParams => Unit = _ => (),
    onDidChangeBuildTarget: DidChangeBuildTarget => Unit = _ => (),
    onServerStderr: String => Unit = _ => ()
)

/** The local jsonrpc service: forwards server notifications to handlers. */
final class ForwardingBuildClient(handlers: BspClientHandlers) extends BuildClient:
  override def onBuildShowMessage(params: ShowMessageParams): Unit =
    handlers.onShowMessage(params)
  override def onBuildLogMessage(params: LogMessageParams): Unit =
    handlers.onLogMessage(params)
  override def onBuildPublishDiagnostics(params: PublishDiagnosticsParams): Unit =
    handlers.onDiagnostics(params)
  override def onBuildTargetDidChange(params: DidChangeBuildTarget): Unit =
    handlers.onDidChangeBuildTarget(params)
  override def onBuildTaskStart(params: TaskStartParams): Unit = ()
  override def onBuildTaskProgress(params: TaskProgressParams): Unit = ()
  override def onBuildTaskFinish(params: TaskFinishParams): Unit = ()
  override def onRunPrintStdout(params: PrintParams): Unit = ()
  override def onRunPrintStderr(params: PrintParams): Unit = ()
