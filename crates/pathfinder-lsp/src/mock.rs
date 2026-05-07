//! Test double for the `Lawyer` trait.
//!
//! `MockLawyer` allows unit tests of `PathfinderServer` and other consumers
//! to control exactly what the LSP returns — without needing a real language
//! server.
//!
//! # Usage
//! ```rust,ignore
//! let mock = MockLawyer::default();
//! mock.set_goto_definition_result(Ok(Some(DefinitionLocation {
//!     file: "src/main.rs".into(),
//!     line: 10,
//!     column: 5,
//!     preview: "fn main() {".into(),
//! })));
//! let server = PathfinderServer::with_engines(.., Arc::new(mock));
//! ```

use crate::{
    error::LspError,
    lawyer::{DocumentLease, Lawyer},
    types::{CallHierarchyCall, CallHierarchyItem, DefinitionLocation},
};
use async_trait::async_trait;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

// ── NullDocumentLease ────────────────────────────────────────────────────────

/// A no-op `DocumentLease` used by `MockLawyer`.
///
/// `MockLawyer.open_document` tracks the call via `did_open_calls` (matching
/// production behaviour) and returns this sentinel. On drop, nothing happens
/// — the mock already records `did_close` via a separate field when callers
/// drop the lease. Because `MockLawyer.open_document` builds the guard
/// by calling through `self.did_open()`, the `did_open_calls` counter is
/// incremented, and a corresponding `did_close_calls` increment happens only
/// when the caller explicitly drops the lease *and* the guard's Drop fires.
///
/// In practice, `MockDocumentLease` stores back-references to the mock's
/// counters so that dropping it records `did_close` without needing a real
/// LSP connection.
pub struct MockDocumentLease {
    did_close_calls: Arc<Mutex<Vec<String>>>,
    file_path: String,
}

impl DocumentLease for MockDocumentLease {}

impl Drop for MockDocumentLease {
    fn drop(&mut self) {
        self.did_close_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(self.file_path.clone());
    }
}

// ── Fixture types ─────────────────────────────────────────────────────────────

/// Configured result for `goto_definition`.
type GotoDefinitionFixture = Arc<Mutex<Option<Result<Option<DefinitionLocation>, String>>>>;

/// Configured error for `did_open`. `None` = return `Ok(())`.
type DidOpenErrorFixture = Arc<Mutex<Option<LspError>>>;

/// Queue of results for `call_hierarchy_prepare`.
type PrepareCallHierarchyQueue = Arc<Mutex<Vec<Result<Vec<CallHierarchyItem>, String>>>>;

/// Queue of results for `call_hierarchy_incoming` and `call_hierarchy_outgoing`.
type CallHierarchyQueue = Arc<Mutex<Vec<Result<Vec<CallHierarchyCall>, String>>>>;

/// Configured result for `capability_status`.
type CapabilityStatusFixture =
    Arc<Mutex<std::collections::HashMap<String, crate::types::LspLanguageStatus>>>;

/// Configured result for `missing_languages`.
type MissingLanguagesFixture = Arc<Mutex<Vec<crate::client::MissingLanguage>>>;

/// A configurable fake `Lawyer` for unit testing.
#[derive(Clone, Default)]
pub struct MockLawyer {
    // ── goto_definition ───────────────────────────────────────────────────────
    /// Configured result for `goto_definition`. `None` = return `Ok(None)`.
    goto_definition_result: GotoDefinitionFixture,
    /// All calls made to `goto_definition` in order.
    goto_definition_calls: Arc<Mutex<Vec<(String, u32, u32)>>>,

    // ── did_open ──────────────────────────────────────────────────────────────
    /// Number of `did_open` notifications received.
    pub did_open_calls: Arc<Mutex<Vec<(String, String)>>>,
    /// Configurable error to return from `did_open`. `None` = return `Ok(())`.
    pub did_open_error: DidOpenErrorFixture,

    // ── did_close ─────────────────────────────────────────────────────────────
    /// Number of `did_close` notifications received.
    pub did_close_calls: Arc<Mutex<Vec<String>>>,

    // ── capability_status ─────────────────────────────────────────────────────
    /// Configured result for `capability_status`.
    capability_status_result: CapabilityStatusFixture,

    // ── missing_languages ─────────────────────────────────────────────────────
    /// Configured result for `missing_languages`.
    missing_languages_result: MissingLanguagesFixture,

