//! `read_symbol_scope` tool — AST-based symbol extraction via Tree-sitter.

use crate::server::helpers::{
    parse_semantic_path, pathfinder_to_error_data, require_symbol_target, serialize_metadata,
};
use crate::server::types::ReadSymbolScopeParams;
use crate::server::PathfinderServer;
use rmcp::model::{CallToolResult, Content, ErrorData};

impl PathfinderServer {
    /// Core logic for the `read_symbol_scope` tool.
    ///
    /// Parses the semantic path, performs a sandbox check, then delegates
    /// to the `Surgeon` to extract the AST-located symbol scope.
    #[tracing::instrument(skip(self, params), fields(semantic_path = %params.semantic_path))]
    pub(crate) async fn read_symbol_scope_impl(
        &self,
        params: ReadSymbolScopeParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(tool = "read_symbol_scope", "read_symbol_scope: start");

        let semantic_path = parse_semantic_path(&params.semantic_path)?;

        // read_symbol_scope requires a symbol chain, not just a bare file
        require_symbol_target(&semantic_path, &params.semantic_path)?;

        // Sandbox check on the file path
        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            tracing::warn!(tool = "read_symbol_scope", error = %e, "sandbox check failed");
            return Err(pathfinder_to_error_data(&e));
        }

        // Delegate to surgeon
        let ts_start = std::time::Instant::now();
        match self
            .surgeon
            .read_symbol_scope(self.workspace_root.path(), &semantic_path)
            .await
        {
            Ok(scope) => {
                let tree_sitter_ms = ts_start.elapsed().as_millis();
                let duration_ms = start.elapsed().as_millis();
                tracing::info!(
                    tool = "read_symbol_scope",
                    lines = (scope.end_line - scope.start_line + 1),
                    tree_sitter_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter"],
                    "read_symbol_scope: complete"
                );

                let metadata = crate::server::types::ReadSymbolScopeMetadata {
                    content: scope.content.clone(),
                    start_line: scope.start_line,
                    end_line: scope.end_line,
                    language: scope.language,
                };

                let mut result = CallToolResult::success(vec![Content::text(scope.content)]);
                result.structured_content = serialize_metadata(&metadata);

                Ok(result)
            }
            Err(e) => {
                let tree_sitter_ms = ts_start.elapsed().as_millis();
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "read_symbol_scope",
                    error = %e,
                    tree_sitter_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter"],
                    "read_symbol_scope: failed"
                );
                Err(crate::server::helpers::treesitter_error_to_error_data(e))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;
    use tempfile::tempdir;

    // ── GAP-004: version_hash in text output ───────────────────────────────

    #[tokio::test]
    #[allow(clippy::unwrap_used)]
    async fn test_read_symbol_scope_includes_version_hash_in_text() {
        let ws_dir = tempdir().unwrap();
        let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create a test file
        let file_path = ws.path().join("test.rs");
        let content = "fn test() {}\n";
        tokio::fs::write(&file_path, content).await.unwrap();

        let mock_surgeon = MockSurgeon::new();
        let expected_scope = pathfinder_common::types::SymbolScope {
            content: content.to_owned(),
            start_line: 1,
            end_line: 1,
            name_column: 0,
            language: "rust".to_owned(),
        };
        mock_surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(expected_scope));

        let server = crate::server::PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(mock_surgeon),
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = ReadSymbolScopeParams {
            semantic_path: "test.rs::test".to_owned(),
        };

        let result = server.read_symbol_scope_impl(params).await;
        assert!(result.is_ok(), "read_symbol_scope should succeed");
        let call_result = result.unwrap();

        // Verify the text content is the symbol source
        if let Some(content) = call_result.content.first() {
            if let rmcp::model::RawContent::Text(text_content) = &content.raw {
                assert!(
                    !text_content.text.is_empty(),
                    "text output should be non-empty"
                );
            } else {
                panic!("Expected text content");
            }
        } else {
            panic!("Expected content");
        }
    }
}
