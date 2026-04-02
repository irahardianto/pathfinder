//! `read_symbol_scope` tool — AST-based symbol extraction via Tree-sitter.

use crate::server::helpers::pathfinder_to_error_data;
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
    #[tracing::instrument(skip(self, params), fields(semantic_path = %params.semantic_path))]
    pub(crate) async fn read_symbol_scope_impl(
        &self,
        params: ReadSymbolScopeParams,
    ) -> Result<Json<ReadSymbolScopeResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(tool = "read_symbol_scope", "read_symbol_scope: start");

        let Some(semantic_path) = SemanticPath::parse(&params.semantic_path) else {
            let err = pathfinder_common::error::PathfinderError::InvalidSemanticPath {
                input: params.semantic_path.clone(),
                issue: "Semantic path is malformed or missing '::' separator.".to_owned(),
            };
            return Err(pathfinder_to_error_data(&err));
        };

        // read_symbol_scope requires a symbol chain, not just a bare file
        if semantic_path.is_bare_file() {
            let err = pathfinder_common::error::PathfinderError::InvalidSemanticPath {
                input: params.semantic_path.clone(),
                issue: "this tool requires a symbol target — use 'file.rs::symbol' format"
                    .to_owned(),
            };
            return Err(pathfinder_to_error_data(&err));
        }

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

                Ok(Json(ReadSymbolScopeResponse {
                    content: scope.content,
                    start_line: scope.start_line,
                    end_line: scope.end_line,
                    version_hash: scope.version_hash.to_string(),
                    language: scope.language,
                }))
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
