//! The LSP JSON-RPC transport: `Content-Length` message framing over a byte
//! stream plus the request / notification / response message model. The Scala
//! server delegates framing and dispatch to LSP4J; there is no such library
//! here, so both are hand-rolled over `serde_json`, following the LSP base
//! protocol.

use std::io::{self, BufRead, Write};

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// A JSON-RPC request id: an integer or a string (LSP permits either).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum RequestId {
    Number(i64),
    String(String),
}

/// The JSON-RPC and LSP error codes the server returns.
pub mod error_codes {
    pub const PARSE_ERROR: i64 = -32700;
    pub const INVALID_REQUEST: i64 = -32600;
    pub const METHOD_NOT_FOUND: i64 = -32601;
    pub const INVALID_PARAMS: i64 = -32602;
    pub const INTERNAL_ERROR: i64 = -32603;
    /// LSP `ServerNotInitialized`: a request arrived before `initialize`.
    pub const SERVER_NOT_INITIALIZED: i64 = -32002;
    /// LSP `RequestFailed`: a request that failed for a known reason (the
    /// not-ready gate for references/rename returns this).
    pub const REQUEST_FAILED: i64 = -32803;
}

/// The error object of a failed JSON-RPC response.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResponseError {
    pub code: i64,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl ResponseError {
    pub fn new(code: i64, message: impl Into<String>) -> ResponseError {
        ResponseError {
            code,
            message: message.into(),
            data: None,
        }
    }
}

/// A request: carries an id and expects a response.
#[derive(Clone, Debug, PartialEq)]
pub struct Request {
    pub id: RequestId,
    pub method: String,
    pub params: Value,
}

/// A notification: no id, no response.
#[derive(Clone, Debug, PartialEq)]
pub struct Notification {
    pub method: String,
    pub params: Value,
}

/// An inbound RESPONSE from the client — a reply to a server-to-client request:
/// an `id` plus `result` or `error`, and no `method`. The server issues no such
/// requests yet, so the loop consumes and drops these; recognizing the frame
/// (instead of answering it with a null-id INVALID_REQUEST error) is base-
/// protocol robustness and the prerequisite for dynamic registration.
#[derive(Clone, Debug, PartialEq)]
pub struct ClientResponse {
    /// The correlating id (`None` for a null id).
    pub id: Option<RequestId>,
    pub result: Option<Value>,
    pub error: Option<Value>,
}

/// An inbound message: a request (has a method and an id), a notification (a
/// method, no id), or a client response (`result`/`error`, no method).
#[derive(Clone, Debug, PartialEq)]
pub enum Incoming {
    Request(Request),
    Notification(Notification),
    Response(ClientResponse),
}

/// An outbound response. `result` and `error` are mutually exclusive; exactly
/// one is present.
#[derive(Clone, Debug, Serialize)]
pub struct Response {
    pub jsonrpc: &'static str,
    pub id: RequestId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<ResponseError>,
}

impl Response {
    /// A successful response. A `null` result is preserved (serialized as
    /// `"result": null`), which LSP uses for the null-answering methods.
    pub fn success(id: RequestId, result: Value) -> Response {
        Response {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    pub fn failure(id: RequestId, error: ResponseError) -> Response {
        Response {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(error),
        }
    }
}

/// Reads one framed message body from `reader`, or `None` at a clean EOF between
/// messages. The framing is the LSP base protocol: `Content-Length`-prefixed
/// headers terminated by a blank line, then exactly that many body bytes.
pub fn read_frame(reader: &mut impl BufRead) -> io::Result<Option<Vec<u8>>> {
    let mut content_length: Option<usize> = None;
    let mut saw_header = false;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return if saw_header {
                Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "stream ended in the middle of a message header",
                ))
            } else {
                Ok(None)
            };
        }
        let header = line.trim_end_matches(['\r', '\n']);
        if header.is_empty() {
            break;
        }
        saw_header = true;
        if let Some(value) = header_value(header, "Content-Length") {
            content_length = Some(value.trim().parse().map_err(|_| {
                io::Error::new(io::ErrorKind::InvalidData, "invalid Content-Length header")
            })?);
        }
        // Any other header (e.g. Content-Type) is ignored.
    }
    let len = content_length.ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidData, "missing Content-Length header")
    })?;
    let mut body = vec![0u8; len];
    reader.read_exact(&mut body)?;
    Ok(Some(body))
}

