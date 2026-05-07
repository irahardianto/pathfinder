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
    /// `degraded: true` is set when a file uses an unsupported language (no Tree-sitter grammar).
    /// When degraded, `filter_mode` is **bypassed** (all matches returned) to prevent silent
    /// result-loss. The `degraded_reason` is set to `"unsupported_language_filter_bypassed"` in
    /// this case so agents know filtering was not applied.
    ///
    /// **E4 features:**
    /// - `exclude_glob` is passed to the scout to exclude files before search.
    /// - `known_files` suppresses verbose context for files the agent already has.
    /// - `group_by_file` clusters results into `file_groups` in the response.
    // Orchestrates Ripgrep (raw search) and Tree-sitter (AST enrichment), with
    // degraded-mode bypass logic, response formatting for both grouped and flat output,
    // and detailed telemetry logging. The linear structure makes the orchestration
    // easier to understand and verify.
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential pipeline: ripgrep → TS enrichment → filter → group → response. Extraction done at helper level; remaining orchestration is linear."
    )]
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

        if params.query.trim().is_empty() {
            return Err(crate::server::helpers::io_error_data(
                "query must not be empty",
            ));
        }

        let search_params = SearchParams {
            workspace_root: self.workspace_root.path().to_path_buf(),
            query: params.query.clone(),
            is_regex: params.is_regex,
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
                let filter_was_bypassed = degraded && params.filter_mode != FilterMode::All;
                let filtered_matches = if filter_was_bypassed {
                    enriched_matches
                } else {
                    apply_filter_mode(enriched_matches, &node_types, params.filter_mode)
                };

                let degraded_reason = if filter_was_bypassed {
                    Some("unsupported_language_filter_bypassed".to_owned())
                } else if degraded {
                    Some("unsupported_language".to_owned())
                } else {
                    None
                };

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
                tracing::info!(
                    tool = "search_codebase",
                    total_matches = result.total_matches,
                    returned = returned_count,
                    truncated = result.truncated,
                    filter_mode = ?params.filter_mode,
                    filter_bypassed = filter_was_bypassed,
                    ripgrep_ms,
                    tree_sitter_parse_ms,
                    duration_ms,
                    engines_used = ?["ripgrep", "treesitter"],
                    "search_codebase: complete"
                );

                Ok(Json(SearchCodebaseResponse {
                    matches: flat_matches,
                    total_matches: result.total_matches,
                    returned_count,
                    truncated: result.truncated,
                    file_groups,
                    degraded,
                    degraded_reason,
                    next_offset: if result.truncated {
                        #[allow(clippy::cast_possible_truncation)]
                        Some(params.offset + (returned_count as u32))
                    } else {
                        None
                    },
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
    let mut order: Vec<String> = Vec::with_capacity(matches.len());
    let mut groups: HashMap<String, SearchResultGroup> = HashMap::with_capacity(matches.len());

    for m in matches {
        let key = normalize_path(&m.file);

        // First check if it exists using the Entry API directly, but we only want to
        // clone the key if it does NOT exist.
        // Sadly, the standard library `entry` takes `K: Into<K>`, not `&K`, so we
        // must use standard lookup, but we can do it safely.
        if !groups.contains_key(&key) {
            order.push(key.clone());
            groups.insert(
                key.clone(),
                SearchResultGroup {
                    file: m.file.clone(),
                    version_hash: m.version_hash.clone(),
                    total_matches: 0,
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
mod tests {
    use super::*;
    use crate::server::PathfinderServer;
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::WorkspaceRoot;
    use pathfinder_search::RipgrepScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    // ── CG-6: degraded flag for unsupported language ──────────────────────

    #[tokio::test]
    async fn test_search_codebase_degraded_on_unsupported_language() {
        let ws_dir = tempfile::tempdir().unwrap();
        let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create a file with an unsupported extension
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/data.xyz"), "findme content").unwrap();

        // Use real RipgrepScout so it actually searches the filesystem
        let scout = Arc::new(RipgrepScout);
        let surgeon = Arc::new(MockSurgeon::new());
        // Pre-configure surgeon for enrichment calls (1 match expected)
        surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(None));
        surgeon
            .node_type_at_position_results
            .lock()
            .unwrap()
            .push(Ok("code".to_string()));
        let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);

        let server =
            PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

        let params = SearchCodebaseParams {
            query: "findme".to_owned(),
            is_regex: false,
            path_glob: "**/*.xyz".to_owned(),
            exclude_glob: String::default(),
            offset: 0,
            max_results: 10,
            context_lines: 0,
            known_files: vec![],
            group_by_file: false,
            // Use FilterMode::All so no bypass occurs — degraded_reason is "unsupported_language"
            // (not "unsupported_language_filter_bypassed" which happens with CodeOnly/CommentsOnly)
            filter_mode: pathfinder_common::types::FilterMode::All,
        };
        let result = server.search_codebase_impl(params).await;
        let response = result.expect("search should succeed");
        assert!(
            response.0.degraded,
            "should be degraded for unsupported language"
        );
        assert_eq!(
            response.0.degraded_reason.as_deref(),
            Some("unsupported_language"),
            "with FilterMode::All, filter is not bypassed so reason is unsupported_language"
        );
    }

    // ── PATCH-004: group_by_file + known_files regression test ─────────

    #[tokio::test]
    async fn test_search_group_by_file_with_known_files() {
        // Bug scenario: when all matches belong to files in `known_files` with
        // `group_by_file: true`, the original code would:
        // 1. Return total_matches > 0
        // 2. But file_groups would be "empty" because both `matches` and `known_matches`
        //    would be skipped by serde (they use skip_serializing_if = "Vec::is_empty")
        //    and there was no `total_matches` field to indicate matches exist.
        //
        // Fix adds:
        // - `total_matches` field that is always present
        // - Known matches go into `known_matches` array instead of being lost

        let ws_dir = tempfile::tempdir().unwrap();
        let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create a Rust file with two matches
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/main.rs"),
            "fn findme() {}\nfn other() { findme(); }\n",
        )
        .unwrap();

        let scout = Arc::new(RipgrepScout);
        let surgeon = Arc::new(MockSurgeon::new());
        // Two matches expected — pre-configure surgeon for enrichment calls
        surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(None));
        surgeon
            .node_type_at_position_results
            .lock()
            .unwrap()
            .push(Ok("code".to_string()));
        surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(None));
        surgeon
            .node_type_at_position_results
            .lock()
            .unwrap()
            .push(Ok("code".to_string()));
        let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);

        let server =
            PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

        let params = SearchCodebaseParams {
            query: "findme".to_owned(),
            is_regex: false,
            path_glob: "**/*.rs".to_owned(),
            exclude_glob: String::default(),
            offset: 0,
            max_results: 10,
            context_lines: 0,
            // KEY: file is in known_files + group_by_file: true
            known_files: vec!["src/main.rs".to_owned()],
            group_by_file: true,
            filter_mode: pathfinder_common::types::FilterMode::All,
        };

        let result = server.search_codebase_impl(params).await;
        let response = result.expect("search should succeed");

        // 1. total_matches should be positive
        assert_eq!(
            response.0.total_matches, 2,
            "total_matches should reflect actual number of matches"
        );

        // 2. file_groups should NOT be empty (original data-loss bug)
        let groups = response
            .0
            .file_groups
            .expect("should have file_groups when group_by_file=true");
        assert!(
            !groups.is_empty(),
            "file_groups should NOT be empty when matches exist — original bug: total_matches>0 but file_groups empty"
        );

        // 3. total_matches per group should be populated
        assert_eq!(
            groups[0].total_matches, 2,
            "group total_matches should show count even when all matches are known"
        );

        // 4. known_matches should contain the matches (NOT `matches` array)
        //    because the file is in `known_files`
        assert!(
            groups[0].matches.is_empty(),
            "matches array should be empty when all matches belong to known_files"
        );
        assert_eq!(
            groups[0].known_matches.len(),
            2,
            "known_matches should contain the suppressed matches"
        );
        assert!(
            groups[0].known_matches[0].known,
            "known_matches should have known=true flag"
        );
    }

    #[test]
    fn test_apply_filter_mode_code_only() {
        // Arrange
        let matches = vec![
            SearchMatch {
                file: "src/main.rs".to_owned(),
                line: 1,
                column: 1,
                content: "fn main() {}".to_owned(),
                context_before: vec![],
                context_after: vec![],
                version_hash: "hash".to_owned(),
                enclosing_semantic_path: None,
                known: None,
            },
            SearchMatch {
                file: "src/main.rs".to_owned(),
                line: 2,
                column: 1,
                content: "// a comment".to_owned(),
                context_before: vec![],
                context_after: vec![],
                version_hash: "hash".to_owned(),
                enclosing_semantic_path: None,
                known: None,
            },
        ];
        let node_types = vec!["code".to_owned(), "comment".to_owned()];

        // Act
        let filtered = apply_filter_mode(matches, &node_types, FilterMode::CodeOnly);

        // Assert
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].content, "fn main() {}");
    }

    #[test]
    fn test_apply_filter_mode_comments_only() {
        // Arrange
        let matches = vec![
            SearchMatch {
                file: "src/main.rs".to_owned(),
                line: 1,
                column: 1,
                content: "fn main() {}".to_owned(),
                context_before: vec![],
                context_after: vec![],
                version_hash: "hash".to_owned(),
                enclosing_semantic_path: None,
                known: None,
            },
            SearchMatch {
                file: "src/main.rs".to_owned(),
                line: 2,
                column: 1,
                content: "// a comment".to_owned(),
                context_before: vec![],
                context_after: vec![],
                version_hash: "hash".to_owned(),
                enclosing_semantic_path: None,
                known: None,
            },
            SearchMatch {
                file: "src/main.rs".to_owned(),
                line: 3,
                column: 1,
                content: "\"a string\"".to_owned(),
                context_before: vec![],
                context_after: vec![],
                version_hash: "hash".to_owned(),
                enclosing_semantic_path: None,
                known: None,
            },
        ];
        let node_types = vec!["code".to_owned(), "comment".to_owned(), "string".to_owned()];

        // Act
        let filtered = apply_filter_mode(matches, &node_types, FilterMode::CommentsOnly);

        // Assert
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].content, "// a comment");
        assert_eq!(filtered[1].content, "\"a string\"");
    }

    #[test]
    fn test_apply_filter_mode_all() {
        // Arrange
        let matches = vec![
            SearchMatch {
                file: "src/main.rs".to_owned(),
                line: 1,
                column: 1,
                content: "fn main() {}".to_owned(),
                context_before: vec![],
                context_after: vec![],
                version_hash: "hash".to_owned(),
                enclosing_semantic_path: None,
                known: None,
            },
            SearchMatch {
                file: "src/main.rs".to_owned(),
                line: 2,
                column: 1,
                content: "// a comment".to_owned(),
                context_before: vec![],
                context_after: vec![],
                version_hash: "hash".to_owned(),
                enclosing_semantic_path: None,
                known: None,
            },
        ];
        let node_types = vec!["code".to_owned(), "comment".to_owned()];

        // Act
        let filtered = apply_filter_mode(matches.clone(), &node_types, FilterMode::All);

        // Assert
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].content, matches[0].content);
        assert_eq!(filtered[1].content, matches[1].content);
    }

    #[tokio::test]
    async fn test_search_degraded_filter_bypassed_returns_matches() {
        let ws_dir = tempfile::tempdir().unwrap();
        let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // File with unsupported extension — Tree-sitter can't classify nodes
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(ws_dir.path().join("src/data.xyz"), "// TODO: fix this hack").unwrap();

        let scout = Arc::new(RipgrepScout);
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(None));
        surgeon
            .node_type_at_position_results
            .lock()
            .unwrap()
            .push(Ok("code".to_string())); // degraded: defaults to code
        let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);

        let server =
            PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

        // Request comments_only on an unsupported language — without the fix this returns 0 matches
        let params = SearchCodebaseParams {
            query: "TODO".to_owned(),
            is_regex: false,
            path_glob: "**/*.xyz".to_owned(),
            exclude_glob: String::default(),
            offset: 0,
            max_results: 10,
            context_lines: 0,
            known_files: vec![],
            group_by_file: false,
            filter_mode: pathfinder_common::types::FilterMode::CommentsOnly,
        };
        let result = server.search_codebase_impl(params).await;
        let response = result.expect("search should succeed");

        // The match should be returned despite filter_mode = CommentsOnly
        assert!(
            !response.0.matches.is_empty(),
            "matches must not be empty when filter is bypassed"
        );
        assert!(response.0.degraded, "degraded must be true");
        assert_eq!(
            response.0.degraded_reason.as_deref(),
            Some("unsupported_language_filter_bypassed"),
            "degraded_reason must indicate filter was bypassed"
        );
    }
}
