//! `search_codebase` tool — Ripgrep-backed text search with Tree-sitter enrichment.

use crate::server::helpers::io_error_data;
use crate::server::types::{SearchCodebaseParams, SearchCodebaseResponse};
use crate::server::PathfinderServer;
use pathfinder_common::types::FilterMode;
use pathfinder_search::SearchParams;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;
use std::path::Path;

impl PathfinderServer {
    /// Core logic for the `search_codebase` tool.
    ///
    /// Runs Ripgrep across the workspace, then enriches each match with its
    /// `enclosing_semantic_path` via Tree-sitter. Sets `degraded = true` when
    /// `filter_mode != All` (Tree-sitter filtering is not yet implemented).
    pub(crate) async fn search_codebase_impl(
        &self,
        params: SearchCodebaseParams,
    ) -> Result<Json<SearchCodebaseResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "search_codebase",
            query = %params.query,
            is_regex = params.is_regex,
            path_glob = %params.path_glob,
            filter_mode = ?params.filter_mode,
            "search_codebase: start"
        );

        // Note: filter_mode requires Tree-sitter (Epic 3).
        // In Epic 2 we set `degraded: true` if a filtered mode was requested.
        let degraded = params.filter_mode != FilterMode::All;

        let search_params = SearchParams {
            workspace_root: self.workspace_root.path().to_path_buf(),
            query: params.query.clone(),
            is_regex: params.is_regex,
            path_glob: params.path_glob.clone(),
            max_results: params.max_results as usize,
            context_lines: params.context_lines as usize,
        };

        match self.scout.search(&search_params).await {
            Ok(result) => {
                let mut enriched_matches = result.matches;

                // Populate enclosing_semantic_path using Surgeon
                for m in &mut enriched_matches {
                    let file_path = Path::new(&m.file);
                    if let Ok(Some(symbol)) = self
                        .surgeon
                        .enclosing_symbol(
                            self.workspace_root.path(),
                            file_path,
                            usize::try_from(m.line).unwrap_or(usize::MAX),
                        )
                        .await
                    {
                        m.enclosing_semantic_path = Some(format!("{}::{}", m.file, symbol));
                    }
                }

                let duration_ms = start.elapsed().as_millis();
                tracing::info!(
                    tool = "search_codebase",
                    total_matches = result.total_matches,
                    returned = enriched_matches.len(),
                    truncated = result.truncated,
                    duration_ms,
                    engines_used = ?["ripgrep", "treesitter"],
                    degraded,
                    "search_codebase: complete"
                );

                let mut response = SearchCodebaseResponse {
                    matches: enriched_matches,
                    total_matches: result.total_matches,
                    truncated: result.truncated,
                    degraded: None,
                    degraded_reason: None,
                };

                if degraded {
                    response.degraded = Some(true);
                    response.degraded_reason = Some(
                        "filter_mode requires Tree-sitter (available in Epic 3); returning unfiltered results"
                            .to_owned(),
                    );
                }

                Ok(Json(response))
            }
            Err(err) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "search_codebase",
                    error = %err,
                    error_code = "INTERNAL_ERROR",
                    error_message = %err,
                    duration_ms,
                    engines_used = ?["ripgrep"],
                    "search_codebase: failed"
                );
                Err(io_error_data(err.to_string()))
            }
        }
    }
}
