package ls.bsp

/** Typed failures of the BSP client layer. Protocol-level problems surface as
  * one of these (wrapped in [[BspException]]); domain-level results such as a
  * failed compile stay in typed result values (see [[BspCompileOutcome]]).
  */
enum BspError(val message: String):
  case NoConnectionFile(workspaceRoot: String)
      extends BspError(s"no usable .bsp/*.json connection file under $workspaceRoot")
  case InvalidConnectionFile(path: String, detail: String)
      extends BspError(s"invalid BSP connection file $path: $detail")
  case LaunchFailed(server: String, detail: String)
      extends BspError(s"failed to launch BSP server '$server': $detail")
  case RequestTimeout(method: String, timeoutMillis: Long)
      extends BspError(s"BSP request $method timed out after ${timeoutMillis}ms")
  case RequestFailed(method: String, detail: String)
      extends BspError(s"BSP request $method failed: $detail")
  case InvalidResponse(method: String, detail: String)
      extends BspError(s"BSP response for $method is invalid: $detail")
  case SessionClosed(method: String)
      extends BspError(s"BSP session is closed; cannot send $method")

final class BspException(val error: BspError) extends RuntimeException(error.message)
