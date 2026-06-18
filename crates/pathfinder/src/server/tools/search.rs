//! `search` tool (text/regex mode) — Ripgrep-backed text search with Tree-sitter enrichment.

use crate::server::helpers::{invalid_params_error, io_error_data, millis_to_u64};
use crate::server::types::{
    GroupedKnownMatch, GroupedMatch, SearchCodebaseResponse, SearchMode, SearchParams,
    SearchResultGroup,
};
use crate::server::PathfinderServer;
use futures::StreamExt as _;
use pathfinder_common::types::{DegradedReason, FilterMode};
use pathfinder_search::{SearchMatch, SearchParams as ScoutSearchParams};
use pathfinder_treesitter::language::SupportedLanguage;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;
use std::collections::HashMap;
use std::path::Path;

/// Maximum number of concurrent Tree-sitter enrichment futures per search call.
/// Prevents unbounded memory growth when `max_results` is set to a large value.
const ENRICHMENT_CONCURRENCY: usize = 32;

/// Per-match enrichment output: `(enclosing_semantic_path, node_type, is_definition)`.
type EnrichResult = (Option<String>, String, Option<bool>);

impl PathfinderServer {
    /// Core logic for the `search_codebase` tool.
    ///
    /// Runs Ripgrep across the workspace, then concurrently enriches each match with:
    /// 1. `enclosing_semantic_path` — the AST symbol containing the match
    /// 2. Node-type classification — determines if at a comment, string, or code position
    ///
    /// After enrichment, matches are filtered by `filter_mode` (`code_only` / `comments_only`).
    /// `degraded: true` is set when a file uses an unsupported language (no Tree-sitter grammar).
    /// When degraded, `filter_mode` is **bypassed** (all matches returned) to prevent silent
    /// result-loss. The `degraded_reason` is set to `"unsupported_language_filter_bypassed"` in
    /// this case so agents know filtering was not applied.
    ///
    /// **E4 features:**
    /// - `exclude_glob` is passed to the scout to exclude files before search.
    /// - `known_files` suppresses verbose context for files the agent already has.
    /// - `group_by_file` clusters results into `file_groups` in the response.
    pub(crate) async fn search_impl(
        &self,
        params: SearchParams,
    ) -> Result<rmcp::model::CallToolResult, ErrorData> {
        match params.mode {
            SearchMode::Text | SearchMode::Regex => {
                let result = self.search_codebase_impl(params).await?;
                let text = serde_json::to_string_pretty(&result.0)
                    .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}"));
                Ok(rmcp::model::CallToolResult::success(vec![
                    rmcp::model::Content::text(text),
                ]))
            }
            SearchMode::Symbol => {
                let result = self.find_symbol_impl(params).await?;
                let text = serde_json::to_string_pretty(&result.0)
                    .unwrap_or_else(|e| format!("{{\"error\": \"serialization failed: {e}\"}}"));
                Ok(rmcp::model::CallToolResult::success(vec![
                    rmcp::model::Content::text(text),
                ]))
            }
        }
    }

    /// Core logic for the `search` tool (text/regex mode).
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential pipeline: ripgrep → TS enrichment → filter → group → response. Extraction done at helper level; remaining orchestration is linear."
    )]
    pub(crate) async fn search_codebase_impl(
        &self,
        params: SearchParams,
    ) -> Result<Json<SearchCodebaseResponse>, ErrorData> {
        let start = std::time::Instant::now();
        let is_regex = matches!(params.mode, SearchMode::Regex);
        let group_by_file = params.group_by_file;
        let filter_mode = params.filter_mode;

        tracing::info!(
            tool = "search_codebase",
            query = %params.query,
            is_regex,
            path_glob = %params.path_glob,
            exclude_glob = %params.exclude_glob,
            known_files_count = params.known_files.len(),
            group_by_file,
            filter_mode = ?filter_mode,
            "search_codebase: start"
        );

        if params.query.trim().is_empty() {
            return Err(invalid_params_error("query must not be empty"));
        }

        let search_params = ScoutSearchParams {
            workspace_root: self.workspace_root.path().to_path_buf(),
            query: params.query.clone(),
            is_regex,
            path_glob: params.path_glob.clone(),
            exclude_glob: params.exclude_glob.clone(),
            max_results: params.max_results as usize,
            offset: params.offset as usize,
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

                // When degraded (unsupported language), node-type classification is unavailable.
                // Bypass the filter entirely rather than silently dropping every match.
                // Agents that requested `comments_only` will receive all matches labelled with
                // `degraded: true` and `degraded_reason: "unsupported_language_filter_bypassed"`
                // so they can decide how to handle the results themselves. Returning zero matches
                // would cause agents to falsely conclude the codebase has no results.
                let filter_was_bypassed = degraded && filter_mode != FilterMode::All;
                let filtered_matches = if filter_was_bypassed {
                    enriched_matches
                } else {
                    apply_filter_mode(enriched_matches, &node_types, filter_mode)
                };

                let degraded_reason = if filter_was_bypassed {
                    Some(DegradedReason::UnsupportedLanguageFilterBypassed)
                } else if degraded {
                    Some(DegradedReason::UnsupportedLanguage)
                } else {
                    None
                };

                // Build the normalised known-file set for O(1) lookups.
                // Strip a leading "./" so that "./src/auth.ts" == "src/auth.ts".
                let known_set: std::collections::HashSet<String> = params
                    .known_files
                    .iter()
                    .map(|p| normalize_path(p).into_owned())
                    .collect();

                // Build grouped output if requested.
                let file_groups = if group_by_file {
                    Some(build_file_groups(&filtered_matches, &known_set))
                } else {
                    None
                };

                // For the flat `matches` list, strip content/context from known files
                // so the response is still useful even when grouping is off.
                let flat_matches: Vec<SearchMatch> = filtered_matches
                    .into_iter()
                    .map(|mut m| {
                        if known_set.contains(&*normalize_path(&m.file)) {
                            m.content = String::default();
                            m.context_before = vec![];
                            m.context_after = vec![];
                            m.known = Some(true);
                        }
                        m
                    })
                    .collect();

                let returned_count = flat_matches.len();
                let duration_ms = start.elapsed().as_millis();
                let files_searched = result.files_searched;
                let files_in_scope = result.files_in_scope;
                let coverage_percent: u8 = files_searched
                    .saturating_mul(100)
                    .checked_div(files_in_scope)
                    .and_then(|v| v.try_into().ok())
                    .unwrap_or(100);

                let filter_mode_name = match filter_mode {
                    FilterMode::All => "all",
                    FilterMode::CodeOnly => "code_only",
                    FilterMode::CommentsOnly => "comments_only",
                };

                let hint = if returned_count == 0
                    && result.total_matches > 0
                    && filter_mode != FilterMode::All
                {
                    Some(format!(
                        "0 matches with filter_mode={} but {} match(es) exist with filter_mode=all. Retry with filter_mode='all' to see all match types.",
                        filter_mode_name,
                        result.total_matches,
                    ))
                } else if returned_count == 0 && result.total_matches == 0 {
                    // P2-7: Zero total matches — help the agent recover.
                    let mut msg = "No matches found. Check spelling, try a regex pattern, or broaden path_glob.".to_owned();
                    if coverage_percent < 100 {
                        use std::fmt::Write;
                        let _ = write!(
                            msg,
                            " Note: only {coverage_percent}% of in-scope files were searched."
                        );
                    }
                    Some(msg)
                } else if coverage_percent < 50 {
                    // Low coverage with results: warn the agent that a significant portion of the
                    // codebase was not searched. Check skip counters for the cause.
                    Some(format!(
                        "Low coverage: only {coverage_percent}% of in-scope files were searched. \
                        Results may be incomplete. \
                        Possible causes: binary_skipped={binary}, gitignored_skipped={gitignored}, other_skipped={other}. \
                        Try narrowing path_glob or checking .gitignore rules.",
                        coverage_percent = coverage_percent,
                        binary = result.binary_skipped,
                        gitignored = result.gitignored_skipped,
                        other = result.other_skipped,
                    ))
                } else {
                    None
                };

                tracing::info!(
                    tool = "search_codebase",
                    total_matches = result.total_matches,
                    returned = returned_count,
                    truncated = result.truncated,
                    files_searched,
                    files_in_scope,
                    coverage_percent,
                    filter_mode = ?filter_mode,
                    filter_bypassed = filter_was_bypassed,
                    hint_emitted = hint.is_some(),
                    ripgrep_ms,
                    tree_sitter_parse_ms,
                    duration_ms,
                    engines_used = ?["ripgrep", "treesitter"],
                    "search_codebase: complete"
                );

                Ok(Json(SearchCodebaseResponse {
                    matches: flat_matches,
                    raw_match_count: result.total_matches,
                    total_matches: returned_count,
                    returned_count,
                    filtered_count: result.total_matches.saturating_sub(returned_count),
                    files_searched,
                    files_in_scope,
                    coverage_percent,
                    truncated: result.truncated,
                    file_groups,
                    degraded,
                    degraded_reason,
                    actionable_guidance: degraded_reason.as_ref().map(DegradedReason::guidance),
                    hint,
                    next_offset: if result.truncated {
                        #[allow(clippy::cast_possible_truncation)]
                        Some(params.offset + (returned_count as u32))
                    } else {
                        None
                    },
                    duration_ms: Some(millis_to_u64(duration_ms)),
                    binary_skipped: result.binary_skipped,
                    gitignored_skipped: result.gitignored_skipped,
                    other_skipped: result.other_skipped,
                }))
            }
            Err(err) => {
                let duration_ms = start.elapsed().as_millis();

                // Invalid regex/glob patterns are client errors (bad input),
                // not server failures. Return INVALID_PARAMS (-32602).
                // SearchError::InvalidPattern formats as "invalid {type} pattern: ..."
                let err_msg = err.to_string();
                if err_msg.starts_with("invalid regex pattern:")
                    || err_msg.starts_with("invalid path_glob:")
                    || err_msg.starts_with("invalid exclude_glob:")
                {
                    tracing::info!(
                        tool = "search_codebase",
                        error = %err,
                        error_code = "INVALID_PARAMS",
                        duration_ms,
                        "search_codebase: invalid pattern"
                    );
                    return Err(ErrorData::new(
                        rmcp::model::ErrorCode::INVALID_PARAMS,
                        format!("invalid pattern: {err_msg}"),
                        None,
                    ));
                }

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

                let detail = self
                    .surgeon
                    .enclosing_symbol_detail(self.workspace_root.path(), file_path, line)
                    .await
                    .ok()
                    .flatten();

                let (symbol, is_definition) = if let Some(ref sym) = detail {
                    let path = format!("{file}::{}", sym.semantic_path);
                    // Guard: ripgrep returns 1-indexed lines, tree-sitter uses 0-indexed.
                    // `saturating_sub(1)` converts; assert catches unexpected 0-indexed input.
                    debug_assert!(line > 0, "ripgrep line should be 1-indexed, got 0");
                    let is_def = sym.start_line == line.saturating_sub(1);
                    (Some(path), Some(is_def))
                } else {
                    (None, None)
                };

                let node_type = self
                    .surgeon
                    .node_type_at_position(self.workspace_root.path(), file_path, line, column)
                    .await
                    .unwrap_or_else(|_| "code".to_owned());

                (symbol, node_type, is_definition)
            })
            .buffered(ENRICHMENT_CONCURRENCY)
            .collect()
            .await;

        enrichment
            .into_iter()
            .zip(matches.iter_mut())
            .map(|((symbol, node_type, is_definition), m)| {
                m.enclosing_semantic_path = symbol;
                m.is_definition = is_definition;
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
///
/// Returns a `Cow` to avoid allocation when no stripping is needed (~50% of
/// typical ripgrep output has no `./` prefix).
fn normalize_path(p: &str) -> std::borrow::Cow<'_, str> {
    match p.strip_prefix("./") {
        Some(stripped) => std::borrow::Cow::Borrowed(stripped),
        None => std::borrow::Cow::Borrowed(p),
    }
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
    // OPT-5: Pre-allocate with reasonable capacity estimates.
    // Unique files are typically much fewer than total matches.
    let estimated_files = matches.len().min(64);
    let mut order: Vec<String> = Vec::with_capacity(estimated_files);
    let mut groups: HashMap<String, SearchResultGroup> = HashMap::with_capacity(estimated_files);

    for m in matches {
        let key = normalize_path(&m.file);
        if !groups.contains_key(&*key) {
            let key_owned = key.clone().into_owned();
            order.push(key_owned.clone());
            groups.insert(
                key_owned,
                SearchResultGroup {
                    file: m.file.clone(),
                    version_hash: m.version_hash.clone(),
                    total_matches: 0,
                    matches: Vec::new(),
                    known_matches: Vec::new(),
                },
            );
        }
        if let Some(group) = groups.get_mut(&*key) {
            if known_set.contains(&*key) {
                group.known_matches.push(GroupedKnownMatch {
                    line: m.line,
                    column: m.column,
                    enclosing_semantic_path: m.enclosing_semantic_path.clone(),
                    is_definition: m.is_definition,
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
                    is_definition: m.is_definition,
                });
            }
        }
    }

    // Set total_matches for each group before returning
    for group in groups.values_mut() {
        group.total_matches = group.matches.len() + group.known_matches.len();
    }

    order
        .into_iter()
        .filter_map(|k| groups.remove(&k))
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "search_test.rs"]
mod tests;