/// Splits a `Name: value` header line, returning the value when the name matches
/// case-insensitively.
fn header_value<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let (key, value) = line.split_once(':')?;
    key.trim().eq_ignore_ascii_case(name).then_some(value)
}

/// Parses a framed body into an [`Incoming`] message: a JSON object with a
/// `method` and a non-null `id` is a request, with a `method` and no id a
/// notification, and with no `method` but a `result` or `error` an inbound
/// client [`ClientResponse`]. Returns a [`ResponseError`] for a body that is
/// not a well-formed JSON-RPC message.
pub fn parse_incoming(body: &[u8]) -> Result<Incoming, ResponseError> {
    let value: Value = serde_json::from_slice(body)
        .map_err(|e| ResponseError::new(error_codes::PARSE_ERROR, format!("invalid json: {e}")))?;
    let object = value.as_object().ok_or_else(|| {
        ResponseError::new(error_codes::INVALID_REQUEST, "message is not a json object")
    })?;
    let Some(method) = object.get("method").and_then(Value::as_str) else {
        // No method: an inbound client RESPONSE frame when it carries `result`
        // or `error`; anything else is malformed.
        if !object.contains_key("method")
            && (object.contains_key("result") || object.contains_key("error"))
        {
            let id = match object.get("id") {
                None | Some(Value::Null) => None,
                Some(id) => Some(serde_json::from_value(id.clone()).map_err(|_| {
                    ResponseError::new(error_codes::INVALID_REQUEST, "invalid response id")
                })?),
            };
            return Ok(Incoming::Response(ClientResponse {
                id,
                result: object.get("result").cloned(),
                error: object.get("error").cloned(),
            }));
        }
        return Err(ResponseError::new(
            error_codes::INVALID_REQUEST,
            "message has no method",
        ));
    };
    let method = method.to_string();
    let params = object.get("params").cloned().unwrap_or(Value::Null);
    match object.get("id") {
        None | Some(Value::Null) => Ok(Incoming::Notification(Notification { method, params })),
        Some(id) => {
            let id: RequestId = serde_json::from_value(id.clone()).map_err(|_| {
                ResponseError::new(error_codes::INVALID_REQUEST, "invalid request id")
            })?;
            Ok(Incoming::Request(Request { id, method, params }))
        }
    }
}

/// Writes one framed message to `writer` with LSP `Content-Length` framing.
pub fn write_frame(writer: &mut impl Write, message: &impl Serialize) -> io::Result<()> {
    let body = serde_json::to_vec(message).map_err(io::Error::other)?;
    write!(writer, "Content-Length: {}\r\n\r\n", body.len())?;
    writer.write_all(&body)?;
    writer.flush()
}

