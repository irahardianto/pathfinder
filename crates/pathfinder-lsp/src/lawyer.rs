//! The `Lawyer` trait — testability boundary for LSP operations.
//!
//! All consumers of LSP functionality depend on this trait, **not** on any
//! concrete LSP client. This enables unit testing without a real language
//! server by injecting [`MockLawyer`](crate::MockLawyer).

use crate::{
    error::LspError,
    types::{CallHierarchyCall, CallHierarchyItem, DefinitionLocation, ReferenceLocation},
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

    /// Find all implementations of the symbol at the given position.
    ///
    /// For Java: finds classes that `extend` or `implement` this class/interface.
    /// For Rust: finds types that implement a trait.
    /// For TypeScript: finds classes that `extend` or `implement`.
    ///
    /// Uses LSP `textDocument/implementation` request.
    ///
    /// `line` and `column` are both 1-indexed.
    ///
    /// Returns `Ok(Vec)` containing all implementation locations.
    /// Returns `Err(LspError::NoLspAvailable)` when no LSP is configured.
    ///
    /// Default implementation returns `Ok(vec![])` for backwards compatibility.
    ///
    /// # Errors
    /// - `LspError::NoLspAvailable` — no language server for this file type
    /// - `LspError::Timeout` — LSP did not respond within timeout
    /// - `LspError::Protocol` — LSP returned an error response
    /// - `LspError::ConnectionLost` — LSP process crashed
    async fn goto_implementation(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _line: u32,
        _column: u32,
    ) -> Result<Vec<DefinitionLocation>, LspError> {
        Ok(vec![])
    }

    /// Find all references to the symbol at the given position.
    ///
    /// Returns all locations where the symbol is referenced (used, called, or accessed).
    /// Uses LSP `textDocument/references` request.
    ///
    /// `line` and `column` are both 1-indexed.
    ///
    /// Returns `Ok(Vec)` containing all reference locations.
    /// Returns `Err(LspError::NoLspAvailable)` when no LSP is configured.
    ///
    /// # Errors
    /// - `LspError::NoLspAvailable` — no language server for this file type
    /// - `LspError::Timeout` — LSP did not respond within timeout
    /// - `LspError::Protocol` — LSP returned an error response
    /// - `LspError::ConnectionLost` — LSP process crashed
    async fn references(
        &self,
        workspace_root: &Path,
        file_path: &Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<ReferenceLocation>, LspError>;

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

    /// The position encoding negotiated with the language server for
    /// `language_id` (TS-A2 / LSP-A5).
    ///
    /// Callers building a `textDocument/*` request position from a
    /// Tree-sitter-derived **byte** column MUST convert it through
    /// [`pathfinder_treesitter::surgeon::to_lsp_character`] using this
    /// encoding first — sending a raw byte offset to a server that negotiated
    /// `utf-16` silently mispositions the query on any line with a
    /// multi-byte character before the target identifier.
    ///
    /// Defaults to `Utf16` (the LSP 3.17 spec default when unnegotiated, or
    /// when the language isn't running) — never assume `Utf8`.
    fn position_encoding(
        &self,
        language_id: &str,
    ) -> pathfinder_treesitter::surgeon::PositionEncoding {
        let _ = language_id;
        pathfinder_treesitter::surgeon::PositionEncoding::Utf16
    }

    /// Retrieve the current LSP process status and capabilities per language.
    ///
    /// # Status (v6)
    /// Not currently called from any of the three surviving MCP tools
    /// (`locate`/`trace`/`inspect`) — the `health`/`explore` tools that used
    /// to surface this were removed. Retained as public `Lawyer` API for the
    /// WS-3.7 residual allowance (a one-line `setup_notice` on a session's
    /// first tool response when a detected language has no LSP binary on
    /// PATH) should that be built; not wired up today.
    async fn capability_status(
        &self,
    ) -> std::collections::HashMap<String, crate::types::LspLanguageStatus>;

    /// Retrieve languages whose markers were found but whose LSP binaries are not on PATH.
    ///
    /// # Status (v6)
    /// Not currently called from any of the three surviving MCP tools — see
    /// [`Lawyer::capability_status`]'s note. The underlying `missing_languages`
    /// *field* this wraps is still live and consulted internally by
    /// `ensure_ready`'s install-hint lookup; only this convenience accessor
    /// method is unused.
    fn missing_languages(&self) -> Vec<crate::client::MissingLanguage>;

    /// IW-4: Force-respawn the LSP process for `language_id`.
    ///
    /// Drops the existing process entry and triggers a fresh spawn.
    /// Returns `Ok(())` on successful start, `Err(LspError::NoLspAvailable)` if
    /// no descriptor is registered for the language.
    async fn force_respawn(&self, language_id: &str) -> Result<(), LspError>;

    /// PATCH-004: Check whether `warm_start` has completed.
    ///
    /// Returns `true` if all `warm_start` tasks have finished, `false` otherwise.
    /// This is used by `health` to report accurate status.
    /// Default implementation returns `false` (no `warm_start` has been triggered).
    fn is_warm_start_complete(&self) -> bool {
        false
    }

    /// LT-4: Pre-warm LSP processes for specific languages.
    ///
    /// Called after `explore` to start LSPs for languages found in the
    /// project skeleton before the agent explicitly requests LSP operations.
    ///
    /// PATCH-004: Returns `Vec<JoinHandle<()>>` for tracking `warm_start` completion.
    /// Default implementation is a no-op (used by `NoOpLawyer` and `MockLawyer`).
    fn warm_start_for_languages(
        &self,
        _language_ids: &[String],
    ) -> Vec<tokio::task::JoinHandle<()>> {
        Vec::new()
    }

    /// PATCH-004: Kick off `warm_start` for specific languages and track completion.
    ///
    /// Like `warm_start_for_languages` but also sets the `warm_start_complete` flag
    /// when all tasks finish. Default implementation delegates to
    /// `warm_start_for_languages` and ignores handles (no completion tracking).
    fn warm_start_for_languages_and_track(&self, language_ids: &[String]) {
        let _ = self.warm_start_for_languages(language_ids);
    }
    /// Ensure that the LSP server for `language_id` is ready to serve requests.
    ///
    /// Executes or awaits the behavioural symbol probe within `budget`.
    ///
    /// # Returns
    /// - [`Readiness::Ready`](crate::types::Readiness::Ready) if the server is up and verified with probe.
    /// - [`Readiness::NotReady`](crate::types::Readiness::NotReady) if the server is still initializing/probing and budget expired.
    /// - [`Readiness::Unavailable`](crate::types::Readiness::Unavailable) if no LSP binary exists for this language.
    /// - [`Readiness::Degraded`](crate::types::Readiness::Degraded) if the probe failed or server crashed repeatedly.
    async fn ensure_ready(
        &self,
        language_id: &str,
        budget: std::time::Duration,
    ) -> Result<crate::types::Readiness, LspError> {
        let _ = (language_id, budget);
        Ok(crate::types::Readiness::Ready(
            crate::types::CapabilityReadiness::all(),
        ))
    }
}
