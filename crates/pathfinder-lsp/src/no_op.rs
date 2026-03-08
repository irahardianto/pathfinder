//! `NoOpLawyer` — graceful degradation when no LSP is configured.
//!
//! This is the default `Lawyer` implementation used when Pathfinder starts
//! without LSP support. All methods return [`LspError::NoLspAvailable`].
//!
//! Tool handlers catch this error and return a degraded response with
//! `"degraded": true` instead of propagating it.

use crate::{error::LspError, lawyer::Lawyer, types::DefinitionLocation};
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[tokio::test]
    async fn test_no_op_lawyer_goto_definition_returns_no_lsp() {
        let lawyer = NoOpLawyer;
        let result = lawyer
            .goto_definition(
                &PathBuf::from("/workspace"),
                &PathBuf::from("src/main.rs"),
                1,
                1,
            )
            .await;
        assert!(matches!(result, Err(LspError::NoLspAvailable)));
    }
}
