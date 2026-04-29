use std::path::PathBuf;
use thiserror::Error;

/// Errors that the surgeon engine can produce.
#[derive(Debug, Error)]
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
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),

    /// The target symbol is incompatible with the requested edit operation.
    ///
    /// E.g., calling `replace_body` on a constant or abstract declaration.
    #[error("invalid target: {reason} (path: {path})")]
    InvalidTarget { path: String, reason: String },
}

impl From<SurgeonError> for pathfinder_common::error::PathfinderError {
    fn from(error: SurgeonError) -> Self {
        match error {
            SurgeonError::SymbolNotFound { path, did_you_mean } => Self::SymbolNotFound {
                semantic_path: path,
                did_you_mean,
            },
            SurgeonError::FileNotFound(path) => Self::FileNotFound { path },
            SurgeonError::UnsupportedLanguage(path) => Self::UnsupportedLanguage { path },
            SurgeonError::ParseError { path, reason } => Self::ParseError { path, reason },
            SurgeonError::Io(err) => Self::IoError {
                message: err.to_string(),
            },
            SurgeonError::InvalidTarget { path, reason } => Self::InvalidTarget {
                semantic_path: path,
                reason,
                edit_index: None,
                valid_edit_types: None,
            },
        }
    }
}
