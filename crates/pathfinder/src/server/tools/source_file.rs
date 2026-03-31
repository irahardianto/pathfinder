//! `read_source_file` tool — AST-based full file symbol extraction via Tree-sitter.

use crate::server::helpers::{pathfinder_to_error_data, treesitter_error_to_error_data};
use crate::server::types::{ReadSourceFileParams, ReadSourceFileResponse, SourceSymbol};
use crate::server::PathfinderServer;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;

fn map_symbols(syms: Vec<pathfinder_treesitter::surgeon::ExtractedSymbol>) -> Vec<SourceSymbol> {
    syms.into_iter()
        .map(|s| SourceSymbol {
            name: s.name,
            semantic_path: s.semantic_path,
            kind: format!("{:?}", s.kind),
            start_line: s.start_line + 1, // AST lines are 0-indexed, UI is 1-indexed
            end_line: s.end_line + 1,
            children: map_symbols(s.children),
        })
        .collect()
}

impl PathfinderServer {
    /// Core logic for the `read_source_file` tool.
    ///
    /// Performs a sandbox check, then delegates to the `Surgeon` to extract
    /// the AST hierarchy and read the full source context.
    #[tracing::instrument(skip(self, params), fields(file = %params.filepath))]
    pub(crate) async fn read_source_file_impl(
        &self,
        params: ReadSourceFileParams,
    ) -> Result<Json<ReadSourceFileResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(tool = "read_source_file", "read_source_file: start");

        let file_path = std::path::Path::new(&params.filepath);

        // Sandbox check on the file path
        if let Err(e) = self.sandbox.check(file_path) {
            tracing::warn!(tool = "read_source_file", error = %e, "sandbox check failed");
            return Err(pathfinder_to_error_data(&e));
        }

        // Delegate to surgeon
        let ts_start = std::time::Instant::now();
        match self
            .surgeon
            .read_source_file(self.workspace_root.path(), file_path)
            .await
        {
            Ok((content, version_hash, language, symbols)) => {
                let tree_sitter_ms = ts_start.elapsed().as_millis();
                let duration_ms = start.elapsed().as_millis();
                tracing::info!(
                    tool = "read_source_file",
                    tree_sitter_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter"],
                    "read_source_file: complete"
                );

                Ok(Json(ReadSourceFileResponse {
                    content,
                    version_hash: version_hash.to_string(),
                    language,
                    symbols: map_symbols(symbols),
                }))
            }
            Err(e) => {
                let tree_sitter_ms = ts_start.elapsed().as_millis();
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "read_source_file",
                    error = %e,
                    tree_sitter_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter"],
                    "read_source_file: failed"
                );
                Err(treesitter_error_to_error_data(e))
            }
        }
    }
}
