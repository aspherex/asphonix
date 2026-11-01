//! Shim for the WebView app's `ipc.rs` error types (vendored modules `use crate::ipc::{IpcError, IpcErrorCode}`).
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IpcErrorCode {
    NotConnected,
    Network,
    AuthFailed,
    NotFound,
    ServerError,
    Io,
    Decode,
    OutputUnavailable,
    Unimplemented,
    InvalidRequest,
    Unknown,
}

#[derive(Debug, Clone, Serialize, thiserror::Error)]
#[error("{message}")]
#[serde(rename_all = "camelCase")]
pub struct IpcError {
    pub code: IpcErrorCode,
    pub message: String,
}

impl IpcError {
    pub fn unimplemented(what: &str) -> Self {
        Self { code: IpcErrorCode::Unimplemented, message: format!("{what} is not implemented yet") }
    }
}

pub type IpcResult<T> = Result<T, IpcError>;
