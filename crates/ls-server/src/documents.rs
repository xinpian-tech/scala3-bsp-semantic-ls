//! The open-buffer store and the incremental text-sync fold. `didOpen` seeds a
//! buffer; `didChange` folds LSP `contentChanges` events (incremental sync,
//! `TextDocumentSyncKind.Incremental`) onto the current text via
//! [`apply_content_changes`]; `didClose` drops it. Everything downstream of the
//! store stays full-text: the fold reduces a change list to the whole post-edit
//! document. A buffer is dirty when its in-memory text differs from the file
//! re-read from disk on each check (so an edit that is undone back to the
//! on-disk contents reads clean again).
//!
//! Positions are interpreted in UTF-16 code units, the LSP default encoding the
//! server advertises (`positionEncoding: "utf-16"`). The UTF-16 line/column ↔
//! byte-offset arithmetic is `line-index` (the rust-analyzer extraction); this
//! module adds only the LSP-client leniency around its exact lookups:
//! out-of-range lines/columns clamp to the document/line end, and a position
//! landing mid-surrogate rounds DOWN to the char boundary (VS Code leniency).
//! Lone `\r` line breaks are a documented non-goal: `\r` outside `\r\n` is
//! ordinary in-line content.

use std::collections::HashMap;
use std::sync::Mutex;

use line_index::{LineIndex, WideEncoding, WideLineCol};
use serde::Deserialize;

use ls_index_model::uri::uri_to_path;

use crate::protocol::{Position, Range};

/// One `contentChanges` event from a `textDocument/didChange` notification. An
/// event without a `range` replaces the whole document (clients may mix both
/// forms in one notification); a ranged event's range addresses the text as
/// ALREADY EDITED by the preceding events in the same list. The deprecated
/// `rangeLength` field is ignored.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
pub struct ContentChange {
    #[serde(default)]
    pub range: Option<Range>,
    pub text: String,
}

/// Folds a `didChange` event list, in order, onto `base`, returning the full
/// post-edit document. Each ranged event addresses the already-edited text;
/// a rangeless event replaces the whole document.
pub fn apply_content_changes(base: &str, changes: &[ContentChange]) -> String {
    let mut text = base.to_string();
    for change in changes {
        match change.range {
            None => {
                text.clear();
                text.push_str(&change.text);
            }
            Some(range) => {
                let index = LineIndex::new(&text);
                let start = byte_offset(&index, &text, range.start);
                let end = byte_offset(&index, &text, range.end);
                // A reversed range from a lenient client is applied, not dropped.
                let (start, end) = if start <= end {
                    (start, end)
                } else {
                    (end, start)
                };
                text.replace_range(start..end, &change.text);
            }
        }
    }
    text
}

/// The byte offset of a UTF-16 `(line, character)` LSP position in `text`,
/// with client leniency wrapped around the exact `line-index` lookups:
/// a line past the document end addresses the document end, a column past the
/// line end addresses the line end (before its `\n`/`\r\n` terminator), and a
/// position landing between the two units of a surrogate pair rounds DOWN to
/// the character boundary.
fn byte_offset(index: &LineIndex, text: &str, position: Position) -> usize {
    // Line clamp: `LineIndex::line` is None past the last line.
    let Some(line_range) = index.line(position.line) else {
        return text.len();
    };
    let line_start = usize::from(line_range.start());
    let line_end = usize::from(line_range.end());
    // The line's content, excluding its terminator: columns never address past
    // the line break (a `\r` NOT followed by `\n` is ordinary content).
    let raw = &text[line_start..line_end];
    let content = raw
        .strip_suffix('\n')
        .map(|s| s.strip_suffix('\r').unwrap_or(s))
        .unwrap_or(raw);
    // Column clamp, in UTF-16 units, to the line-content width.
    let width = WideEncoding::Utf16.measure(content) as u32;
    let col = position.character.min(width);
    // Exact UTF-16 -> UTF-8 column conversion is line-index's.
    let offset = index
        .to_utf8(
            WideEncoding::Utf16,
            WideLineCol {
                line: position.line,
                col,
            },
        )
        .and_then(|line_col| index.offset(line_col))
        .map(usize::from)
        .unwrap_or(line_end)
        .min(text.len());
    // Mid-surrogate leniency: for a column between the two UTF-16 units of an
    // astral char, `to_utf8` lands mid-character in UTF-8 (line-index does not
    // model half positions) — round DOWN to the char boundary, as VS Code does.
    let mut offset = offset;
    while !text.is_char_boundary(offset) {
        offset -= 1;
    }
    offset
}

