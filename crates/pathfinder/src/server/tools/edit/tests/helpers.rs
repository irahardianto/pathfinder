#![allow(clippy::unwrap_used, clippy::expect_used, clippy::needless_return)]
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::WorkspaceRoot;
use pathfinder_lsp::types::{DefinitionLocation, LspDiagnostic};
use pathfinder_lsp::{Lawyer, LspError};
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use pathfinder_treesitter::surgeon::BodyRange;
use std::path::Path;
use std::sync::Arc;

#[allow(dead_code)]
pub struct UnsupportedDiagLawyer;

#[async_trait::async_trait]
impl Lawyer for UnsupportedDiagLawyer {
    async fn goto_definition(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _line: u32,
        _column: u32,
    ) -> Result<Option<DefinitionLocation>, LspError> {
        Ok(None)
    }
    async fn call_hierarchy_prepare(
        &self,
        _workspace_root: &std::path::Path,
        _file_path: &std::path::Path,
        _line: u32,
        _column: u32,
    ) -> Result<Vec<pathfinder_lsp::types::CallHierarchyItem>, LspError> {
        Err(LspError::NoLspAvailable)
    }
    async fn call_hierarchy_incoming(
        &self,
        _workspace_root: &std::path::Path,
        _item: &pathfinder_lsp::types::CallHierarchyItem,
    ) -> Result<Vec<pathfinder_lsp::types::CallHierarchyCall>, LspError> {
        Err(LspError::NoLspAvailable)
    }
    async fn call_hierarchy_outgoing(
        &self,
        _workspace_root: &std::path::Path,
        _item: &pathfinder_lsp::types::CallHierarchyItem,
    ) -> Result<Vec<pathfinder_lsp::types::CallHierarchyCall>, LspError> {
        Err(LspError::NoLspAvailable)
    }
    async fn did_open(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _content: &str,
    ) -> Result<(), LspError> {
        Ok(())
    }
    async fn did_change(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _content: &str,
        _version: i32,
    ) -> Result<(), LspError> {
        Ok(())
    }
    async fn did_close(&self, _workspace_root: &Path, _file_path: &Path) -> Result<(), LspError> {
        Ok(())
    }
    async fn pull_diagnostics(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        Err(LspError::UnsupportedCapability {
            capability: "diagnosticProvider".into(),
        })
    }
    async fn pull_workspace_diagnostics(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
    ) -> Result<Vec<LspDiagnostic>, LspError> {
        Err(LspError::UnsupportedCapability {
            capability: "diagnosticProvider".into(),
        })
    }
    async fn range_formatting(
        &self,
        _workspace_root: &Path,
        _file_path: &Path,
        _start_line: u32,
        _end_line: u32,
        _original_content: &str,
    ) -> Result<Option<String>, LspError> {
        Ok(None)
    }
    async fn capability_status(
        &self,
    ) -> std::collections::HashMap<String, pathfinder_lsp::types::LspLanguageStatus> {
        std::collections::HashMap::new()
    }
    async fn did_change_watched_files(
        &self,
        _changes: Vec<pathfinder_lsp::types::FileEvent>,
    ) -> Result<(), LspError> {
        Ok(())
    }
}

pub fn make_server_dyn(
    ws_dir: &tempfile::TempDir,
    surgeon: Arc<dyn pathfinder_treesitter::surgeon::Surgeon>,
) -> crate::server::PathfinderServer {
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    crate::server::PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        surgeon,
    )
}

pub fn make_server(
    ws_dir: &tempfile::TempDir,
    mock_surgeon: MockSurgeon,
) -> crate::server::PathfinderServer {
    make_server_dyn(ws_dir, Arc::new(mock_surgeon))
}

pub fn make_body_range(
    open: usize,
    close: usize,
    indent: usize,
    body_indent: usize,
) -> BodyRange {
    BodyRange {
        start_byte: open,
        end_byte: close,
        indent_column: indent,
        body_indent_column: body_indent,
    }
}
