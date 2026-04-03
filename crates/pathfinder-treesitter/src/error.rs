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
            SurgeonError::SymbolNotFound { path, did_you_mean } => {
                pathfinder_common::error::PathfinderError::SymbolNotFound {
                    semantic_path: path,
                    did_you_mean,
                }
            }
            SurgeonError::UnsupportedLanguage(path) => {
                pathfinder_common::error::PathfinderError::UnsupportedLanguage { path }
            }
            SurgeonError::ParseError { path, reason } => {
                pathfinder_common::error::PathfinderError::ParseError { path, reason }
            }
            SurgeonError::Io(err) => pathfinder_common::error::PathfinderError::IoError {
                message: err.to_string(),
            },
            SurgeonError::InvalidTarget { path, reason } => {
                pathfinder_common::error::PathfinderError::InvalidTarget {
                    semantic_path: path,
                    reason,
                    edit_index: None,
                    valid_edit_types: None,
                }
            }
        }
    }
}