/// One open document: the buffer text plus the last `didOpen`/`didChange`
/// version the client reported for it.
struct Buffer {
    text: String,
    version: i64,
}

/// A concurrent map of open document URIs to their current buffer. Shared
/// across request threads behind `&self` (mirroring the Scala `TrieMap`).
pub struct DocumentStore {
    buffers: Mutex<HashMap<String, Buffer>>,
}

impl DocumentStore {
    pub fn new() -> DocumentStore {
        DocumentStore {
            buffers: Mutex::new(HashMap::new()),
        }
    }

    pub fn open(&self, uri: &str, text: &str) {
        self.open_versioned(uri, text, 0);
    }

    /// `didOpen` with the notification's `textDocument.version`.
    pub fn open_versioned(&self, uri: &str, text: &str, version: i64) {
        self.buffers.lock().unwrap().insert(
            uri.to_string(),
            Buffer {
                text: text.to_string(),
                version,
            },
        );
    }

    /// Full-text replace outside the `didChange` protocol (`didSave` refresh,
    /// tests): swaps the text, keeping the stored version.
    pub fn change(&self, uri: &str, text: &str) {
        let mut buffers = self.buffers.lock().unwrap();
        match buffers.get_mut(uri) {
            Some(buffer) => buffer.text = text.to_string(),
            None => {
                buffers.insert(
                    uri.to_string(),
                    Buffer {
                        text: text.to_string(),
                        version: 0,
                    },
                );
            }
        }
    }

    /// Applies a `didChange` event list to the buffer and stores the result,
    /// returning the full post-edit text (the downstream seams stay full-text).
    ///
    /// A non-monotonic `version` is logged and APPLIED anyway — the server must
    /// never let a client version quirk desync the buffer. Returns `None` only
    /// when a RANGED change arrives for a buffer that was never opened (there is
    /// no base text to edit; the caller drops the notification with a log); an
    /// all-rangeless list for an unopened URI still opens it, preserving the
    /// full-sync behavior for clients that never sent `didOpen`.
    pub fn apply_changes(
        &self,
        uri: &str,
        version: Option<i64>,
        changes: &[ContentChange],
    ) -> Option<String> {
        let mut buffers = self.buffers.lock().unwrap();
        match buffers.get_mut(uri) {
            Some(buffer) => {
                if let Some(version) = version {
                    if version <= buffer.version {
                        eprintln!(
                            "ls-server: didChange version {version} for {uri} is not newer than \
                             the stored {}; applying the edits anyway",
                            buffer.version
                        );
                    }
                    buffer.version = version;
                }
                let text = apply_content_changes(&buffer.text, changes);
                buffer.text = text.clone();
                Some(text)
            }
            None if changes.iter().any(|change| change.range.is_some()) => None,
            None => {
                let text = apply_content_changes("", changes);
                buffers.insert(
                    uri.to_string(),
                    Buffer {
                        text: text.clone(),
                        version: version.unwrap_or(0),
                    },
                );
                Some(text)
            }
        }
    }

    pub fn close(&self, uri: &str) {
        self.buffers.lock().unwrap().remove(uri);
    }

    pub fn text(&self, uri: &str) -> Option<String> {
        self.buffers
            .lock()
            .unwrap()
            .get(uri)
            .map(|buffer| buffer.text.clone())
    }

