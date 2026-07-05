//! A minimal JSON-RPC 2.0 client over framed streams: a background reader
//! thread correlates responses to in-flight requests by id and forwards
//! server-originated notifications to a callback. Requests are bounded by a
//! per-call timeout. Rust has no lsp4j, so this stands in for its `Launcher`.

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use serde_json::{json, Value};

use crate::wire;

/// A server -> client notification (a message with a method and no id).
pub struct Notification {
    pub method: String,
    pub params: Value,
}

/// Why a request did not return a result.
pub enum RpcCallError {
    Timeout,
    Failed(String),
}

enum Outcome {
    Result(Value),
    Error(String),
}

type Pending = Arc<Mutex<HashMap<i64, Sender<Outcome>>>>;

pub struct JsonRpcClient {
    writer: Mutex<Box<dyn Write + Send>>,
    pending: Pending,
    next_id: AtomicI64,
}

impl JsonRpcClient {
    /// Spawns the reader thread over `input`; `output` carries client -> server
    /// traffic. `on_notification` runs on the reader thread for every
    /// server-originated notification.
    pub fn start(
        input: Box<dyn Read + Send>,
        output: Box<dyn Write + Send>,
        on_notification: Box<dyn Fn(Notification) + Send>,
    ) -> JsonRpcClient {
        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let reader_pending = Arc::clone(&pending);
        thread::Builder::new()
            .name("bsp-jsonrpc-reader".to_string())
            .spawn(move || read_loop(input, reader_pending, on_notification))
            .expect("spawn bsp reader thread");
        JsonRpcClient {
            writer: Mutex::new(output),
            pending,
            next_id: AtomicI64::new(1),
        }
    }

    /// Sends a request and blocks up to `timeout` for its response.
    pub fn request(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, RpcCallError> {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        let (tx, rx) = mpsc::channel();
        self.pending.lock().unwrap().insert(id, tx);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        if let Err(e) = self.write_message(&msg) {
            self.pending.lock().unwrap().remove(&id);
            return Err(RpcCallError::Failed(format!("write failed: {e}")));
        }
        match rx.recv_timeout(timeout) {
            Ok(Outcome::Result(v)) => Ok(v),
            Ok(Outcome::Error(detail)) => Err(RpcCallError::Failed(detail)),
            Err(RecvTimeoutError::Timeout) => {
                self.pending.lock().unwrap().remove(&id);
                Err(RpcCallError::Timeout)
            }
            Err(RecvTimeoutError::Disconnected) => {
                self.pending.lock().unwrap().remove(&id);
                Err(RpcCallError::Failed(
                    "connection closed before response".to_string(),
                ))
            }
        }
    }

    /// Sends a notification (no response expected).
    pub fn notify(&self, method: &str, params: Value) -> Result<(), RpcCallError> {
        let msg = json!({"jsonrpc": "2.0", "method": method, "params": params});
        self.write_message(&msg)
            .map_err(|e| RpcCallError::Failed(format!("write failed: {e}")))
    }

    fn write_message(&self, msg: &Value) -> std::io::Result<()> {
        let mut writer = self.writer.lock().unwrap();
        wire::write_message(&mut *writer, msg)
    }
}

fn read_loop(
    input: Box<dyn Read + Send>,
    pending: Pending,
    on_notification: Box<dyn Fn(Notification) + Send>,
) {
    let mut reader = BufReader::new(input);
    // Stops on a clean EOF (`Ok(None)`) or any read/parse error.
    while let Ok(Some(msg)) = wire::read_message(&mut reader) {
        dispatch(msg, &pending, on_notification.as_ref());
    }
    // Wake any remaining waiters so they fail fast rather than hang.
    pending.lock().unwrap().clear();
}

fn dispatch(msg: Value, pending: &Pending, on_notification: &(dyn Fn(Notification) + Send)) {
    let id = msg.get("id").and_then(Value::as_i64);
    let has_result = msg.get("result").is_some();
    let has_error = msg.get("error").is_some();

    if let Some(id) = id {
        if has_result || has_error {
            let waiter = pending.lock().unwrap().remove(&id);
            if let Some(tx) = waiter {
                let outcome = if has_error {
                    Outcome::Error(describe_error(msg.get("error")))
                } else {
                    Outcome::Result(msg.get("result").cloned().unwrap_or(Value::Null))
                };
                let _ = tx.send(outcome);
            }
        }
        // Server -> client requests (id + method) are not something this client
        // answers; drop them.
        return;
    }

    if let Some(method) = msg.get("method").and_then(Value::as_str) {
        let params = msg.get("params").cloned().unwrap_or(Value::Null);
        on_notification(Notification {
            method: method.to_string(),
            params,
        });
    }
}

fn describe_error(error: Option<&Value>) -> String {
    match error {
        Some(e) => {
            let message = e
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            match e.get("code").and_then(Value::as_i64) {
                Some(code) => format!("rpc error {code}: {message}"),
                None => message.to_string(),
            }
        }
        None => "unknown error".to_string(),
    }
}
