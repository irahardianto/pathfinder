//! The `Scout` trait — testability boundary for the search engine.
//!
//! All consumers of search functionality depend on this trait, **not** on
//! the concrete `RipgrepScout`. This enables unit testing without a real
//! file system by injecting [`MockScout`](crate::MockScout).

use crate::types::{SearchParams, SearchResult};
use thiserror::Error;

/// Errors that the search engine can produce.
#[derive(Debug, Error)]
pub enum SearchError {
    /// The search pattern is not valid regex (only when `is_regex = true`).
    #[error("invalid regex pattern: {0}")]
    InvalidPattern(String),

    /// A file-system error during directory traversal.
    #[error("filesystem error: {0}")]
    Io(#[from] std::io::Error),

    /// An unexpected internal error from the grep engine.
    #[error("search engine error: {0}")]
    Engine(String),
}

/// Abstracts file-system text search behind a testable interface.
///
/// # Contract
/// - Results are sorted by file path, then by line number (ascending).
/// - Matches are capped at `params.max_results`; `SearchResult::truncated` is
///   set when the real count exceeds the cap.
/// - `SearchMatch::enclosing_semantic_path` is always `None` in Epic 2.
#[async_trait::async_trait]
pub trait Scout: Send + Sync {
    /// Search the workspace for `params.query`.
    ///
    /// # Errors
    /// Returns [`SearchError`] if the pattern is invalid or I/O fails.
    async fn search(&self, params: &SearchParams) -> Result<SearchResult, SearchError>;
}