    // ── call_hierarchy ────────────────────────────────────────────────────────
    /// Queue of results for successive `call_hierarchy_prepare` calls.
    pub prepare_call_hierarchy_results: PrepareCallHierarchyQueue,
    /// Queue of results for successive `call_hierarchy_incoming` calls.
    pub incoming_call_results: CallHierarchyQueue,
    /// Queue of results for successive `call_hierarchy_outgoing` calls.
    pub outgoing_call_results: CallHierarchyQueue,
}

impl MockLawyer {
    /// Pop the next result from a queued result mutex.
    ///
    /// Shared helper for `call_hierarchy_prepare`, `call_hierarchy_incoming`,
    /// `call_hierarchy_outgoing`.
    /// Pop the next queued result, returning `None` when the queue is empty.
    ///
    /// Returns:
    /// - `Some(Ok(item))` when the queue has a success result
    /// - `Some(Err(..))` when the queue has a configured error
    /// - `None` when the queue is empty (caller decides default behavior)
    fn pop_queued_result<T>(
        mutex: &Arc<Mutex<Vec<Result<T, String>>>>,
    ) -> Option<Result<T, LspError>> {
        let mut guard = mutex
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if guard.is_empty() {
            return None;
        }
        match guard.remove(0) {
            Ok(item) => Some(Ok(item)),
            Err(msg) => Some(Err(LspError::Protocol(msg))),
        }
    }

    // ── goto_definition ───────────────────────────────────────────────────────

    /// Set the result to return from the next `goto_definition()` call.
    pub fn set_goto_definition_result(&self, result: Result<Option<DefinitionLocation>, String>) {
        let mut guard = self
            .goto_definition_result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(result);
    }

