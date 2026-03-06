//! `read_symbol_scope` tool — AST-based symbol extraction via Tree-sitter.

use crate::server::helpers::{io_error_data, pathfinder_to_error_data};
use crate::server::types::{ReadSymbolScopeParams, ReadSymbolScopeResponse};
use crate::server::PathfinderServer;
use pathfinder_common::types::SemanticPath;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;

impl PathfinderServer {
    /// Core logic for the `read_symbol_scope` tool.
    ///
    /// Parses the semantic path, performs a sandbox check, then delegates
    /// to the `Surgeon` to extract the AST-located symbol scope.
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn read_symbol_scope_impl(
        &self,
        params: ReadSymbolScopeParams,
    ) -> Result<Json<ReadSymbolScopeResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "read_symbol_scope",
            semantic_path = %params.semantic_path,
            "read_symbol_scope: start"
        );

        let Some(semantic_path) = SemanticPath::parse(&params.semantic_path) else {
            let duration_ms = start.elapsed().as_millis();
            let e = "invalid semantic path format";
            tracing::warn!(
                tool = "read_symbol_scope",
                error = %e,
                error_code = "INVALID_TARGET",
                error_message = %e,
                duration_ms,
                engines_used = ?(&[] as &[&str]),
                "invalid semantic path"
            );
            return Err(io_error_data(e.to_string()));
        };

        // Sandbox check on the file path
        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "read_symbol_scope",
                error = %e,
                error_code = e.error_code(),
                error_message = %e,
                duration_ms,
                engines_used = ?(&[] as &[&str]),
                "sandbox check failed"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // Delegate to surgeon
        match self
            .surgeon
            .read_symbol_scope(self.workspace_root.path(), &semantic_path)
            .await
        {
            Ok(scope) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::info!(
                    tool = "read_symbol_scope",
                    semantic_path = %params.semantic_path,
                    lines = (scope.end_line - scope.start_line + 1),
                    duration_ms,
                    engines_used = ?["tree-sitter"],
                    "read_symbol_scope: complete"
                );

                Ok(Json(ReadSymbolScopeResponse {
                    content: scope.content,
                    start_line: scope.start_line,
                    end_line: scope.end_line,
                    version_hash: scope.version_hash.to_string(),
                    language: scope.language,
                }))
            }
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();

                // Convert SurgeonError to PathfinderError if possible, or io_error
                let error_data = crate::server::helpers::treesitter_error_to_error_data(
                    &e,
                    &params.semantic_path,
                    &semantic_path.file_path,
                );

                tracing::warn!(
                    tool = "read_symbol_scope",
                    error = %e,
                    error_message = %e,
                    duration_ms,
                    engines_used = ?["tree-sitter"],
                    "read_symbol_scope: failed"
                );
                Err(error_data)
            }
        }
    }
}
