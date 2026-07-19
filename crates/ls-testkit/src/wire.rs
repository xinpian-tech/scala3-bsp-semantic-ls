//! `Content-Length`-framed JSON-RPC builders and decoders — the one copy of the
//! frame/request/notification/by_id helpers every wire suite used to re-implement.

use std::io::Cursor;

use serde_json::{json, Value};

use ls_server::read_frame;

/// One framed message.
pub fn frame(body: &Value) -> Vec<u8> {
    let text = serde_json::to_string(body).expect("serialize frame body");
    format!("Content-Length: {}\r\n\r\n{}", text.len(), text).into_bytes()
}

/// A framed request.
pub fn request(id: i64, method: &str, params: Value) -> Vec<u8> {
    frame(&json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }))
}

/// A framed notification.
pub fn notification(method: &str, params: Value) -> Vec<u8> {
    frame(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
}

/// Every framed message in `bytes`, decoded in order.
pub fn decode_frames(bytes: Vec<u8>) -> Vec<Value> {
    let mut reader = Cursor::new(bytes);
    let mut out = Vec::new();
    while let Some(body) = read_frame(&mut reader).expect("read frame") {
        out.push(serde_json::from_slice(&body).expect("decode frame body"));
    }
    out
}

/// The response with the given id; panics (with the full transcript) when absent.
pub fn by_id(out: &[Value], id: i64) -> &Value {
    out.iter()
        .find(|r| r["id"] == id)
        .unwrap_or_else(|| panic!("no response for id {id} in {out:?}"))
}

/// The `textDocument/publishDiagnostics` notifications in `out`, in order.
pub fn publishes(out: &[Value]) -> Vec<&Value> {
    out.iter()
        .filter(|m| m["method"] == "textDocument/publishDiagnostics")
        .collect()
}

/// The byte offset just past the `initialized` notification — the split point
/// pumped in-process drivers use to run the async bootstrap between halves.
pub fn split_after_initialized(bytes: &[u8]) -> usize {
    let mut reader = Cursor::new(bytes.to_vec());
    while let Ok(Some(body)) = read_frame(&mut reader) {
        let is_initialized = serde_json::from_slice::<Value>(&body)
            .ok()
            .and_then(|v| v.get("method")?.as_str().map(str::to_string))
            .as_deref()
            == Some("initialized");
        if is_initialized {
            return reader.position() as usize;
        }
    }
    bytes.len()
}