    /// Returns how many times `goto_definition()` was called.
    #[must_use]
    pub fn goto_definition_call_count(&self) -> usize {
        self.goto_definition_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Returns a snapshot of all `(file, line, column)` passed to `goto_definition()`.
    #[must_use]
    pub fn goto_definition_calls(&self) -> Vec<(String, u32, u32)> {
        self.goto_definition_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Force `did_open` to return the given `LspError` (once).
    ///
    /// Use this to test the `no_lsp` / `unsupported` early-exit branches of
    /// `run_lsp_validation` without needing a separate `Lawyer` implementation.
    /// After the error is consumed by one `did_open` call it reverts to `Ok(())`.
    pub fn set_did_open_error(&self, error: LspError) {
        let mut guard = self
            .did_open_error
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(error);
    }

    // ── call_hierarchy ────────────────────────────────────────────────────────

    /// Push a result onto the `call_hierarchy_prepare` queue.
    pub fn push_prepare_call_hierarchy_result(
        &self,
        result: Result<Vec<CallHierarchyItem>, String>,
    ) {
        self.prepare_call_hierarchy_results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(result);
    }

    /// Push a result onto the `call_hierarchy_incoming` queue.
    pub fn push_incoming_call_result(&self, result: Result<Vec<CallHierarchyCall>, String>) {
        self.incoming_call_results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(result);
    }

    /// Push a result onto the `call_hierarchy_outgoing` queue.
    pub fn push_outgoing_call_result(&self, result: Result<Vec<CallHierarchyCall>, String>) {
        self.outgoing_call_results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(result);
    }

    /// Returns the number of `did_open` calls recorded.
    #[must_use]
    pub fn did_open_call_count(&self) -> usize {
        self.did_open_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Returns the number of `did_close` calls recorded.
    #[must_use]
    pub fn did_close_call_count(&self) -> usize {
        self.did_close_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .len()
    }

    /// Set the result to return from `capability_status()`.
    pub fn set_capability_status(
        &self,
        status: std::collections::HashMap<String, crate::types::LspLanguageStatus>,
    ) {
        *self
            .capability_status_result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = status;
    }

    /// Set the result to return from `missing_languages()`.
    pub fn set_missing_languages(&self, missing: Vec<crate::client::MissingLanguage>) {
        *self
            .missing_languages_result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = missing;
    }
}

#[async_trait]
impl Lawyer for MockLawyer {
    async fn goto_definition(
        &self,
        _workspace_root: &Path,
        file_path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Option<DefinitionLocation>, LspError> {
        {
            let mut guard = self
                .goto_definition_calls
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.push((file_path.to_string_lossy().into_owned(), line, column));
        }

        let next = {
            let mut guard = self
                .goto_definition_result
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.take()
        };

        match next {
            Some(Ok(result)) => Ok(result),
            Some(Err(msg)) => Err(LspError::Protocol(msg)),
            None => Ok(None),
        }
    }

    async fn call_hierarchy_prepare(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _line: u32,
        _column: u32,
    ) -> Result<Vec<CallHierarchyItem>, LspError> {
        Self::pop_queued_result(&self.prepare_call_hierarchy_results).unwrap_or_else(|| Ok(vec![]))
    }

    async fn call_hierarchy_incoming(
        &self,
        _workspace_root: &Path,
        _item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError> {
        Self::pop_queued_result(&self.incoming_call_results).unwrap_or_else(|| Ok(vec![]))
    }

    async fn call_hierarchy_outgoing(
        &self,
        _workspace_root: &Path,
        _item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError> {
        Self::pop_queued_result(&self.outgoing_call_results).unwrap_or_else(|| Ok(vec![]))
    }

    /// IW-3 (DS-1 gap fix): RAII document lifecycle.
    ///
    /// Records the open via `did_open_calls` and returns a `MockDocumentLease`
    /// that records `did_close_calls` on drop — mirroring production semantics
    /// without needing a real LSP. Tests can assert that open and close counts
    /// match exactly, verifying no lifecycle leaks.
    async fn open_document(
        &self,
        _workspace_root: &Path,
        file_path: &Path,
        content: &str,
    ) -> Result<Box<dyn crate::lawyer::DocumentLease>, LspError> {
        // If a failure has been configured on did_open, propagate it here too.
        self.did_open_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((file_path.to_string_lossy().into_owned(), content.to_owned()));

        let maybe_error = {
            let mut guard = self
                .did_open_error
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            guard.take()
        };
        if let Some(e) = maybe_error {
            return Err(e);
        }

        Ok(Box::new(MockDocumentLease {
            did_close_calls: self.did_close_calls.clone(),
            file_path: file_path.to_string_lossy().into_owned(),
        }))
    }

    async fn capability_status(
        &self,
    ) -> std::collections::HashMap<String, crate::types::LspLanguageStatus> {
        self.capability_status_result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn missing_languages(&self) -> Vec<crate::client::MissingLanguage> {
        self.missing_languages_result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    async fn force_respawn(&self, _language_id: &str) -> Result<(), LspError> {
        // No-op in mock
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn workspace() -> PathBuf {
        PathBuf::from("/workspace")
    }

    fn file() -> PathBuf {
        PathBuf::from("src/main.rs")
    }

    #[tokio::test]
    async fn test_mock_defaults_to_none() {
        let mock = MockLawyer::default();
        let result = mock.goto_definition(&workspace(), &file(), 1, 1).await;
        assert!(matches!(result, Ok(None)));
    }

    #[tokio::test]
    async fn test_mock_returns_configured_definition() {
        let mock = MockLawyer::default();
        let expected = DefinitionLocation {
            file: "src/auth.rs".into(),
            line: 42,
            column: 5,
            preview: "pub fn login() {".into(),
        };
        mock.set_goto_definition_result(Ok(Some(expected.clone())));

        let result = mock
            .goto_definition(&workspace(), &file(), 10, 15)
            .await
            .expect("should succeed");
        assert_eq!(result, Some(expected));
    }

    #[tokio::test]
    async fn test_mock_records_calls() {
        let mock = MockLawyer::default();
        let _ = mock.goto_definition(&workspace(), &file(), 5, 10).await;
        let _ = mock.goto_definition(&workspace(), &file(), 20, 3).await;

        assert_eq!(mock.goto_definition_call_count(), 2);
        let calls = mock.goto_definition_calls();
        assert_eq!(calls[0], ("src/main.rs".into(), 5, 10));
        assert_eq!(calls[1], ("src/main.rs".into(), 20, 3));
    }

    #[tokio::test]
    async fn test_mock_returns_error_when_configured() {
        let mock = MockLawyer::default();
        mock.set_goto_definition_result(Err("LSP crashed".into()));
        let result = mock.goto_definition(&workspace(), &file(), 1, 1).await;
        assert!(result.is_err());
    }
}