    /// The stored version of an open buffer (test observability).
    pub fn version(&self, uri: &str) -> Option<i64> {
        self.buffers
            .lock()
            .unwrap()
            .get(uri)
            .map(|buffer| buffer.version)
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

    fn ranged(
        start_line: u32,
        start_char: u32,
        end_line: u32,
        end_char: u32,
        text: &str,
    ) -> ContentChange {
        ContentChange {
            range: Some(Range {
                start: Position {
                    line: start_line,
                    character: start_char,
                },
                end: Position {
                    line: end_line,
                    character: end_char,
                },
            }),
            text: text.to_string(),
        }
    }

    fn full(text: &str) -> ContentChange {
        ContentChange {
            range: None,
            text: text.to_string(),
        }
    }

    fn apply(base: &str, changes: &[ContentChange]) -> String {
        apply_content_changes(base, changes)
    }

    // --- apply_content_changes: the edit-application matrix ---------------------

    #[test]
    fn inserts_at_start_middle_and_end() {
        assert_eq!(apply("hello", &[ranged(0, 0, 0, 0, "X")]), "Xhello");
        assert_eq!(apply("hello", &[ranged(0, 2, 0, 2, "X")]), "heXllo");
        assert_eq!(apply("hello", &[ranged(0, 5, 0, 5, "X")]), "helloX");
    }

    #[test]
    fn deletes_at_start_middle_and_end() {
        assert_eq!(apply("hello", &[ranged(0, 0, 0, 1, "")]), "ello");
        assert_eq!(apply("hello", &[ranged(0, 2, 0, 3, "")]), "helo");
        assert_eq!(apply("hello", &[ranged(0, 4, 0, 5, "")]), "hell");
    }

    #[test]
    fn replaces_at_start_middle_and_end() {
        assert_eq!(apply("hello", &[ranged(0, 0, 0, 2, "J")]), "Jllo");
        assert_eq!(apply("hello", &[ranged(0, 1, 0, 4, "ipp")]), "hippo");
        assert_eq!(apply("hello", &[ranged(0, 3, 0, 5, "p!")]), "help!");
    }

    #[test]
    fn multi_line_ranges_delete_and_replace_across_lines() {
        let base = "one\ntwo\nthree\n";
        // Delete from mid-line-0 to mid-line-2.
        assert_eq!(apply(base, &[ranged(0, 1, 2, 3, "")]), "oee\n");
        // Replace a cross-line span with multi-line text.
        assert_eq!(
            apply(base, &[ranged(0, 3, 2, 0, "!\nTWO\n")]),
            "one!\nTWO\nthree\n"
        );
        // Join two lines by deleting the line break.
        assert_eq!(apply(base, &[ranged(0, 3, 1, 0, "")]), "onetwo\nthree\n");
    }

    #[test]
    fn later_events_in_a_batch_address_the_already_edited_text() {
        // Event 2's range points at the "X" event 1 inserted — valid only
        // against the already-edited text.
        assert_eq!(
            apply("ab", &[ranged(0, 1, 0, 1, "X"), ranged(0, 1, 0, 2, "YZ")]),
            "aYZb"
        );
        // Sequential typing: three inserts, each one column further right.
        assert_eq!(
            apply(
                "",
                &[
                    ranged(0, 0, 0, 0, "a"),
                    ranged(0, 1, 0, 1, "b"),
                    ranged(0, 2, 0, 2, "c"),
                ]
            ),
            "abc"
        );
    }

    #[test]
    fn a_batch_may_mix_a_rangeless_full_replace_with_ranged_events() {
        // A rangeless event resets the document; the following ranged event
        // addresses the reset text.
        assert_eq!(
            apply(
                "junk",
                &[
                    ranged(0, 0, 0, 4, "ignored"),
                    full("line1\nline2\n"),
                    ranged(1, 0, 1, 5, "LINE2"),
                ]
            ),
            "line1\nLINE2\n"
        );
        assert_eq!(apply("old", &[full("new")]), "new");
    }

    #[test]
    fn edits_before_after_and_inside_astral_chars_count_utf16_units() {
        // "a𐐀b": UTF-16 columns are a=0, 𐐀=1..3 (two units), b=3.
        let base = "a\u{10400}b";
        assert_eq!(apply(base, &[ranged(0, 0, 0, 1, "")]), "\u{10400}b");
        assert_eq!(apply(base, &[ranged(0, 3, 0, 4, "")]), "a\u{10400}");
        assert_eq!(apply(base, &[ranged(0, 1, 0, 3, "")]), "ab");
        assert_eq!(apply(base, &[ranged(0, 3, 0, 3, "X")]), "a\u{10400}Xb");
        // Emoji are astral too: 😀 = 2 UTF-16 units.
        assert_eq!(apply("😀x", &[ranged(0, 0, 0, 2, "")]), "x");
        assert_eq!(apply("😀x", &[ranged(0, 2, 0, 3, "y")]), "😀y");
    }

    #[test]
    fn a_mid_surrogate_position_clamps_down_to_the_char_boundary() {
        // Column 2 of "a𐐀b" is between 𐐀's two UTF-16 units: it rounds DOWN to
        // the char boundary (column 1), the VS Code leniency.
        let base = "a\u{10400}b";
        // Delete [mid-surrogate, after-𐐀): start rounds down => 𐐀 is removed.
        assert_eq!(apply(base, &[ranged(0, 2, 0, 3, "")]), "ab");
        // Insert at a mid-surrogate point lands BEFORE the astral char.
        assert_eq!(apply(base, &[ranged(0, 2, 0, 2, "X")]), "aX\u{10400}b");
    }

    #[test]
    fn crlf_documents_edit_and_clamp_before_the_terminator() {
        let base = "ab\r\ncd\r\n";
        assert_eq!(apply(base, &[ranged(1, 1, 1, 1, "X")]), "ab\r\ncXd\r\n");
        // A column past the line end clamps BEFORE the \r\n terminator.
        assert_eq!(apply(base, &[ranged(0, 10, 0, 10, "X")]), "abX\r\ncd\r\n");
        // Deleting across the CRLF joins the lines.
        assert_eq!(apply(base, &[ranged(0, 2, 1, 0, "")]), "abcd\r\n");
    }

    #[test]
    fn a_whole_document_range_deletes_to_empty() {
        assert_eq!(apply("whole\ndoc\n", &[ranged(0, 0, 2, 0, "")]), "");
        assert_eq!(apply("x", &[ranged(0, 0, 0, 1, "")]), "");
    }

    #[test]
    fn out_of_bounds_lines_and_columns_clamp_to_document_and_line_end() {
        // A start line past the document end addresses the document end.
        assert_eq!(apply("ab\n", &[ranged(5, 0, 9, 3, "X")]), "ab\nX");
        // A column past the line end addresses the line end.
        assert_eq!(apply("ab\n", &[ranged(0, 99, 0, 99, "X")]), "abX\n");
        // An end past the document end clamps to the document end.
        assert_eq!(apply("ab", &[ranged(0, 1, 9, 9, "")]), "a");
        // The virtual empty line after a trailing newline is addressable.
        assert_eq!(apply("ab\n", &[ranged(1, 0, 1, 5, "X")]), "ab\nX");
    }

    #[test]
    fn content_changes_parse_from_the_wire_shape() {
        // Mixed ranged + rangeless items; the deprecated rangeLength is ignored.
        let parsed: Vec<ContentChange> = serde_json::from_value(serde_json::json!([
            {
                "range": {
                    "start": { "line": 1, "character": 2 },
                    "end": { "line": 1, "character": 4 }
                },
                "rangeLength": 2,
                "text": "xy"
            },
            { "text": "whole document" }
        ]))
        .unwrap();
        assert_eq!(
            parsed,
            vec![ranged(1, 2, 1, 4, "xy"), full("whole document")]
        );
    }

    // --- the store: versioned buffers over the fold -----------------------------

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
    fn apply_changes_folds_ranged_events_and_tracks_the_version() {
        let docs = DocumentStore::new();
        docs.open_versioned("file:///a.scala", "val x = 1\n", 1);
        assert_eq!(docs.version("file:///a.scala"), Some(1));
        let text = docs
            .apply_changes("file:///a.scala", Some(2), &[ranged(0, 8, 0, 9, "2")])
            .unwrap();
        assert_eq!(text, "val x = 2\n");
        assert_eq!(docs.text("file:///a.scala").as_deref(), Some("val x = 2\n"));
        assert_eq!(docs.version("file:///a.scala"), Some(2));
    }

    #[test]
    fn a_non_monotonic_version_is_applied_anyway() {
        // Log-and-apply: a version quirk must never desync the buffer.
        let docs = DocumentStore::new();
        docs.open_versioned("file:///a.scala", "ab", 5);
        let text = docs
            .apply_changes("file:///a.scala", Some(3), &[ranged(0, 2, 0, 2, "c")])
            .unwrap();
        assert_eq!(text, "abc");
        assert_eq!(docs.version("file:///a.scala"), Some(3));
    }

    #[test]
    fn a_ranged_change_for_a_never_opened_buffer_is_refused() {
        let docs = DocumentStore::new();
        assert_eq!(
            docs.apply_changes("file:///a.scala", Some(1), &[ranged(0, 0, 0, 0, "X")]),
            None
        );
        assert!(!docs.is_open("file:///a.scala"));
        // An all-rangeless list still opens the buffer (full-sync tolerance).
        assert_eq!(
            docs.apply_changes("file:///a.scala", Some(1), &[full("whole")])
                .as_deref(),
            Some("whole")
        );
        assert_eq!(docs.text("file:///a.scala").as_deref(), Some("whole"));
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
        // An incremental edit back to the on-disk contents reads clean.
        docs.apply_changes(&uri, Some(2), &[ranged(0, 8, 0, 9, "1")])
            .unwrap();
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

    // --- proptest: the line-index-based fold vs a naive UTF-16 oracle -----------

    mod oracle {
        use super::*;
        use proptest::prelude::*;

        /// The oracle's position→offset, computed DIRECTLY in UTF-16 unit
        /// space over `Vec<u16>` — independent of line-index and of all byte
        /// arithmetic — with the same documented leniency: line past the end →
        /// document end; column past the line content → line end (before the
        /// `\n`/`\r\n` terminator); a mid-surrogate landing rounds down.
        fn oracle_offset(units: &[u16], line: u32, character: u32) -> usize {
            let mut current = 0u32;
            let mut line_start = 0usize;
            if line > 0 {
                for (i, &unit) in units.iter().enumerate() {
                    if unit == u16::from(b'\n') {
                        current += 1;
                        if current == line {
                            line_start = i + 1;
                            break;
                        }
                    }
                }
                if current != line {
                    return units.len();
                }
            }
            let content_end = match units[line_start..]
                .iter()
                .position(|&unit| unit == u16::from(b'\n'))
            {
                Some(rel) => {
                    let mut end = line_start + rel;
                    if end > line_start && units[end - 1] == u16::from(b'\r') {
                        end -= 1;
                    }
                    end
                }
                None => units.len(),
            };
            let mut offset = line_start + (character as usize).min(content_end - line_start);
            // Mid-surrogate rounds down to the pair's high surrogate.
            if offset < units.len() && (0xDC00..0xE000).contains(&units[offset]) {
                offset -= 1;
            }
            offset
        }

        /// Applies the change list entirely in UTF-16 space.
        fn oracle_apply(base: &str, changes: &[ContentChange]) -> String {
            let mut units: Vec<u16> = base.encode_utf16().collect();
            for change in changes {
                match change.range {
                    None => units = change.text.encode_utf16().collect(),
                    Some(range) => {
                        let start = oracle_offset(&units, range.start.line, range.start.character);
                        let end = oracle_offset(&units, range.end.line, range.end.character);
                        let (start, end) = if start <= end {
                            (start, end)
                        } else {
                            (end, start)
                        };
                        let mut next = units[..start].to_vec();
                        next.extend(change.text.encode_utf16());
                        next.extend_from_slice(&units[end..]);
                        units = next;
                    }
                }
            }
            String::from_utf16(&units).expect("edits never split a surrogate pair")
        }

        /// ASCII, accented, CJK (3-byte), astral (4-byte / 2-unit), `\n`, and a
        /// bare `\r` (ordinary content per the documented non-goal).
        const CHARS: &[char] = &[
            'a',
            'b',
            'Z',
            '0',
            'é',
            'ß',
            '好',
            '\u{10400}',
            '😀',
            ' ',
            '\t',
            '\n',
            '\r',
        ];

        fn text_strategy(max: usize) -> impl Strategy<Value = String> {
            proptest::collection::vec(proptest::sample::select(CHARS), 0..max)
                .prop_map(|chars| chars.into_iter().collect())
        }

        fn change_strategy() -> impl Strategy<Value = ContentChange> {
            prop_oneof![
                1 => text_strategy(12).prop_map(|text| ContentChange { range: None, text }),
                5 => (0u32..8, 0u32..24, 0u32..8, 0u32..24, text_strategy(12)).prop_map(
                    |(start_line, start_char, end_line, end_char, text)| ContentChange {
                        range: Some(Range {
                            start: Position { line: start_line, character: start_char },
                            end: Position { line: end_line, character: end_char },
                        }),
                        text,
                    }
                ),
            ]
        }

        proptest! {
            // Random edit scripts over random documents: the line-index-based
            // implementation and the naive UTF-16 oracle must agree exactly.
            #[test]
            fn random_edit_scripts_match_the_utf16_oracle(
                base in text_strategy(48),
                changes in proptest::collection::vec(change_strategy(), 0..8),
            ) {
                prop_assert_eq!(
                    apply_content_changes(&base, &changes),
                    oracle_apply(&base, &changes)
                );
            }
        }
    }
}
