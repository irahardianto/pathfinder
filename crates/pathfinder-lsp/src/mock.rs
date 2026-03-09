//! Test double for the `Lawyer` trait.
//!
//! `MockLawyer` allows unit tests of `PathfinderServer` and other consumers
//! to control exactly what the LSP returns — without needing a real language
//! server.
//!
//! # Usage
//! ```ignore
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
    lawyer::Lawyer,
    types::{CallHierarchyCall, CallHierarchyItem, DefinitionLocation, LspDiagnostic},
};
use async_trait::async_trait;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

// ── Fixture types ─────────────────────────────────────────────────────────────

/// Configured result for `goto_definition`.
type GotoDefinitionFixture = Arc<Mutex<Option<Result<Option<DefinitionLocation>, String>>>>;

/// Queue of results for `pull_diagnostics` calls.
type PullDiagnosticsQueue = Arc<Mutex<Vec<Result<Vec<LspDiagnostic>, String>>>>;

/// Configured result for `range_formatting`.
type RangeFormattingFixture = Arc<Mutex<Option<Result<Option<String>, String>>>>;

/// Configured error for `did_open`. `None` = return `Ok(())`.
type DidOpenErrorFixture = Arc<Mutex<Option<LspError>>>;

/// Queue of results for `call_hierarchy_prepare`.
type PrepareCallHierarchyQueue = Arc<Mutex<Vec<Result<Vec<CallHierarchyItem>, String>>>>;

/// Queue of results for `call_hierarchy_incoming` and `call_hierarchy_outgoing`.
type CallHierarchyQueue = Arc<Mutex<Vec<Result<Vec<CallHierarchyCall>, String>>>>;

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

    // ── did_change ────────────────────────────────────────────────────────────
    /// All `(file_path, content, version)` tuples passed to `did_change`.
    pub did_change_calls: Arc<Mutex<Vec<(String, String, i32)>>>,

    // ── did_close ─────────────────────────────────────────────────────────────
    /// Number of `did_close` notifications received.
    pub did_close_calls: Arc<Mutex<Vec<String>>>,

    // ── pull_diagnostics ──────────────────────────────────────────────────────
    /// Queue of results for successive `pull_diagnostics` calls.
    ///
    /// Each call pops the front. When empty, returns `Ok(vec![])`.
    pub pull_diagnostics_results: PullDiagnosticsQueue,

    // ── pull_workspace_diagnostics ────────────────────────────────────────────
    /// Queue of results for successive `pull_workspace_diagnostics` calls.
    pub pull_workspace_diagnostics_results: PullDiagnosticsQueue,

    // ── call_hierarchy ────────────────────────────────────────────────────────
    /// Queue of results for successive `call_hierarchy_prepare` calls.
    pub prepare_call_hierarchy_results: PrepareCallHierarchyQueue,
    /// Queue of results for successive `call_hierarchy_incoming` calls.
    pub incoming_call_results: CallHierarchyQueue,
    /// Queue of results for successive `call_hierarchy_outgoing` calls.
    pub outgoing_call_results: CallHierarchyQueue,

    // ── range_formatting ──────────────────────────────────────────────────────
    /// Configured result for `range_formatting`.
    ///
    /// `None` = return `Ok(None)` (LSP available, no formatting edits).
    pub range_formatting_result: RangeFormattingFixture,
}

impl MockLawyer {
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

    // ── pull_diagnostics ──────────────────────────────────────────────────────

    /// Push a result onto the `pull_diagnostics` queue.
    ///
    /// Calls are served FIFO. When the queue is empty, `Ok(vec![])` is returned.
    pub fn push_pull_diagnostics_result(&self, result: Result<Vec<LspDiagnostic>, String>) {
        self.pull_diagnostics_results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(result);
    }

