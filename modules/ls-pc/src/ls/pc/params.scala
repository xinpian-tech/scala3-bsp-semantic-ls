package ls.pc

import java.net.URI
import java.util.concurrent.{CompletableFuture, CompletionStage}

import scala.meta.pc.{CancelToken, OffsetParams, VirtualFileParams}

/** A cancel token that never cancels. PC requests are bounded by the
  * synchronous timeout in [[PcInstance]] instead of cooperative cancellation.
  */
object PcCancelToken:
  object Empty extends CancelToken:
    private val never = new CompletableFuture[java.lang.Boolean]()
    override def onCancel(): CompletionStage[java.lang.Boolean] = never
    override def checkCanceled(): Unit = ()

/** In-memory file contents handed to the presentation compiler. */
final case class PcVirtualFileParams(
    uri: URI,
    text: String,
    returnDiagnostics: Boolean = false,
    token: CancelToken = PcCancelToken.Empty
) extends VirtualFileParams:
  override def shouldReturnDiagnostics: Boolean = returnDiagnostics

/** In-memory file contents plus a UTF-16 code-unit offset. */
final case class PcOffsetParams(
    uri: URI,
    text: String,
    offset: Int,
    token: CancelToken = PcCancelToken.Empty
) extends OffsetParams
