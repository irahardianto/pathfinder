//! `NoOpLawyer` — graceful degradation when no LSP is configured.
//!
//! This is the default `Lawyer` implementation used when Pathfinder starts
//! without LSP support. All methods return [`LspError::NoLspAvailable`].
//!
//! Tool handlers catch this error and return a degraded response with
//! `"degraded": true` instead of propagating it.

use crate::{
    error::LspError,
    lawyer::Lawyer,
    types::{CallHierarchyCall, CallHierarchyItem, DefinitionLocation, FileEvent, LspDiagnostic},
};
use async_trait::async_trait;
use std::path::Path;

/// A no-op `Lawyer` that always reports LSP as unavailable.
///
/// Use this as the production default until a real LSP client is configured.
/// Tool handlers calling this will receive `LspError::NoLspAvailable` and
/// should fall back to degraded mode.
#[derive(Debug, Clone, Default)]
pub struct NoOpLawyer;

#[async_trait]
impl Lawyer for NoOpLawyer {
    async fn goto_definition(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _line: u32,
        _column: u32,
    ) -> Result<Option<DefinitionLocation>, LspError> {
        Err(LspError::NoLspAvailable)
    }

    async fn call_hierarchy_prepare(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _line: u32,
        _column: u32,
    ) -> Result<Vec<CallHierarchyItem>, LspError> {
        Err(LspError::NoLspAvailable)
    }

    async fn call_hierarchy_incoming(
        &self,
        _workspace_root: &Path,
        _item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError> {
        Err(LspError::NoLspAvailable)
    }

    async fn call_hierarchy_outgoing(
        &self,
        _workspace_root: &Path,
        _item: &CallHierarchyItem,
    ) -> Result<Vec<CallHierarchyCall>, LspError> {
        Err(LspError::NoLspAvailable)
    }

    async fn did_open(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _content: &str,
    ) -> Result<(), LspError> {
        Err(LspError::NoLspAvailable)
    }

    async fn did_change(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _content: &str,
        _version: i32,
    ) -> Result<(), LspError> {
        Err(LspError::NoLspAvailable)
    }

    async fn did_close(&self, _workspace_root: &Path, _file_path: &Path) -> Result<(), LspError> {
        Err(LspError::NoLspAvailable)
    }

    async fn pull_diagnostics(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        Err(LspError::NoLspAvailable)
    }

    async fn pull_workspace_diagnostics(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        Err(LspError::NoLspAvailable)
    }

    async fn range_formatting(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _start_line: u32,
        _end_line: u32,
        _original_content: &str,
    ) -> Result<Option<String>, LspError> {
        Err(LspError::NoLspAvailable)
    }

    async fn capability_status(
        &self,
    ) -> std::collections::HashMap<String, crate::types::LspLanguageStatus> {
        std::collections::HashMap::new()
    }

    async fn did_change_watched_files(&self, _changes: Vec<FileEvent>) -> Result<(), LspError> {
        Ok(())
    }
}

#[cfg(test)]
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
    async fn test_no_op_lawyer_goto_definition_returns_no_lsp() {
        let lawyer = NoOpLawyer;
        let result = lawyer.goto_definition(&workspace(), &file(), 1, 1).await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_no_op_lawyer_call_hierarchy_prepare_returns_no_lsp() {
        let lawyer = NoOpLawyer;
        let result = lawyer
            .call_hierarchy_prepare(&workspace(), &file(), 1, 1)
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_no_op_lawyer_call_hierarchy_incoming_returns_no_lsp() {
        let lawyer = NoOpLawyer;
        let item = CallHierarchyItem {
            name: "foo".into(),
            kind: "function".into(),
            detail: None,
            file: "src/main.rs".into(),
            line: 1,
            column: 1,
            data: None,
        };
        let result = lawyer.call_hierarchy_incoming(&workspace(), &item).await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_no_op_lawyer_call_hierarchy_outgoing_returns_no_lsp() {
        let lawyer = NoOpLawyer;
        let item = CallHierarchyItem {
            name: "foo".into(),
            kind: "function".into(),
            detail: None,
            file: "src/main.rs".into(),
            line: 1,
            column: 1,
            data: None,
        };
        let result = lawyer.call_hierarchy_outgoing(&workspace(), &item).await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_no_op_lawyer_did_open_returns_no_lsp() {
        let lawyer = NoOpLawyer;
        let result = lawyer.did_open(&workspace(), &file(), "fn main() {}").await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_no_op_lawyer_did_change_returns_no_lsp() {
        let lawyer = NoOpLawyer;
        let result = lawyer
            .did_change(&workspace(), &file(), "fn main() {}", 1)
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_no_op_lawyer_pull_diagnostics_returns_no_lsp() {
        let lawyer = NoOpLawyer;
        let result = lawyer.pull_diagnostics(&workspace(), &file()).await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_no_op_lawyer_pull_workspace_diagnostics_returns_no_lsp() {
        let lawyer = NoOpLawyer;
        let result = lawyer
            .pull_workspace_diagnostics(&workspace(), &file())
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_no_op_lawyer_range_formatting_returns_no_lsp() {
        let lawyer = NoOpLawyer;
        let result = lawyer
            .range_formatting(&workspace(), &file(), 1, 5, "")
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }

    #[tokio::test]
    async fn test_no_op_lawyer_did_change_watched_files_returns_ok() {
        let lawyer = NoOpLawyer;
        let result = lawyer.did_change_watched_files(vec![]).await;
        assert!(result.is_ok());
    }
}
