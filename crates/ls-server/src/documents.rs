//! The open-buffer store. Full text sync: each `didOpen`/`didChange` replaces
//! the buffer text; `didClose` drops it. A buffer is dirty when its in-memory
//! text differs from the file re-read from disk on each check (so an edit that
//! is undone back to the on-disk contents reads clean again).

use std::collections::HashMap;
use std::sync::Mutex;

use ls_index_model::uri::uri_to_path;

/// A concurrent map of open document URIs to their current buffer text. Shared
/// across request threads behind `&self` (mirroring the Scala `TrieMap`).
pub struct DocumentStore {
    buffers: Mutex<HashMap<String, String>>,
}

impl DocumentStore {
    pub fn new() -> DocumentStore {
        DocumentStore {
            buffers: Mutex::new(HashMap::new()),
        }
    }

    pub fn open(&self, uri: &str, text: &str) {
        self.buffers
            .lock()
            .unwrap()
            .insert(uri.to_string(), text.to_string());
    }

    /// Full-sync change: replace the whole buffer.
    pub fn change(&self, uri: &str, text: &str) {
        self.open(uri, text);
    }

    pub fn close(&self, uri: &str) {
        self.buffers.lock().unwrap().remove(uri);
    }

    pub fn text(&self, uri: &str) -> Option<String> {
        self.buffers.lock().unwrap().get(uri).cloned()
    }

    pub fn is_open(&self, uri: &str) -> bool {
        self.buffers.lock().unwrap().contains_key(uri)
    }

    /// Every open URI, sorted for deterministic replay.
    pub fn open_uris(&self) -> Vec<String> {
        let mut uris: Vec<String> = self.buffers.lock().unwrap().keys().cloned().collect();
        uris.sort();
        uris
    }

    /// Dirty = the buffer differs from the file on disk (re-read now). A URI with
    /// no open buffer is never dirty; a buffer whose file is missing IS dirty.
    pub fn is_dirty(&self, uri: &str) -> bool {
        match self.text(uri) {
            None => false,
            Some(text) => self.disk_text(uri).as_deref() != Some(text.as_str()),
        }
    }

    /// The file's contents (UTF-8, lossy for malformed bytes like Java's
    /// `new String(bytes, UTF_8)`), or `None` if it is not a readable regular
    /// file.
    pub fn disk_text(&self, uri: &str) -> Option<String> {
        let path = uri_to_path(uri).ok()?;
        if !path.is_file() {
            return None;
        }
        std::fs::read(&path)
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
    }
}

impl Default for DocumentStore {
    fn default() -> Self {
        DocumentStore::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ls_index_model::uri::path_to_uri;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn open_change_close_track_the_buffer() {
        let docs = DocumentStore::new();
        assert!(!docs.is_open("file:///a.scala"));
        docs.open("file:///a.scala", "one");
        assert!(docs.is_open("file:///a.scala"));
        assert_eq!(docs.text("file:///a.scala").as_deref(), Some("one"));
        docs.change("file:///a.scala", "two");
        assert_eq!(docs.text("file:///a.scala").as_deref(), Some("two"));
        docs.close("file:///a.scala");
        assert!(!docs.is_open("file:///a.scala"));
        assert_eq!(docs.text("file:///a.scala"), None);
    }

    #[test]
    fn open_uris_are_sorted() {
        let docs = DocumentStore::new();
        docs.open("file:///b.scala", "");
        docs.open("file:///a.scala", "");
        assert_eq!(
            docs.open_uris(),
            vec!["file:///a.scala".to_string(), "file:///b.scala".to_string()]
        );
    }

    #[test]
    fn an_unopened_uri_is_never_dirty() {
        let docs = DocumentStore::new();
        assert!(!docs.is_dirty("file:///missing.scala"));
    }

    #[test]
    fn buffer_matching_disk_is_clean_and_a_divergence_is_dirty() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.scala");
        fs::write(&path, "object A").unwrap();
        let uri = path_to_uri(&path);

        let docs = DocumentStore::new();
        docs.open(&uri, "object A");
        assert!(!docs.is_dirty(&uri), "buffer equals disk => clean");

        docs.change(&uri, "object B");
        assert!(docs.is_dirty(&uri), "buffer diverges from disk => dirty");
    }

    #[test]
    fn typing_and_undoing_back_to_disk_reads_clean() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("a.scala");
        fs::write(&path, "val x = 1\n").unwrap();
        let uri = path_to_uri(&path);

        let docs = DocumentStore::new();
        docs.open(&uri, "val x = 2\n");
        assert!(docs.is_dirty(&uri));
        docs.change(&uri, "val x = 1\n"); // undo back to the on-disk contents
        assert!(!docs.is_dirty(&uri));
    }

    #[test]
    fn a_buffer_whose_file_is_absent_is_dirty() {
        let dir = tempdir().unwrap();
        let uri = path_to_uri(&dir.path().join("never-written.scala"));
        let docs = DocumentStore::new();
        docs.open(&uri, "content");
        assert!(docs.is_dirty(&uri));
    }
}
