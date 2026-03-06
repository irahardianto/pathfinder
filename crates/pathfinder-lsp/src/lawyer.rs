//! The `Lawyer` trait — testability boundary for LSP operations.
//!
//! All consumers of LSP functionality depend on this trait, **not** on any
//! concrete LSP client. This enables unit testing without a real language
//! server by injecting [`MockLawyer`](crate::MockLawyer).

use crate::{error::LspError, types::DefinitionLocation};
use async_trait::async_trait;
use std::path::Path;

/// Abstracts Language Server Protocol operations behind a testable interface.
///
/// # Contract
/// - All positions are 1-indexed (line and column).
/// - All file paths are relative to the workspace root.
/// - `LspError::NoLspAvailable` is not a crash — it is the expected error
///   when running without an LSP. Tool handlers must gracefully degrade.
#[async_trait]
pub trait Lawyer: Send + Sync {
    /// Locate the definition of the symbol at the given file position.
    ///
    /// `line` and `column` are both 1-indexed.
    ///
    /// Returns `Ok(None)` if the symbol has no definition (e.g., a built-in).
    /// Returns `Err(LspError::NoLspAvailable)` when no LSP is configured.
    ///
    /// # Errors
    /// - `LspError::NoLspAvailable` — no language server for this file type
    /// - `LspError::Timeout` — LSP did not respond within the timeout
    /// - `LspError::Protocol` — LSP returned an error response
    /// - `LspError::ConnectionLost` — LSP process crashed
    async fn goto_definition(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Option<DefinitionLocation>, LspError>;
}
