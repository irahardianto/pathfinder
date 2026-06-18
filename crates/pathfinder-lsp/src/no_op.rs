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
    types::{CallHierarchyCall, CallHierarchyItem, DefinitionLocation, ReferenceLocation},
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

    async fn references(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _line: u32,
        _column: u32,
    ) -> Result<Vec<ReferenceLocation>, LspError> {
        Err(LspError::NoLspAvailable)
    }

    async fn goto_implementation(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _line: u32,
        _column: u32,
    ) -> Result<Vec<DefinitionLocation>, LspError> {
        Err(LspError::NoLspAvailable)
    }

    async fn open_document(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _content: &str,
    ) -> Result<Box<dyn crate::lawyer::DocumentLease>, LspError> {
        Err(LspError::NoLspAvailable)
    }

    async fn capability_status(
        &self,
    ) -> std::collections::HashMap<String, crate::types::LspLanguageStatus> {
        std::collections::HashMap::new()
    }

    fn missing_languages(&self) -> Vec<crate::client::MissingLanguage> {
        vec![]
    }

    async fn force_respawn(&self, _language_id: &str) -> Result<(), LspError> {
        Err(LspError::NoLspAvailable)
    }

    fn is_warm_start_complete(&self) -> bool {
        false
    }
}

#[cfg(test)]
#[path = "no_op_test.rs"]
mod tests;