    /// Push a result onto the `pull_workspace_diagnostics` queue.
    pub fn push_pull_workspace_diagnostics_result(
        &self,
        result: Result<Vec<LspDiagnostic>, String>,
    ) {
        self.pull_workspace_diagnostics_results
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(result);
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

    /// Returns the number of `did_change` calls recorded.
    #[must_use]
    pub fn did_change_call_count(&self) -> usize {
        self.did_change_calls
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

    /// Set the result for `range_formatting`.
    ///
    /// Pass `Ok(Some(text))` for formatted output, `Ok(None)` for no edits, or `Err` for error.
    pub fn set_range_formatting_result(&self, result: Result<Option<String>, String>) {
        let mut guard = self
            .range_formatting_result
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        *guard = Some(result);
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
        let next = {
            let mut guard = self
                .prepare_call_hierarchy_results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.is_empty() {
                None
            } else {
                Some(guard.remove(0))
            }
        };

        match next {
            Some(Ok(items)) => Ok(items),
            Some(Err(msg)) => Err(LspError::Protocol(msg)),
            None => Ok(vec![]),
        }
    }

    async fn call_hierarchy_incoming(
        &self,
        _workspace_root: &Path,
        _item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError> {
        let next = {
            let mut guard = self
                .incoming_call_results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.is_empty() {
                None
            } else {
                Some(guard.remove(0))
            }
        };

        match next {
            Some(Ok(calls)) => Ok(calls),
            Some(Err(msg)) => Err(LspError::Protocol(msg)),
            None => Ok(vec![]),
        }
    }

    async fn call_hierarchy_outgoing(
        &self,
        _workspace_root: &Path,
        _item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError> {
        let next = {
            let mut guard = self
                .outgoing_call_results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.is_empty() {
                None
            } else {
                Some(guard.remove(0))
            }
        };

        match next {
            Some(Ok(calls)) => Ok(calls),
            Some(Err(msg)) => Err(LspError::Protocol(msg)),
            None => Ok(vec![]),
        }
    }

    async fn did_open(
        &self,
        _workspace_root: &Path,
        file_path: &Path,
        content: &str,
    ) -> Result<(), LspError> {
        self.did_open_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((file_path.to_string_lossy().into_owned(), content.to_owned()));

        // If a failure has been configured, consume and return it.
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
        Ok(())
    }

    async fn did_change(
        &self,
        _workspace_root: &Path,
        file_path: &Path,
        content: &str,
        version: i32,
    ) -> Result<(), LspError> {
        self.did_change_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push((
                file_path.to_string_lossy().into_owned(),
                content.to_owned(),
                version,
            ));
        Ok(())
    }

    async fn did_close(&self, _workspace_root: &Path, file_path: &Path) -> Result<(), LspError> {
        self.did_close_calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(file_path.to_string_lossy().into_owned());
        Ok(())
    }

    async fn pull_diagnostics(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        let next = {
            let mut guard = self
                .pull_diagnostics_results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.is_empty() {
                None
            } else {
                Some(guard.remove(0))
            }
        };

        match next {
            Some(Ok(diags)) => Ok(diags),
            Some(Err(msg)) => Err(LspError::Protocol(msg)),
            None => Ok(vec![]),
        }
    }

    async fn pull_workspace_diagnostics(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        let next = {
            let mut guard = self
                .pull_workspace_diagnostics_results
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if guard.is_empty() {
                None
            } else {
                Some(guard.remove(0))
            }
        };

        match next {
            Some(Ok(diags)) => Ok(diags),
            Some(Err(msg)) => Err(LspError::Protocol(msg)),
            None => Ok(vec![]),
        }
    }

    async fn range_formatting(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _start_line: u32,
        _end_line: u32,
    ) -> Result<Option<String>, LspError> {
        let next = {
            let mut guard = self
                .range_formatting_result
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

    #[tokio::test]
    async fn test_mock_did_open_records_calls() {
        let mock = MockLawyer::default();
        mock.did_open(&workspace(), &file(), "fn main() {}")
            .await
            .expect("should succeed");
        assert_eq!(mock.did_open_call_count(), 1);
        let calls = mock.did_open_calls.lock().expect("lock");
        assert_eq!(calls[0].0, "src/main.rs");
        assert_eq!(calls[0].1, "fn main() {}");
    }

    #[tokio::test]
    async fn test_mock_did_change_records_calls() {
        let mock = MockLawyer::default();
        mock.did_change(&workspace(), &file(), "fn main() { let x = 1; }", 2)
            .await
            .expect("should succeed");
        assert_eq!(mock.did_change_call_count(), 1);
        let calls = mock.did_change_calls.lock().expect("lock");
        assert_eq!(calls[0].2, 2); // version
    }

    #[tokio::test]
    async fn test_mock_did_close_records_calls() {
        let mock = MockLawyer::default();
        mock.did_close(&workspace(), &file())
            .await
            .expect("should succeed");
        assert_eq!(mock.did_close_call_count(), 1);
        let calls = mock.did_close_calls.lock().expect("lock");
        assert_eq!(calls[0], "src/main.rs");
    }

    #[tokio::test]
    async fn test_mock_pull_diagnostics_empty_queue_returns_empty() {
        let mock = MockLawyer::default();
        let result = mock
            .pull_diagnostics(&workspace(), &file())
            .await
            .expect("should succeed");
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_mock_pull_diagnostics_returns_configured() {
        use crate::types::LspDiagnosticSeverity;
        let mock = MockLawyer::default();
        let diag = LspDiagnostic {
            severity: LspDiagnosticSeverity::Error,
            code: Some("E001".into()),
            message: "type mismatch".into(),
            file: "src/main.rs".into(),
            start_line: 5,
            end_line: 5,
        };
        mock.push_pull_diagnostics_result(Ok(vec![diag.clone()]));
        let result = mock
            .pull_diagnostics(&workspace(), &file())
            .await
            .expect("should succeed");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].message, "type mismatch");
    }

    #[tokio::test]
    async fn test_mock_pull_workspace_diagnostics_empty_queue_returns_empty() {
        let mock = MockLawyer::default();
        let result = mock
            .pull_workspace_diagnostics(&workspace(), &file())
            .await
            .expect("should succeed");
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_mock_pull_workspace_diagnostics_returns_configured() {
        use crate::types::LspDiagnosticSeverity;
        let mock = MockLawyer::default();
        let diag = LspDiagnostic {
            severity: LspDiagnosticSeverity::Error,
            code: Some("W002".into()),
            message: "workspace error".into(),
            file: "src/other.rs".into(),
            start_line: 1,
            end_line: 1,
        };
        mock.push_pull_workspace_diagnostics_result(Ok(vec![diag.clone()]));
        let result = mock
            .pull_workspace_diagnostics(&workspace(), &file())
            .await
            .expect("should succeed");
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].message, "workspace error");
    }

    #[tokio::test]
    async fn test_mock_range_formatting_defaults_to_none() {
        let mock = MockLawyer::default();
        let result = mock
            .range_formatting(&workspace(), &file(), 1, 10)
            .await
            .expect("should succeed");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_mock_range_formatting_returns_configured() {
        let mock = MockLawyer::default();
        mock.set_range_formatting_result(Ok(Some("formatted_code".into())));
        let result = mock
            .range_formatting(&workspace(), &file(), 1, 5)
            .await
            .expect("should succeed");
        assert_eq!(result, Some("formatted_code".into()));
    }
}
