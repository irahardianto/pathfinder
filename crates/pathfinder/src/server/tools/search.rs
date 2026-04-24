//! `search_codebase` tool — Ripgrep-backed text search with Tree-sitter enrichment.

use crate::server::helpers::io_error_data;
use crate::server::types::{
    GroupedKnownMatch, GroupedMatch, SearchCodebaseParams, SearchCodebaseResponse,
    SearchResultGroup,
};
use crate::server::PathfinderServer;
use futures::StreamExt as _;
use pathfinder_common::types::FilterMode;
use pathfinder_search::{SearchMatch, SearchParams};
use pathfinder_treesitter::language::SupportedLanguage;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;
use std::collections::HashMap;
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
    ///
    /// **E4 features:**
    /// - `exclude_glob` is passed to the scout to exclude files before search.
    /// - `known_files` suppresses verbose context for files the agent already has.
    /// - `group_by_file` clusters results into `file_groups` in the response.
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
            exclude_glob = %params.exclude_glob,
            known_files_count = params.known_files.len(),
            group_by_file = params.group_by_file,
            filter_mode = ?params.filter_mode,
            "search_codebase: start"
        );

        let search_params = SearchParams {
            workspace_root: self.workspace_root.path().to_path_buf(),
            query: params.query.clone(),
            is_regex: params.is_regex,
            path_glob: params.path_glob.clone(),
            exclude_glob: params.exclude_glob.clone(),
            max_results: params.max_results as usize,
            context_lines: params.context_lines as usize,
        };

        let ripgrep_start = std::time::Instant::now();
        match self.scout.search(&search_params).await {
            Ok(result) => {
                let ripgrep_ms = ripgrep_start.elapsed().as_millis();

                let mut enriched_matches = result.matches;

                let ts_start = std::time::Instant::now();
                let node_types = self.enrich_matches(&mut enriched_matches).await;
                let tree_sitter_parse_ms = ts_start.elapsed().as_millis();

                // Detect degradation: any matched file with no Tree-sitter grammar
                // means enrichment fell back to "code" for all its matches.
                let degraded = enriched_matches
                    .iter()
                    .any(|m| SupportedLanguage::detect(Path::new(&m.file)).is_none());
                let degraded_reason = if degraded {
                    Some("unsupported_language".to_owned())
                } else {
                    None
                };

                let filtered_matches =
                    apply_filter_mode(enriched_matches, &node_types, params.filter_mode);

                // Build the normalised known-file set for O(1) lookups.
                // Strip a leading "./" so that "./src/auth.ts" == "src/auth.ts".
                let known_set: std::collections::HashSet<String> = params
                    .known_files
                    .iter()
                    .map(|p| normalize_path(p))
                    .collect();

                // Build grouped output if requested.
                let file_groups = if params.group_by_file {
                    Some(build_file_groups(&filtered_matches, &known_set))
                } else {
                    None
                };

                // For the flat `matches` list, strip content/context from known files
                // so the response is still useful even when grouping is off.
                let flat_matches: Vec<SearchMatch> = filtered_matches
                    .into_iter()
                    .map(|mut m| {
                        if known_set.contains(&normalize_path(&m.file)) {
                            m.content = String::new();
                            m.context_before = vec![];
                            m.context_after = vec![];
                            m.known = Some(true);
                        }
                        m
                    })
                    .collect();

                let returned_count = flat_matches.len();
                let duration_ms = start.elapsed().as_millis();
                tracing::info!(
                    tool = "search_codebase",
                    total_matches = result.total_matches,
                    returned = returned_count,
                    truncated = result.truncated,
                    filter_mode = ?params.filter_mode,
                    ripgrep_ms,
                    tree_sitter_parse_ms,
                    duration_ms,
                    engines_used = ?["ripgrep", "treesitter"],
                    "search_codebase: complete"
                );

                Ok(Json(SearchCodebaseResponse {
                    matches: flat_matches,
                    total_matches: result.total_matches,
                    truncated: result.truncated,
                    file_groups,
                    degraded,
                    degraded_reason,
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

        let enrichment: Vec<EnrichResult> = futures::stream::iter(snapshots)
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

/// Strip a leading `./` from a path string for normalised comparison.
///
/// Both stored match paths and caller-supplied `known_files` entries may or may
/// not have a leading `./`. Normalising both sides ensures consistent lookups.
fn normalize_path(p: &str) -> String {
    p.strip_prefix("./").unwrap_or(p).to_owned()
}

/// Build the grouped response for `group_by_file: true`.
///
/// Groups matches by their `file` field, deduplicating `version_hash` per group.
/// Matches for files in `known_set` are rendered as `KnownFileMatch` (minimal);
/// all others are full `SearchMatch` entries.
fn build_file_groups(
    matches: &[SearchMatch],
    known_set: &std::collections::HashSet<String>,
) -> Vec<SearchResultGroup> {
    // Preserve insertion order by tracking the file sequence separately.
    let mut order: Vec<String> = Vec::new();
    let mut groups: HashMap<String, SearchResultGroup> = HashMap::new();

    for m in matches {
        let key = normalize_path(&m.file);
        if !groups.contains_key(&key) {
            order.push(key.clone());
            groups.insert(
                key.clone(),
                SearchResultGroup {
                    file: m.file.clone(),
                    version_hash: m.version_hash.clone(),
                    matches: Vec::new(),
                    known_matches: Vec::new(),
                },
            );
        }
        if let Some(group) = groups.get_mut(&key) {
            if known_set.contains(&key) {
                group.known_matches.push(GroupedKnownMatch {
                    line: m.line,
                    column: m.column,
                    enclosing_semantic_path: m.enclosing_semantic_path.clone(),
                    known: true,
                });
            } else {
                group.matches.push(GroupedMatch {
                    line: m.line,
                    column: m.column,
                    content: m.content.clone(),
                    context_before: m.context_before.clone(),
                    context_after: m.context_after.clone(),
                    enclosing_semantic_path: m.enclosing_semantic_path.clone(),
                });
            }
        }
    }

    order
        .into_iter()
        .filter_map(|k| groups.remove(&k))
        .collect()
}
