//! `read_symbol_scope` tool — AST-based symbol extraction via Tree-sitter.

use crate::server::helpers::{
    parse_semantic_path, pathfinder_to_error_data, require_symbol_target,
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
                    start_line: scope.start_line,
                    end_line: scope.end_line,
                    version_hash: scope.version_hash.to_string(),
                    language: scope.language,
                };

                let mut result = CallToolResult::success(vec![Content::text(scope.content)]);
                result.structured_content =
                    Some(serde_json::to_value(&metadata).unwrap_or_default());

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
