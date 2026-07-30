//! Typed error system — pattern from adancurusul/serial-mcp-server.
//!
//! Provides SerialError with recovery hints (is_recoverable()) and
//! log/metric categories (category()), replacing ad-hoc json!({"error":...}).

use thiserror::Error;

/// Main error type for serial MCP operations.
#[derive(Error, Debug)]
pub enum SerialError {
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),

    #[error("Operation timed out")]
    OperationTimeout,

    #[error("Command cancelled")]
    CommandCancelled,

    #[error("Invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Serialization error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("Internal error: {0}")]
    Internal(String),
}

impl SerialError {
    /// Whether the operation can be retried (transient/recoverable errors).
    pub fn is_recoverable(&self) -> bool {
        matches!(
            self,
            SerialError::OperationTimeout
                | SerialError::CommandCancelled
                | SerialError::ConnectionFailed(_)
        )
    }

    /// Error category for logging and metrics.
    pub fn category(&self) -> &'static str {
        match self {
            SerialError::ConnectionFailed(_) => "connection",
            SerialError::OperationTimeout | SerialError::CommandCancelled => "communication",
            SerialError::InvalidConfig(_) => "configuration",
            SerialError::Io(_) => "system",
            SerialError::Json(_) => "serialization",
            SerialError::Internal(_) => "internal",
        }
    }
}

/// Convenience result type.
pub type Result<T> = std::result::Result<T, SerialError>;
