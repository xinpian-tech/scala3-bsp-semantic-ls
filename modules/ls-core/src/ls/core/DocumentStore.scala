package ls.core

import java.nio.charset.StandardCharsets
import java.nio.file.{Files, Path}

import scala.collection.concurrent.TrieMap
import scala.util.control.NonFatal

/** Open-editor-buffer store keyed by normalized `file://` URI.
  *
  * Dirty tracking follows the plan-12.1 definition exactly: a buffer is
  * *dirty* when its text differs from the file on disk (not merely "has seen
  * a didChange"): a didChange that types and then undoes back to the disk
  * content leaves the buffer clean, and a didSave that makes disk equal the
  * buffer clears dirtiness with no extra bookkeeping. The disk is re-read on
  * each check so external file changes are never missed; workspace sources
  * are small and this sits far from any postings hot path.
  */
final class DocumentStore:

  private val buffers = TrieMap.empty[String, String]

  def open(uri: String, text: String): Unit = buffers.put(uri, text)

  /** Replaces the full text (full-sync). Unknown uris are opened implicitly
    * so a lost didOpen cannot wedge the session.
    */
  def change(uri: String, text: String): Unit = buffers.put(uri, text)

  def close(uri: String): Unit = buffers.remove(uri)

  def text(uri: String): Option[String] = buffers.get(uri)

  def isOpen(uri: String): Boolean = buffers.contains(uri)

  def openUris: Vector[String] = buffers.keySet.toVector.sorted

  /** True when the open buffer for `uri` differs from the file on disk
    * (a missing or unreadable file counts as different). Closed uris are
    * never dirty.
    */
  def isDirty(uri: String): Boolean =
    buffers.get(uri) match
      case None => false
      case Some(text) => !diskText(uri).contains(text)

  def diskText(uri: String): Option[String] =
    try
      val path: Path = Uris.toPath(uri)
      if Files.isRegularFile(path) then
        Some(new String(Files.readAllBytes(path), StandardCharsets.UTF_8))
      else None
    catch case NonFatal(_) => None
