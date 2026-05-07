//! The `Lawyer` trait — testability boundary for LSP operations.
//!
//! All consumers of LSP functionality depend on this trait, **not** on any
//! concrete LSP client. This enables unit testing without a real language
//! server by injecting [`MockLawyer`](crate::MockLawyer).

use crate::{
    error::LspError,
    types::{CallHierarchyCall, CallHierarchyItem, DefinitionLocation},
};
use async_trait::async_trait;
use std::path::Path;

/// A live document registration with the LSP.
///
/// # IW-3 (DS-1 gap fix)
///
/// This trait represents the RAII contract for an open document. When the
/// value is dropped, the LSP receives `textDocument/didClose` automatically,
/// preventing memory leaks regardless of early returns or panics in callers.
///
/// Navigation tools should obtain this via [`Lawyer::open_document`] rather
/// than manually managing document lifecycle.
pub trait DocumentLease: Send + Sync {}

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

    /// Open a document and return a RAII guard that auto-closes it on drop.
    ///
    /// # IW-3 (DS-1 gap fix)
    ///
    /// This is the **preferred** way to open documents for transient LSP queries
    /// (navigation, impact analysis, deep context). The returned `DocumentLease`
    /// automatically calls `did_close` when dropped, ensuring no document leaks
    /// regardless of early returns or panics.
    ///
    /// Callers **must** hold the returned lease for the duration of their LSP
    /// query. Dropping it early will trigger `did_close` prematurely.
    ///
    /// # Errors
    /// - `LspError::NoLspAvailable` — no language server for this file type
    /// - `LspError::ConnectionLost` — LSP process crashed
    ///
    /// # Notes
    /// - When no LSP is available, implementations should return
    ///   `Err(LspError::NoLspAvailable)`. The caller should handle this
    ///   gracefully by skipping the LSP query.
    async fn open_document(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        content: &str,
    ) -> Result<Box<dyn DocumentLease>, LspError>;

    /// Retrieve the current LSP process status and capabilities per language.
    async fn capability_status(
        &self,
    ) -> std::collections::HashMap<String, crate::types::LspLanguageStatus>;

    /// Retrieve languages whose markers were found but whose LSP binaries are not on PATH.
    ///
    /// Used to surface actionable install guidance in `lsp_health` responses.
    fn missing_languages(&self) -> Vec<crate::client::MissingLanguage>;

    /// IW-4: Force-respawn the LSP process for `language_id`.
    ///
    /// Drops the existing process entry and triggers a fresh spawn.
    /// Returns `Ok(())` on successful start, `Err(LspError::NoLspAvailable)` if
    /// no descriptor is registered for the language.
    async fn force_respawn(&self, language_id: &str) -> Result<(), LspError>;

    /// LT-4: Pre-warm LSP processes for specific languages.
    ///
    /// Called after `get_repo_map` to start LSPs for languages found in the
    /// project skeleton before the agent explicitly requests LSP operations.
    ///
    /// Default implementation is a no-op (used by `NoOpLawyer` and `MockLawyer`).
    fn warm_start_for_languages(&self, _language_ids: &[String]) {}

    /// LT-4: Extend idle timer for a language without making an LSP request.
    ///
    /// Called by `read_source_file` to prevent the LSP from timing out while
    /// the agent is actively reading files.
    ///
    /// Default implementation is a no-op.
    fn touch_language(&self, _language_id: &str) {}
}
