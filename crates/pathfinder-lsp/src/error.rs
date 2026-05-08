//! Error types for LSP operations.

use thiserror::Error;

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn test_lsp_error_no_lsp_available_recovery_hint() {
        let err = LspError::NoLspAvailable;
        let hint = err.recovery_hint();

        assert!(hint.is_some());
        let hint_str = hint.expect("recovery hint should be Some");
        assert!(hint_str.contains("No LSP available"));
        assert!(hint_str.contains("lsp_health"));
    }

    #[test]
    fn test_lsp_error_timeout_recovery_hint() {
        let err = LspError::Timeout {
            operation: "textDocument/definition".to_string(),
            timeout_ms: 10000,
        };
        let hint = err.recovery_hint();

        assert!(hint.is_some());
        let hint_str = hint.expect("recovery hint should be Some");
        assert!(hint_str.contains("textDocument/definition"));
        assert!(hint_str.contains("10000ms"));
        assert!(hint_str.contains("lsp_health"));
        assert!(hint_str.contains("retry in 30s"));
    }

    #[test]
    fn test_lsp_error_protocol_recovery_hint() {
        let err = LspError::Protocol("malformed JSON-RPC".to_string());
        let hint = err.recovery_hint();

        assert!(hint.is_some());
        let hint_str = hint.expect("recovery hint should be Some");
        assert!(hint_str.contains("malformed JSON-RPC"));
        assert!(hint_str.contains("lsp_health"));
        assert!(hint_str.contains("search_codebase"));
    }

    #[test]
    fn test_lsp_error_connection_lost_recovery_hint() {
        let err = LspError::ConnectionLost;
        let hint = err.recovery_hint();

        assert!(hint.is_some());
        let hint_str = hint.expect("recovery hint should be Some");
        assert!(hint_str.contains("connection lost"));
        assert!(hint_str.contains("lsp_health"));
        assert!(hint_str.contains("restart"));
        assert!(hint_str.contains("read_source_file"));
    }

    #[test]
    fn test_lsp_error_unsupported_capability_recovery_hint() {
        let err = LspError::UnsupportedCapability {
            capability: "diagnosticProvider".to_string(),
        };
        let hint = err.recovery_hint();

        assert!(hint.is_some());
        let hint_str = hint.expect("recovery hint should be Some");
        assert!(hint_str.contains("diagnosticProvider"));
        assert!(hint_str.contains("does not support"));
        assert!(hint_str.contains("search_codebase"));
    }

    #[test]
    fn test_lsp_error_io_not_found_recovery_hint() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "binary not found");
        let err = LspError::Io(io_err);
        let hint = err.recovery_hint();

        assert!(hint.is_some());
        let hint_str = hint.expect("recovery hint should be Some");
        assert!(hint_str.contains("binary not found"));
        assert!(hint_str.contains("installed"));
        assert!(hint_str.contains("PATH"));
        assert!(hint_str.contains("lsp_health"));
    }

    #[test]
    fn test_lsp_error_io_permission_denied_recovery_hint() {
        let io_err = io::Error::new(io::ErrorKind::PermissionDenied, "access denied");
        let err = LspError::Io(io_err);
        let hint = err.recovery_hint();

        assert!(hint.is_some());
        let hint_str = hint.expect("recovery hint should be Some");
        assert!(hint_str.contains("I/O error"));
        assert!(hint_str.contains("access denied"));
        assert!(hint_str.contains("lsp_health"));
    }

    #[test]
    fn test_lsp_error_io_broken_pipe_recovery_hint() {
        let io_err = io::Error::new(io::ErrorKind::BrokenPipe, "pipe broken");
        let err = LspError::Io(io_err);
        let hint = err.recovery_hint();

        assert!(hint.is_some());
        let hint_str = hint.expect("recovery hint should be Some");
        assert!(hint_str.contains("I/O error"));
        assert!(hint_str.contains("pipe broken"));
        assert!(hint_str.contains("lsp_health"));
    }

    #[test]
    fn test_lsp_error_display_no_lsp_available() {
        let err = LspError::NoLspAvailable;
        let display = format!("{err}");
        assert_eq!(display, "no LSP available for this file type");
    }

    #[test]
    fn test_lsp_error_display_timeout() {
        let err = LspError::Timeout {
            operation: "initialize".to_string(),
            timeout_ms: 30000,
        };
        let display = format!("{err}");
        assert!(display.contains("initialize"));
        assert!(display.contains("30000ms"));
    }

    #[test]
    fn test_lsp_error_display_protocol() {
        let err = LspError::Protocol("invalid response".to_string());
        let display = format!("{err}");
        assert_eq!(display, "LSP protocol error: invalid response");
    }

    #[test]
    fn test_lsp_error_display_connection_lost() {
        let err = LspError::ConnectionLost;
        let display = format!("{err}");
        assert_eq!(display, "LSP connection lost");
    }

    #[test]
    fn test_lsp_error_display_unsupported_capability() {
        let err = LspError::UnsupportedCapability {
            capability: "callHierarchy".to_string(),
        };
        let display = format!("{err}");
        assert!(display.contains("callHierarchy"));
    }

    #[test]
    fn test_lsp_error_display_io() {
        let io_err = io::Error::new(io::ErrorKind::NotFound, "test error");
        let err = LspError::Io(io_err);
        let display = format!("{err}");
        assert!(display.contains("I/O error"));
        assert!(display.contains("test error"));
    }

    #[test]
    fn test_all_error_variants_have_recovery_hints() {
        // NoLspAvailable
        assert!(LspError::NoLspAvailable.recovery_hint().is_some());

        // Timeout
        assert!(LspError::Timeout {
            operation: "test".to_string(),
            timeout_ms: 5000,
        }
        .recovery_hint()
        .is_some());

        // Protocol
        assert!(LspError::Protocol("test".to_string())
            .recovery_hint()
            .is_some());

        // ConnectionLost
        assert!(LspError::ConnectionLost.recovery_hint().is_some());

        // UnsupportedCapability
        assert!(LspError::UnsupportedCapability {
            capability: "test".to_string(),
        }
        .recovery_hint()
        .is_some());

        // Io - NotFound
        assert!(
            LspError::Io(io::Error::new(io::ErrorKind::NotFound, "test"))
                .recovery_hint()
                .is_some()
        );

        // Io - PermissionDenied
        assert!(
            LspError::Io(io::Error::new(io::ErrorKind::PermissionDenied, "test"))
                .recovery_hint()
                .is_some()
        );

        // Io - Other
        assert!(LspError::Io(std::io::Error::other("test"))
            .recovery_hint()
            .is_some());
    }
}

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
    /// will trigger this error for diagnostic queries. The tool handler should
    /// degrade gracefully and report the limitation to the caller.
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
