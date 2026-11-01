use serde::{Deserialize, Serialize};
use std::fmt;

/// Mirrors `IpcErrorCode` (snake_case on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
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

/// Shape of every failed command (`IpcError`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub message: String,
}

impl ApiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self { code, message: message.into() }
    }
    pub fn unimplemented(what: &str) -> Self {
        Self::new(ErrorCode::Unimplemented, format!("{what} is not implemented by this backend"))
    }
    pub fn not_found(what: &str) -> Self {
        Self::new(ErrorCode::NotFound, format!("{what} not found"))
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.message)
    }
}
impl std::error::Error for ApiError {}

pub type ApiResult<T> = Result<T, ApiError>;
