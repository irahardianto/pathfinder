//! `inspect` tool (symbol scope mode) — AST-based symbol extraction via Tree-sitter.

use crate::server::helpers::{
    millis_to_u64, parse_semantic_path, pathfinder_to_error_data, require_symbol_target,
    serialize_metadata,
};
use crate::server::types::InspectParams;
use crate::server::PathfinderServer;
use rmcp::model::{CallToolResult, Content, ErrorData};

impl PathfinderServer {
    /// Consolidated `inspect` handler.
    pub(crate) async fn inspect_impl(
        &self,
        params: InspectParams,
    ) -> Result<CallToolResult, ErrorData> {
        if params.include_dependencies {
            self.read_with_deep_context_impl(params).await
        } else {
            self.read_symbol_scope_impl(params).await
        }
    }

    /// Core logic for the `read_symbol_scope` tool.
    ///
    /// Parses the semantic path, performs a sandbox check, then delegates
    /// to the `Surgeon` to extract the AST-located symbol scope.
    #[tracing::instrument(skip(self, params), fields(semantic_path = %params.semantic_path))]
    pub(crate) async fn read_symbol_scope_impl(
        &self,
        params: InspectParams,
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

        // Early file existence check — avoid tree-sitter parse on nonexistent files
        let abs_file = self.workspace_root.path().join(&semantic_path.file_path);
        if !abs_file.exists() {
            let err = pathfinder_common::error::PathfinderError::FileNotFound {
                path: abs_file.clone(),
            };
            tracing::warn!(
                tool = "read_symbol_scope",
                path = %abs_file.display(),
                "file not found"
            );
            return Err(pathfinder_to_error_data(&err));
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
                    duration_ms: Some(millis_to_u64(duration_ms)),
                };

                let text = format!("{}\n[completed in {duration_ms}ms]", scope.content);
                let mut result = CallToolResult::success(vec![Content::text(text)]);
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
#[path = "symbols_test.rs"]
mod tests;
