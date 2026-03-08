//! `search_codebase` tool — Ripgrep-backed text search with Tree-sitter enrichment.

use crate::server::helpers::io_error_data;
use crate::server::types::{SearchCodebaseParams, SearchCodebaseResponse};
use crate::server::PathfinderServer;
use futures::StreamExt as _;
use pathfinder_common::types::FilterMode;
use pathfinder_search::{SearchMatch, SearchParams};
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;
use std::path::Path;

/// Maximum number of concurrent Tree-sitter enrichment futures per search call.
/// Prevents unbounded memory growth when `max_results` is set to a large value.
const ENRICHMENT_CONCURRENCY: usize = 32;

/// Per-match enrichment output: `(enclosing_symbol_path, node_type)`.
type EnrichResult = (Option<String>, String);

impl PathfinderServer {
    /// Core logic for the `search_codebase` tool.
    ///
    /// Runs Ripgrep across the workspace, then concurrently enriches each match with:
    /// 1. `enclosing_semantic_path` — the AST symbol containing the match
    /// 2. Node-type classification — determines if at a comment, string, or code position
    ///
    /// After enrichment, matches are filtered by `filter_mode` (`code_only` / `comments_only`).
    /// `degraded: true` is only set when a file uses an unsupported language (no Tree-sitter grammar).
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
                let node_types = self.enrich_matches(&mut enriched_matches).await;

                let filtered_matches =
                    apply_filter_mode(enriched_matches, &node_types, params.filter_mode);

                let returned_count = filtered_matches.len();
                let duration_ms = start.elapsed().as_millis();
                tracing::info!(
                    tool = "search_codebase",
                    total_matches = result.total_matches,
                    returned = returned_count,
                    truncated = result.truncated,
                    filter_mode = ?params.filter_mode,
                    duration_ms,
                    engines_used = ?["ripgrep", "treesitter"],
                    "search_codebase: complete"
                );

                // TODO: track per-file degradation when node_type_at_position
                // falls back to "code" on unsupported languages.

                Ok(Json(SearchCodebaseResponse {
                    matches: filtered_matches,
                    total_matches: result.total_matches,
                    truncated: result.truncated,
                    degraded: None,
                    degraded_reason: None,
                }))
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

    /// Enrich a slice of search matches with Tree-sitter metadata.
    ///
    /// - Populates `enclosing_semantic_path` on each match.
    /// - Returns a parallel `Vec<String>` of node-type classifications (`"code"`,
    ///   `"comment"`, or `"string"`).
    ///
    /// Enrichment runs concurrently, capped at [`ENRICHMENT_CONCURRENCY`] to bound
    /// memory and thread contention when the match list is large.
    ///
    /// # Design note
    /// Uses a three-phase snapshot approach to avoid holding `&mut SearchMatch`
    /// across an async boundary (which violates Rust's higher-ranked lifetime rules):
    /// Phase 1 — snapshot owned file/line/column per match.
    /// Phase 2 — enrich concurrently with `buffer_unordered`.
    /// Phase 3 — zip results back and mutate matches.
    async fn enrich_matches(&self, matches: &mut [SearchMatch]) -> Vec<String> {
        let snapshots: Vec<(String, u64, u64)> = matches
            .iter()
            .map(|m| (m.file.clone(), m.line, m.column))
            .collect();

        let enrichment: Vec<EnrichResult> = futures::stream::iter(snapshots.into_iter())
            .map(|(file, line_u64, column_u64)| async move {
                let file_path = Path::new(&file);
                let line = usize::try_from(line_u64).unwrap_or(usize::MAX);
                let column = usize::try_from(column_u64).unwrap_or(0);

                let symbol = self
                    .surgeon
                    .enclosing_symbol(self.workspace_root.path(), file_path, line)
                    .await
                    .ok()
                    .flatten()
                    .map(|s| format!("{file}::{s}"));

                let node_type = self
                    .surgeon
                    .node_type_at_position(self.workspace_root.path(), file_path, line, column)
                    .await
                    .unwrap_or_else(|_| "code".to_owned()); // degrade gracefully on unsupported files

                (symbol, node_type)
            })
            .buffer_unordered(ENRICHMENT_CONCURRENCY)
            .collect()
            .await;

        enrichment
            .into_iter()
            .zip(matches.iter_mut())
            .map(|((symbol, node_type), m)| {
                m.enclosing_semantic_path = symbol;
                node_type
            })
            .collect()
    }
}

/// Apply `filter_mode` to a list of enriched matches using pre-computed node types.
///
/// - `All` — return all matches unchanged
/// - `CodeOnly` — retain only matches classified as `"code"`
/// - `CommentsOnly` — retain only matches classified as `"comment"` or `"string"`
fn apply_filter_mode(
    matches: Vec<SearchMatch>,
    node_types: &[String],
    mode: FilterMode,
) -> Vec<SearchMatch> {
    match mode {
        FilterMode::All => matches,
        FilterMode::CodeOnly => matches
            .into_iter()
            .zip(node_types.iter())
            .filter(|(_, t)| t.as_str() == "code")
            .map(|(m, _)| m)
            .collect(),
        FilterMode::CommentsOnly => matches
            .into_iter()
            .zip(node_types.iter())
            .filter(|(_, t)| t.as_str() == "comment" || t.as_str() == "string")
            .map(|(m, _)| m)
            .collect(),
    }
}
