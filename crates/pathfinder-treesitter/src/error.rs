use std::path::PathBuf;
use std::sync::Arc;
use thiserror::Error;

/// Errors that the surgeon engine can produce.
///
/// `Clone` is derived by wrapping the `Io` variant's `std::io::Error` in `Arc`.
/// `io::Error` is not `Clone`, so a bare `#[from] std::io::Error` would prevent
/// deriving `Clone`. Wrapping in `Arc` gives us reference-counted sharing while
/// preserving the `Display` impl through `io::Error`'s `Deref`.
#[derive(Debug, Error, Clone)]
pub enum SurgeonError {
    /// The requested symbol could not be found via the semantic path.
    #[error("symbol not found: {path}")]
    SymbolNotFound {
        path: String,
        /// Alternative symbols with similar names (Levenshtein distance).
        did_you_mean: Vec<String>,
    },

    /// The requested file does not exist on disk.
    ///
    /// Distinct from a generic I/O error so the MCP layer can surface this as a
    /// client error (`INVALID_PARAMS / FILE_NOT_FOUND`) rather than an internal
    /// server error (`INTERNAL_ERROR`).
    #[error("file not found: {0}")]
    FileNotFound(PathBuf),

    /// The requested file's language is not supported.
    #[error("unsupported language for path: {0}")]
    UnsupportedLanguage(PathBuf),

    /// A parsing error occurred.
    #[error("parse error in {path}: {reason}")]
    ParseError {
        path: std::path::PathBuf,
        reason: String,
    },

    /// A file-system error occurred when attempting to read source files.
    ///
    /// Wrapped in `Arc` to make this variant (and the whole enum) `Clone`.
    /// `std::io::Error` is not `Clone` itself.
    #[error("filesystem error: {0}")]
    Io(Arc<std::io::Error>),
}

impl From<std::io::Error> for SurgeonError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(Arc::new(e))
    }
}

impl From<SurgeonError> for pathfinder_common::error::PathfinderError {
    fn from(error: SurgeonError) -> Self {
        match error {
            SurgeonError::SymbolNotFound { path, did_you_mean } => Self::SymbolNotFound {
                semantic_path: path,
                did_you_mean,
                retry_after_seconds: None,
            },
            SurgeonError::FileNotFound(path) => Self::FileNotFound { path },
            SurgeonError::UnsupportedLanguage(path) => Self::UnsupportedLanguage { path },
            SurgeonError::ParseError { path, reason } => Self::ParseError { path, reason },
            SurgeonError::Io(err) => Self::IoError {
                message: err.to_string(),
            },
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "error_test.rs"]
mod tests;