/// Writes an error response with a `null` id, for a frame that could not be
/// parsed into a request (so no id is available to correlate the reply).
pub fn write_null_id_error(writer: &mut impl Write, error: &ResponseError) -> io::Result<()> {
    #[derive(Serialize)]
    struct NullId<'a> {
        jsonrpc: &'static str,
        id: (),
        error: &'a ResponseError,
    }
    write_frame(
        writer,
        &NullId {
            jsonrpc: "2.0",
            id: (),
            error,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::io::Cursor;

    fn frame(body: &str) -> Vec<u8> {
        format!("Content-Length: {}\r\n\r\n{}", body.len(), body).into_bytes()
    }

    #[test]
    fn write_then_read_round_trips_a_response() {
        let mut buffer = Vec::new();
        let response = Response::success(RequestId::Number(7), json!({"ok": true}));
        write_frame(&mut buffer, &response).unwrap();

        let mut reader = Cursor::new(buffer);
        let body = read_frame(&mut reader).unwrap().unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["jsonrpc"], "2.0");
        assert_eq!(value["id"], 7);
        assert_eq!(value["result"]["ok"], true);
        assert!(value.get("error").is_none());
        // Nothing left after the single frame.
        assert!(read_frame(&mut reader).unwrap().is_none());
    }

    #[test]
    fn a_null_result_is_serialized_present() {
        let mut buffer = Vec::new();
        write_frame(
            &mut buffer,
            &Response::success(RequestId::Number(1), Value::Null),
        )
        .unwrap();
        let text = String::from_utf8(buffer).unwrap();
        assert!(text.contains("\"result\":null"), "{text}");
        assert!(!text.contains("error"), "{text}");
    }

    #[test]
    fn reads_two_concatenated_messages_then_eof() {
        let mut bytes = frame(r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#);
        bytes.extend(frame(r#"{"jsonrpc":"2.0","method":"exit"}"#));
        let mut reader = Cursor::new(bytes);

        let first = parse_incoming(&read_frame(&mut reader).unwrap().unwrap()).unwrap();
        match first {
            Incoming::Request(r) => {
                assert_eq!(r.id, RequestId::Number(1));
                assert_eq!(r.method, "initialize");
            }
            other => panic!("expected a request, got {other:?}"),
        }
        let second = parse_incoming(&read_frame(&mut reader).unwrap().unwrap()).unwrap();
        assert_eq!(
            second,
            Incoming::Notification(Notification {
                method: "exit".to_string(),
                params: Value::Null,
            })
        );
        assert!(read_frame(&mut reader).unwrap().is_none());
    }

    #[test]
    fn a_string_id_request_parses() {
        let body = r#"{"jsonrpc":"2.0","id":"abc","method":"shutdown"}"#;
        match parse_incoming(body.as_bytes()).unwrap() {
            Incoming::Request(r) => assert_eq!(r.id, RequestId::String("abc".to_string())),
            other => panic!("expected a request, got {other:?}"),
        }
    }

    #[test]
    fn a_null_id_is_a_notification() {
        let body = r#"{"jsonrpc":"2.0","id":null,"method":"initialized","params":{}}"#;
        assert!(matches!(
            parse_incoming(body.as_bytes()).unwrap(),
            Incoming::Notification(_)
        ));
    }

    #[test]
    fn missing_content_length_is_an_error() {
        let mut reader = Cursor::new(b"Content-Type: x\r\n\r\n{}".to_vec());
        assert!(read_frame(&mut reader).is_err());
    }

    #[test]
    fn header_names_are_case_insensitive() {
        let mut reader = Cursor::new(b"content-length: 2\r\n\r\n{}".to_vec());
        let body = read_frame(&mut reader).unwrap().unwrap();
        assert_eq!(body, b"{}");
    }

    #[test]
    fn invalid_json_is_a_parse_error() {
        let err = parse_incoming(b"not json").unwrap_err();
        assert_eq!(err.code, error_codes::PARSE_ERROR);
    }

    #[test]
    fn a_message_without_a_method_is_an_invalid_request() {
        // No method AND no result/error: malformed, not a client response.
        let err = parse_incoming(br#"{"jsonrpc":"2.0","id":1}"#).unwrap_err();
        assert_eq!(err.code, error_codes::INVALID_REQUEST);
    }

    // An inbound client RESPONSE frame (a reply to a server-to-client request):
    // id + result, no method. Parsed as `Incoming::Response`, never answered
    // with a null-id INVALID_REQUEST error.
    #[test]
    fn an_id_result_frame_without_a_method_is_a_client_response() {
        let body = r#"{"jsonrpc":"2.0","id":7,"result":{"ok":true}}"#;
        match parse_incoming(body.as_bytes()).unwrap() {
            Incoming::Response(response) => {
                assert_eq!(response.id, Some(RequestId::Number(7)));
                assert_eq!(response.result, Some(json!({ "ok": true })));
                assert_eq!(response.error, None);
            }
            other => panic!("expected a client response, got {other:?}"),
        }
    }

    #[test]
    fn an_id_error_frame_without_a_method_is_a_client_response() {
        let body = r#"{"jsonrpc":"2.0","id":"r1","error":{"code":-32601,"message":"nope"}}"#;
        match parse_incoming(body.as_bytes()).unwrap() {
            Incoming::Response(response) => {
                assert_eq!(response.id, Some(RequestId::String("r1".to_string())));
                assert_eq!(response.result, None);
                assert_eq!(response.error.unwrap()["code"], -32601);
            }
            other => panic!("expected a client response, got {other:?}"),
        }
    }
}
