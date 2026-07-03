package ls.index

/** Typed failures surfaced to LSP as structured errors. No pretend-accurate
  * fallbacks: when semantic truth is unavailable the request fails with one
  * of these.
  */
enum LsError(val message: String):
  case IndexUnavailable(target: String)
      extends LsError(
        s"target $target has no SemanticDB output; workspace symbol, references and rename are disabled for it"
      )
  case StaleIndex(uri: String)
      extends LsError(s"index for $uri is stale (md5 mismatch) and could not be refreshed")
  case CompileFailed(target: String)
      extends LsError(s"buildTarget/compile failed for $target; rename requires a fresh successful compile")
  case RenameRejected(reasons: List[String])
      extends LsError(("rename rejected:" :: reasons.map("  - " + _)).mkString("\n"))
  case PcOnlySymbol()
      extends LsError(
        "This symbol is provided by a PC-only plugin and is not present in fresh SemanticDB. " +
          "Workspace-wide references and cross-file rename are unavailable for this symbol."
      )
  case NoSymbolAtCursor(uri: String, line: Int, character: Int)
      extends LsError(s"no symbol occurrence at $uri:$line:$character")
  case NotIndexed(uri: String)
      extends LsError(s"$uri is not part of any indexed build target")
  case NoSemanticdb(uri: String)
      extends LsError(
        s"$uri has no SemanticDB output; every source must be compiled with -Xsemanticdb"
      )

final class LsException(val error: LsError) extends RuntimeException(error.message)
