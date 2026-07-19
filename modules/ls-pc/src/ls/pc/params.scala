package ls.pc

import java.net.URI
import java.util.concurrent.{CompletableFuture, CompletionStage}

import scala.meta.pc.{CancelToken, InlayHintsParams, OffsetParams, RangeParams, VirtualFileParams}

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

/** In-memory file contents plus a UTF-16 code-unit `[offset, endOffset]` range. */
final case class PcRangeParams(
    uri: URI,
    text: String,
    offset: Int,
    endOffset: Int,
    token: CancelToken = PcCancelToken.Empty
) extends RangeParams

/** [[scala.meta.pc.InlayHintsParams]] carrier: the range plus the nine
  * hint-category booleans, decoded from the boundary's `inlay_hints` flags
  * bitset with [[PcInlayHintFlags.paramsFor]].
  */
final case class PcInlayHintsParams(
    uri: URI,
    text: String,
    offset: Int,
    endOffset: Int,
    inferredTypesEnabled: Boolean,
    typeParametersEnabled: Boolean,
    implicitParametersEnabled: Boolean,
    byNameParametersEnabled: Boolean,
    implicitConversionsEnabled: Boolean,
    namedParametersEnabled: Boolean,
    hintsXRayModeEnabled: Boolean,
    hintsInPatternMatchEnabled: Boolean,
    closingLabelsEnabled: Boolean,
    token: CancelToken = PcCancelToken.Empty
) extends InlayHintsParams:
  override def inferredTypes: Boolean = inferredTypesEnabled
  override def typeParameters: Boolean = typeParametersEnabled
  override def implicitParameters: Boolean = implicitParametersEnabled
  override def byNameParameters: Boolean = byNameParametersEnabled
  override def implicitConversions: Boolean = implicitConversionsEnabled
  override def namedParameters: Boolean = namedParametersEnabled
  override def hintsXRayMode: Boolean = hintsXRayModeEnabled
  override def hintsInPatternMatch: Boolean = hintsInPatternMatchEnabled
  override def closingLabels: Boolean = closingLabelsEnabled

/** The `inlay_hints` flags bitset (the boundary `InlayHintParams.flags` u32):
  * one bit per [[scala.meta.pc.InlayHintsParams]] hint-category boolean. The
  * bit assignment mirrors the Rust `payloads::inlay_hint_flags` constants
  * bit-for-bit; an unset bit disables its category (flags `0` requests no
  * hints).
  */
object PcInlayHintFlags:
  val InferredTypes: Int = 1 << 0
  val TypeParameters: Int = 1 << 1
  val ImplicitParameters: Int = 1 << 2
  val ByNameParameters: Int = 1 << 3
  val ImplicitConversions: Int = 1 << 4
  val NamedParameters: Int = 1 << 5
  val HintsXRayMode: Int = 1 << 6
  val HintsInPatternMatch: Int = 1 << 7
  val ClosingLabels: Int = 1 << 8

  /** Decode `flags` into the typed params for `[offset, endOffset]`. */
  def paramsFor(uri: URI, text: String, offset: Int, endOffset: Int, flags: Int): PcInlayHintsParams =
    PcInlayHintsParams(
      uri = uri,
      text = text,
      offset = offset,
      endOffset = endOffset,
      inferredTypesEnabled = (flags & InferredTypes) != 0,
      typeParametersEnabled = (flags & TypeParameters) != 0,
      implicitParametersEnabled = (flags & ImplicitParameters) != 0,
      byNameParametersEnabled = (flags & ByNameParameters) != 0,
      implicitConversionsEnabled = (flags & ImplicitConversions) != 0,
      namedParametersEnabled = (flags & NamedParameters) != 0,
      hintsXRayModeEnabled = (flags & HintsXRayMode) != 0,
      hintsInPatternMatchEnabled = (flags & HintsInPatternMatch) != 0,
      closingLabelsEnabled = (flags & ClosingLabels) != 0
    )
