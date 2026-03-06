//! Test double for the `Scout` trait.
//!
//! `MockScout` allows unit tests of `PathfinderServer` and other consumers to
//! control exactly what a search returns — without touching the file system.

use crate::searcher::{Scout, SearchError};
use crate::types::{SearchParams, SearchResult};
use std::sync::{Arc, Mutex};

/// A configurable fake `Scout` for unit testing.
///
/// # Usage
/// ```ignore
/// let mut mock = MockScout::default();
/// mock.set_result(Ok(SearchResult { matches: vec![], total_matches: 0, truncated: false }));
/// let server = PathfinderServer::with_scout(Arc::new(mock), ...);
/// ```
#[derive(Clone, Default)]
pub struct MockScout {
    /// What to return from the next `search()` call.
    ///
    /// If `None`, returns an empty result.
    next_result: Arc<Mutex<Option<Result<SearchResult, String>>>>,
    /// All `SearchParams` that were passed to `search()`, in call order.
    calls: Arc<Mutex<Vec<SearchParams>>>,
}

impl MockScout {
    /// Set the result to return from the next `search()` call.
    pub fn set_result(&self, result: Result<SearchResult, String>) {
        let mut guard = self
            .next_result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(result);
    }

    /// Returns a snapshot of all `SearchParams` passed to `search()`.
    #[must_use]
    pub fn calls(&self) -> Vec<SearchParams> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Returns how many times `search()` was called.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.calls().len()
    }
}

#[async_trait::async_trait]
impl Scout for MockScout {
    async fn search(&self, params: &SearchParams) -> Result<SearchResult, SearchError> {
        {
            let mut guard = self
                .calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.push(params.clone());
        }

        let next = {
            let mut guard = self
                .next_result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.take()
        };

        match next {
            Some(Ok(result)) => Ok(result),
            Some(Err(msg)) => Err(SearchError::Engine(msg)),
            None => Ok(SearchResult {
                matches: vec![],
                total_matches: 0,
                truncated: false,
            }),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn params() -> SearchParams {
        SearchParams {
            workspace_root: PathBuf::from("/tmp"),
            query: "test".to_owned(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn test_mock_defaults_to_empty_result() {
        let mock = MockScout::default();
        let result = mock.search(&params()).await.expect("should succeed");
        assert!(result.matches.is_empty());
        assert_eq!(result.total_matches, 0);
    }

    #[tokio::test]
    async fn test_mock_returns_configured_result() {
        let mock = MockScout::default();
        mock.set_result(Ok(SearchResult {
            matches: vec![],
            total_matches: 42,
            truncated: true,
        }));
        let result = mock.search(&params()).await.expect("should succeed");
        assert_eq!(result.total_matches, 42);
        assert!(result.truncated);
    }

    #[tokio::test]
    async fn test_mock_records_calls() {
        let mock = MockScout::default();
        let _ = mock.search(&params()).await;
        let _ = mock.search(&params()).await;
        assert_eq!(mock.call_count(), 2);
    }

    #[tokio::test]
    async fn test_mock_returns_error_when_configured() {
        let mock = MockScout::default();
        mock.set_result(Err("something broke".to_owned()));
        let result = mock.search(&params()).await;
        assert!(result.is_err());
    }
}
