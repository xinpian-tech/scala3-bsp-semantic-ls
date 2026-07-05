//! Typed failures of the BSP client layer. Protocol-level problems surface as
//! one of these; domain-level results such as a failed compile stay in typed
//! result values (a later `BspCompileOutcome`).

use std::fmt;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BspError {
    NoConnectionFile { workspace_root: String },
    InvalidConnectionFile { path: String, detail: String },
    LaunchFailed { server: String, detail: String },
    RequestTimeout { method: String, timeout_millis: u64 },
    RequestFailed { method: String, detail: String },
    InvalidResponse { method: String, detail: String },
    SessionClosed { method: String },
}

impl BspError {
    pub fn message(&self) -> String {
        match self {
            BspError::NoConnectionFile { workspace_root } => {
                format!("no usable .bsp/*.json connection file under {workspace_root}")
            }
            BspError::InvalidConnectionFile { path, detail } => {
                format!("invalid BSP connection file {path}: {detail}")
            }
            BspError::LaunchFailed { server, detail } => {
                format!("failed to launch BSP server '{server}': {detail}")
            }
            BspError::RequestTimeout {
                method,
                timeout_millis,
            } => format!("BSP request {method} timed out after {timeout_millis}ms"),
            BspError::RequestFailed { method, detail } => {
                format!("BSP request {method} failed: {detail}")
            }
            BspError::InvalidResponse { method, detail } => {
                format!("BSP response for {method} is invalid: {detail}")
            }
            BspError::SessionClosed { method } => {
                format!("BSP session is closed; cannot send {method}")
            }
        }
    }
}

impl fmt::Display for BspError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for BspError {}
