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
    types::DefinitionLocation,
};
use async_trait::async_trait;
use std::{
    path::Path,
    sync::{Arc, Mutex},
};

/// Shared state type for configuring mock results.
///
/// `None` means "use the default" (return `Ok(None)`).
/// `Some(Ok(Some(loc)))` returns a definition location.
/// `Some(Ok(None))` returns "no definition found".
/// `Some(Err(msg))` simulates an LSP error.
type GotoDefinitionFixture = Arc<Mutex<Option<Result<Option<DefinitionLocation>, String>>>>;

/// A configurable fake `Lawyer` for unit testing.
#[derive(Clone, Default)]
pub struct MockLawyer {
    /// Configured result for `goto_definition`. `None` = return `Ok(None)`.
    goto_definition_result: GotoDefinitionFixture,
    /// All calls made to `goto_definition` in order.
    goto_definition_calls: Arc<Mutex<Vec<(String, u32, u32)>>>,
}

impl MockLawyer {
    /// Set the result to return from the next `goto_definition()` call.
    ///
    /// Pass `Ok(Some(...))` for a found definition, `Ok(None)` for no definition,
    /// or `Err("reason")` to simulate an LSP error.
    pub fn set_goto_definition_result(
        &self,
        result: Result<Option<DefinitionLocation>, String>,
    ) {
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
            guard.push((
                file_path.to_string_lossy().into_owned(),
                line,
                column,
            ));
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
