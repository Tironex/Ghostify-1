//! Error types shared by every Ghostify crate.

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Result alias used across the workspace.
pub type Result<T> = std::result::Result<T, Error>;

/// The single error type of the Ghostify core crates.
#[derive(Debug, Error)]
pub enum Error {
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    #[error("serialization error: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("configuration error: {0}")]
    Config(String),
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("conflict: {0}")]
    Conflict(String),
    #[error("permission denied: {0}")]
    PermissionDenied(String),
    #[error("database error: {0}")]
    Database(String),
    #[error("internal error: {0}")]
    Internal(String),
}

impl Error {
    /// Stable machine readable code. The UI and the plugin bridge switch on
    /// this value, never on the human readable message.
    pub fn code(&self) -> &'static str {
        match self {
            Error::Io(_) => "io",
            Error::Serde(_) => "serialization",
            Error::Config(_) => "config",
            Error::NotFound(_) => "not_found",
            Error::Invalid(_) => "invalid_input",
            Error::Conflict(_) => "conflict",
            Error::PermissionDenied(_) => "permission_denied",
            Error::Database(_) => "database",
            Error::Internal(_) => "internal",
        }
    }
}

/// Error shape sent across the Tauri IPC boundary and the bridge protocol.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: String,
    pub message: String,
}

impl From<&Error> for ApiError {
    fn from(err: &Error) -> Self {
        ApiError {
            code: err.code().to_owned(),
            message: err.to_string(),
        }
    }
}

impl From<Error> for ApiError {
    fn from(err: Error) -> Self {
        ApiError::from(&err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable() {
        assert_eq!(Error::NotFound("x".into()).code(), "not_found");
        assert_eq!(Error::Invalid("x".into()).code(), "invalid_input");
        assert_eq!(Error::Database("x".into()).code(), "database");
    }

    #[test]
    fn api_error_carries_code_and_message() {
        let api = ApiError::from(Error::Conflict("duplicate".into()));
        assert_eq!(api.code, "conflict");
        assert_eq!(api.message, "conflict: duplicate");
        let json = serde_json::to_string(&api).unwrap();
        let back: ApiError = serde_json::from_str(&json).unwrap();
        assert_eq!(api, back);
    }
}
