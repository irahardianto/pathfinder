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

    /// The LSP server is running but does not advertise the requested capability.
    ///
    /// For example, a server that doesn't implement Pull Diagnostics (LSP 3.17)
    /// will trigger this error for `pull_diagnostics()` calls. Tool handlers
    /// should return `validation_skipped: true, reason: "pull_diagnostics_unsupported"`.
    #[error("LSP does not support capability: {capability}")]
    UnsupportedCapability {
        /// The LSP capability name (e.g., `"diagnosticProvider"`).
        capability: String,
    },

    /// I/O error communicating with the LSP process.
    #[error("LSP I/O error: {0}")]
    Io(#[source] std::io::Error),
}

impl LspError {
    /// Returns an actionable recovery hint for the agent.
    ///
    /// The hint tells agents *what to do next* — not just what went wrong.
    /// All variants return `Some`; agents should surface these in tool
    /// responses when validation or navigation degrades.
    ///
    /// This is the LSP-layer equivalent of `PathfinderError::hint()`.
    #[must_use]
    pub fn recovery_hint(&self) -> Option<String> {
        match self {
            Self::NoLspAvailable => Some(
                "No LSP available for this file type. \
                 Call lsp_health to see install instructions and check which languages are configured."
                    .to_owned(),
            ),
            Self::Timeout { operation, timeout_ms } => Some(format!(
                "LSP request '{operation}' timed out after {timeout_ms}ms. \
                 The language server may still be indexing or under memory pressure. \
                 (1) Call lsp_health to check server status and indexing progress. \
                 (2) If status is 'warming_up', retry in 30s. \
                 (3) Use search_codebase + read_symbol_scope as tree-sitter fallbacks."
            )),
            Self::ConnectionLost => Some(
                "LSP connection lost — the language server may have crashed. \
                 Call lsp_health(action='restart') to recover the server. \
                 Use read_source_file and search_codebase as fallbacks while it restarts."
                    .to_owned(),
            ),
            Self::Protocol(msg) => Some(format!(
                "LSP protocol error: {msg}. \
                 Call lsp_health to check server health. \
                 Use search_codebase for text-based navigation in the meantime."
            )),
            Self::UnsupportedCapability { capability } => Some(format!(
                "The LSP server does not support '{capability}'. \
                 This is a server limitation — validation is skipped for this file type. \
                 Use search_codebase + read_symbol_scope for navigation."
            )),
            Self::Io(io_err) if io_err.kind() == std::io::ErrorKind::NotFound => Some(
                "LSP binary not found. \
                 Ensure the language server is installed and available in PATH. \
                 Call lsp_health for install instructions."
                    .to_owned(),
            ),
            Self::Io(io_err) => Some(format!(
                "LSP I/O error: {io_err}. \
                 The language server process failed to start. \
                 Call lsp_health to check server status."
            )),
        }
    }
}
