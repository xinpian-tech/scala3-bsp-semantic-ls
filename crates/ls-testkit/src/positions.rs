//! Source-position helpers over on-disk texts — token-anchored positions so
//! wire assertions survive fixture edits (no hard-coded line/column literals).

use std::path::Path;

use serde_json::{json, Value};

pub fn source_text(ws: &Path, rel: &str) -> String {
    std::fs::read_to_string(ws.join(rel)).unwrap()
}

/// The 0-based (line, character) of the start of the `nth` occurrence of `token`.
pub fn position_of(text: &str, token: &str, nth: usize) -> (u32, u32) {
    let mut seen = 0usize;
    for (line_no, line) in text.lines().enumerate() {
        let mut from = 0usize;
        while let Some(rel) = line[from..].find(token) {
            let col = from + rel;
            if seen == nth {
                return (line_no as u32, col as u32);
            }
            seen += 1;
            from = col + token.len();
        }
    }
    panic!("token {token:?} occurrence {nth} not found");
}

pub fn position_json(line: u32, character: u32) -> Value {
    json!({"line": line, "character": character})
}

/// The LSP range span of the `nth` occurrence of a single-line `token`.
pub fn span_of(text: &str, token: &str, nth: usize) -> Value {
    let (line, col) = position_of(text, token, nth);
    json!({
        "start": {"line": line, "character": col},
        "end": {"line": line, "character": col + token.len() as u32},
    })
}

/// How many whole occurrences of `token` appear across the given files.
pub fn count_token(ws: &Path, files: &[&str], token: &str) -> usize {
    files
        .iter()
        .map(|rel| source_text(ws, rel).matches(token).count())
        .sum()
}
