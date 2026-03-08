//! The `Lawyer` trait — testability boundary for LSP operations.
//!
//! All consumers of LSP functionality depend on this trait, **not** on any
//! concrete LSP client. This enables unit testing without a real language
//! server by injecting [`MockLawyer`](crate::MockLawyer).

use crate::{
    error::LspError,
    types::{DefinitionLocation, LspDiagnostic},
};
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

    /// Notify the LSP that a file has been opened with the given content.
    ///
    /// This is a notification (fire-and-forget) — the LSP begins tracking the
    /// document so that subsequent `pull_diagnostics` calls work correctly.
    ///
    /// Must be called before the first `did_change` on a file.
    ///
    /// # Errors
    /// - `LspError::NoLspAvailable` — no language server for this file type
    /// - `LspError::ConnectionLost` — LSP process crashed
    async fn did_open(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: &str,
    ) -> Result<(), LspError>;

    /// Notify the LSP of a full content change to an open document.
    ///
    /// Sends `textDocument/didChange` with full-file synchronisation
    /// (version-based change tracking). This updates the LSP's in-memory
    /// document state so that `pull_diagnostics` sees the new content.
    ///
    /// # Errors
    /// - `LspError::NoLspAvailable` — no language server for this file type
    /// - `LspError::ConnectionLost` — LSP process crashed
    async fn did_change(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: &str,
        version: i32,
    ) -> Result<(), LspError>;

    /// Request Pull Diagnostics for a file (LSP 3.17 `textDocument/diagnostic`).
    ///
    /// Intended for use in the edit validation pipeline: called before and
    /// after an in-memory edit to compute the diagnostic diff.
    ///
    /// # Errors
    /// - `LspError::NoLspAvailable` — no language server for this file type
    /// - `LspError::UnsupportedCapability` — LSP does not support Pull Diagnostics
    /// - `LspError::Timeout` — LSP did not respond within the timeout
    /// - `LspError::Protocol` — LSP returned malformed diagnostics
    async fn pull_diagnostics(
        &self,
        workspace_root: &Path,
        file_path: &Path,
    ) -> Result<Vec<LspDiagnostic>, LspError>;

    /// Request range formatting for a changed region (LSP `textDocument/rangeFormatting`).
    ///
    /// Returns `Ok(Some(formatted_text))` when the LSP supports formatting.
    /// Returns `Ok(None)` when the LSP is available but returns no edits.
    ///
    /// # Errors
    /// - `LspError::NoLspAvailable` — no language server for this file type
    /// - `LspError::UnsupportedCapability` — LSP does not support formatting
    /// - `LspError::Timeout` — LSP did not respond within the timeout
    async fn range_formatting(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        start_line: u32,
        end_line: u32,
    ) -> Result<Option<String>, LspError>;
}
