//! The `Lawyer` trait — testability boundary for LSP operations.
//!
//! All consumers of LSP functionality depend on this trait, **not** on any
//! concrete LSP client. This enables unit testing without a real language
//! server by injecting [`MockLawyer`](crate::MockLawyer).

use crate::{
    error::LspError,
    types::{CallHierarchyCall, CallHierarchyItem, DefinitionLocation, FileEvent, LspDiagnostic},
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

    /// Prepare a call hierarchy operation at the given position.
    ///
    /// The returned `CallHierarchyItem` list represents the symbols at the
    /// cursor position that can be queried for incoming/outgoing calls.
    async fn call_hierarchy_prepare(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<CallHierarchyItem>, LspError>;

    /// Resolve incoming calls to a previously prepared `CallHierarchyItem`.
    async fn call_hierarchy_incoming(
        &self,
        workspace_root: &Path,
        item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError>;

    /// Resolve outgoing calls from a previously prepared `CallHierarchyItem`.
    async fn call_hierarchy_outgoing(
        &self,
        workspace_root: &Path,
        item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError>;

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

    /// Notify the LSP that a document has been closed.
    ///
    /// Sends `textDocument/didClose`. This allows the LSP to clear its internal
    /// state for the file (preventing memory leaks for short-lived validations).
    ///
    /// # Errors
    /// - `LspError::NoLspAvailable` — no language server for this file type
    /// - `LspError::ConnectionLost` — LSP process crashed
    async fn did_close(&self, workspace_root: &Path, file_path: &Path) -> Result<(), LspError>;

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

    /// Collect diagnostics using the push model (`textDocument/publishDiagnostics`).
    ///
    /// Sends `didOpen` or `didChange` as indicated by the version, then waits
    /// for `textDocument/publishDiagnostics` notifications targeting the file.
    /// Returns all diagnostics received within the timeout window.
    ///
    /// Used as a fallback for LSP servers that don't support Pull Diagnostics
    /// (LSP 3.17), such as gopls and typescript-language-server.
    ///
    /// # Errors
    /// - `LspError::NoLspAvailable` — no language server for this file type
    /// - `LspError::Timeout` — LSP did not respond within the timeout (rare for push model, returns empty instead)
    /// - `LspError::Protocol` — LSP returned malformed diagnostics
    async fn collect_diagnostics(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: &str,
        version: i32,
        timeout_ms: u64,
    ) -> Result<Vec<LspDiagnostic>, LspError>;

    /// Request Pull Diagnostics for the entire workspace (LSP 3.17 `workspace/diagnostic`).
    ///
    /// Intended for use in the edit validation pipeline: called after an in-memory
    /// edit to catch cross-file breakages.
    ///
    /// # Errors
    /// - `LspError::NoLspAvailable` — no language server for this file type
    /// - `LspError::UnsupportedCapability` — LSP does not support Workspace Diagnostics
    /// - `LspError::Timeout` — LSP did not respond within the timeout
    /// - `LspError::Protocol` — LSP returned malformed diagnostics
    async fn pull_workspace_diagnostics(
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
        original_content: &str,
    ) -> Result<Option<String>, LspError>;

    /// Retrieve the current LSP process status and capabilities per language.
    async fn capability_status(
        &self,
    ) -> std::collections::HashMap<String, crate::types::LspLanguageStatus>;

    /// Retrieve languages whose markers were found but whose LSP binaries are not on PATH.
    ///
    /// Used to surface actionable install guidance in `lsp_health` responses.
    fn missing_languages(&self) -> Vec<crate::client::MissingLanguage>;

    /// Notify all running LSP processes of a filesystem change.
    ///
    /// Broadcasts `workspace/didChangeWatchedFiles` to every running LSP process.
    /// This is a notification (fire-and-forget). Broadcasting to all processes is
    /// intentional: the LSP spec allows servers to ignore events for file patterns
    /// they don't watch, and routing by extension is unreliable when files are
    /// being created or deleted.
    async fn did_change_watched_files(&self, changes: Vec<FileEvent>) -> Result<(), LspError>;
}
