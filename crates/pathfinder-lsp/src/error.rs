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
    /// it is configurable (default: 10s). Includes the operation name and
    /// timeout duration for structured logging.
    #[error("LSP timed out on '{operation}' after {timeout_ms}ms")]
    Timeout {
        /// The JSON-RPC method that timed out.
        operation: String,
        /// The timeout that elapsed, in milliseconds.
        timeout_ms: u64,
    },

    /// The LSP server returned a JSON-RPC error response or sent malformed data.
    ///
    /// The contained string is a human-readable description of the error,
    /// suitable for logging and agent-facing messages.
    #[error("LSP protocol error: {0}")]
    Protocol(String),

    /// The LSP server process crashed or the connection was broken.
    ///
    /// Triggers crash-recovery logic (exponential backoff, max 3 retries).
    #[error("LSP connection lost")]
    ConnectionLost,

    /// I/O error communicating with the LSP process.
    #[error("LSP I/O error: {0}")]
    Io(#[source] std::io::Error),
}
