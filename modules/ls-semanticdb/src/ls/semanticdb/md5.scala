package ls.semanticdb

import java.nio.charset.StandardCharsets
import java.security.MessageDigest

/** Result of comparing current source text against a TextDocument's stored
  * md5. Anything but [[FreshnessCheck.Fresh]] means the SemanticDB document
  * must not be used as semantic truth for that source.
  */
enum FreshnessCheck:
  case Fresh
  /** The document carries no md5 at all; cannot prove freshness. */
  case MissingMd5
  case Stale(documentMd5: String, sourceMd5: String)

  def isFresh: Boolean = this match
    case Fresh => true
    case MissingMd5 => false
    case Stale(_, _) => false

object Md5:
  private val HexDigits = "0123456789ABCDEF".toCharArray

  /** Uppercase-hex MD5 of the UTF-8 bytes of `text` — scalameta's convention
    * for `TextDocument.md5`.
    */
  def computeHex(text: String): String =
    val digest = MessageDigest.getInstance("MD5").digest(text.getBytes(StandardCharsets.UTF_8))
    val out = new Array[Char](digest.length * 2)
    var i = 0
    while i < digest.length do
      val b = digest(i) & 0xff
      out(i * 2) = HexDigits(b >>> 4)
      out(i * 2 + 1) = HexDigits(b & 0xf)
      i += 1
    new String(out)

  /** Compares `sourceText` against a stored md5 (case-insensitively, since
    * the spec convention is uppercase but we do not amplify case bugs into
    * staleness).
    */
  def validate(sourceText: String, documentMd5: String): FreshnessCheck =
    if documentMd5.isEmpty then FreshnessCheck.MissingMd5
    else
      val actual = computeHex(sourceText)
      if actual.equalsIgnoreCase(documentMd5) then FreshnessCheck.Fresh
      else FreshnessCheck.Stale(documentMd5, actual)

  def validate(sourceText: String, doc: SdbDocument): FreshnessCheck =
    validate(sourceText, doc.md5)

  def validate(sourceText: String, doc: ls.index.NormalizedDocument): FreshnessCheck =
    validate(sourceText, doc.md5)
