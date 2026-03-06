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
    #[error("parse error: {0}")]
    ParseError(String),

    /// A file-system error occurred when attempting to read source files.
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),
}
