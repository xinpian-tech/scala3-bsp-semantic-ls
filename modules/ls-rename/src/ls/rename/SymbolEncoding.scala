package ls.rename

import ls.index.{DocId, SymbolKey}
import ls.semanticdb.SymbolStrings

/** Canonical encoding of a [[ls.index.SymbolKey]] into the single-string
  * symbol dictionary of a postings segment.
  *
  * Global symbols are stored verbatim. SemanticDB local symbols (`local0`,
  * `local1`, ...) are only unique within one document, but the segment symbol
  * dictionary requires globally distinct strings, so locals are qualified
  * with the persistent doc id: `local0@42`. The `@` marker cannot appear in a
  * raw SemanticDB local symbol (those are `local` + digits) and global
  * symbols always contain a descriptor boundary character, so the encoding is
  * collision-free and reversible.
  */
object SymbolEncoding:
  private inline val Sep = '@'

  def encode(key: SymbolKey): String =
    key.localDoc match
      case Some(doc) => s"${key.semanticSymbol}$Sep${doc.value}"
      case None => key.semanticSymbol

  def encode(semanticSymbol: String, localDocId: Option[Long]): String =
    localDocId match
      case Some(id) if SymbolStrings.isLocal(semanticSymbol) => s"$semanticSymbol$Sep$id"
      case _ => semanticSymbol

  /** Inverse of [[encode]]: (raw semantic symbol, local doc id when local). */
  def decode(encoded: String): (String, Option[Long]) =
    if encoded.startsWith("local") then
      val at = encoded.indexOf(Sep)
      if at > 0 then
        encoded.substring(at + 1).toLongOption match
          case Some(docId) => (encoded.substring(0, at), Some(docId))
          case None => (encoded, None)
      else (encoded, None)
    else (encoded, None)

  def toKey(encoded: String): SymbolKey =
    val (raw, doc) = decode(encoded)
    doc match
      case Some(id) => SymbolKey.local(raw, DocId(id))
      case None => SymbolKey.global(raw)
