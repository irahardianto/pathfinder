//! Error types for LSP operations.

use thiserror::Error;

/// Errors that the LSP engine can produce.
#[derive(Debug, Error)]
pub enum LspError {
    /// No language server is configured or available for this file type.
    ///
    /// This is the expected error when Pathfinder is running without LSP
    /// support (i.e., in degraded mode). The calling tool handler should
    /// return a gracefully degraded response rather than propagating this error.
    #[error("no LSP available for this file type")]
    NoLspAvailable,

    /// The LSP server did not respond within the timeout window.
    ///
    /// For initialization this is 30 seconds. For individual requests,
    /// it is configurable (default: 10s).
    #[error("LSP timed out: {reason}")]
    Timeout { reason: String },

    /// The LSP server returned a JSON-RPC error response.
    #[error("LSP error (code {code}): {message}")]
    Protocol { code: i64, message: String },

    /// The LSP server process crashed or the connection was broken.
    #[error("LSP connection lost: {reason}")]
    ConnectionLost { reason: String },

    /// I/O error communicating with the LSP process.
    #[error("LSP I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// JSON serialization/deserialization error.
    #[error("LSP message parse error: {0}")]
    Json(#[from] serde_json::Error),
}
