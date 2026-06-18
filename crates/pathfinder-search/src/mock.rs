//! Test double for the `Scout` trait.
//!
//! `MockScout` allows unit tests of `PathfinderServer` and other consumers to
//! control exactly what a search returns — without touching the file system.

use crate::searcher::{Scout, SearchError};
use crate::types::{SearchParams, SearchResult};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A configurable fake `Scout` for unit testing.
///
/// # Usage
/// ```text
/// let mut mock = MockScout::default();
/// mock.set_result(Ok(SearchResult { matches: vec![], total_matches: 0, truncated: false }));
/// let server = PathfinderServer::with_scout(Arc::new(mock), ...);
/// ```
///
/// For sequential returns across multiple `search()` calls, use `set_results()`:
/// ```text
/// mock.set_results(vec![
///     Ok(result_1),  // returned on 1st call
///     Ok(result_2),  // returned on 2nd call
/// ]);
/// ```
#[derive(Clone, Default)]
pub struct MockScout {
    /// What to return from the next `search()` call.
    ///
    /// If `None`, returns an empty result.
    next_result: Arc<Mutex<Option<Result<SearchResult, String>>>>,
    /// Queue of results for sequential returns. Used by `set_results()`.
    /// Popped front on each `search()` call. Takes priority over `next_result`.
    result_queue: Arc<Mutex<VecDeque<Result<SearchResult, String>>>>,
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

    /// Queue multiple results to be returned sequentially across `search()` calls.
    /// Each call to `search()` pops the front of the queue. When the queue is
    /// empty, falls back to `next_result` (if set), then to empty results.
    ///
    /// This enables testing multi-strategy fallback chains (e.g., definition.rs
    /// strategies 1-4) where each strategy calls `search()` independently.
    pub fn set_results(&self, results: Vec<Result<SearchResult, String>>) {
        let mut guard = self
            .result_queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.clear();
        for r in results {
            guard.push_back(r);
        }
    }

    /// Returns a snapshot of all `SearchParams` passed to `search()`.
    #[must_use]
    pub fn calls(&self) -> Vec<SearchParams> {
        // CLONE: snapshot of call history for caller inspection
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
            // CLONE: record search params for later assertion
            guard.push(params.clone());
        }

        // Priority: result_queue (sequential) > next_result (single-shot) > empty
        let from_queue = {
            let mut guard = self
                .result_queue
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.pop_front()
        };

        let next = if let Some(result) = from_queue {
            Some(result)
        } else {
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
                files_searched: 0,
                files_in_scope: 0,
                binary_skipped: 0,
                gitignored_skipped: 0,
                other_skipped: 0,
            }),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;

    fn params() -> SearchParams {
        let temp = tempfile::tempdir().expect("create tempdir");
        SearchParams {
            workspace_root: temp.path().to_path_buf(),
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
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
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

    #[tokio::test]
    async fn test_mock_set_results_returns_sequentially() {
        let mock = MockScout::default();
        mock.set_results(vec![
            Ok(SearchResult {
                matches: vec![],
                total_matches: 1,
                truncated: false,
                files_searched: 0,
                files_in_scope: 0,
                binary_skipped: 0,
                gitignored_skipped: 0,
                other_skipped: 0,
            }),
            Ok(SearchResult {
                matches: vec![],
                total_matches: 2,
                truncated: false,
                files_searched: 0,
                files_in_scope: 0,
                binary_skipped: 0,
                gitignored_skipped: 0,
                other_skipped: 0,
            }),
            Ok(SearchResult {
                matches: vec![],
                total_matches: 3,
                truncated: false,
                files_searched: 0,
                files_in_scope: 0,
                binary_skipped: 0,
                gitignored_skipped: 0,
                other_skipped: 0,
            }),
        ]);

        let r1 = mock.search(&params()).await.expect("call 1");
        assert_eq!(r1.total_matches, 1);
        let r2 = mock.search(&params()).await.expect("call 2");
        assert_eq!(r2.total_matches, 2);
        let r3 = mock.search(&params()).await.expect("call 3");
        assert_eq!(r3.total_matches, 3);
        // Queue exhausted — falls back to empty
        let r4 = mock.search(&params()).await.expect("call 4");
        assert_eq!(r4.total_matches, 0);
    }
}
