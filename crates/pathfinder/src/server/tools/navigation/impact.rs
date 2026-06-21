//! `trace` tool handler (callers/callees mode).
//!
//! LSP-powered call-hierarchy BFS with grep-based fallback when no language
//! server is available. Tool responses include `"degraded": true` and
//! `"degraded_reason"` fields to signal the fallback mode to agents.

use crate::server::helpers::{
    format_degraded_notice, millis_to_u64, parse_semantic_path, pathfinder_to_error_data,
    require_symbol_target, serialize_metadata,
};
use crate::server::types::TraceParams;
use crate::server::PathfinderServer;
use pathfinder_common::types::DegradedReason;
use pathfinder_lsp::LspError;
use rmcp::model::{CallToolResult, ErrorData};

/// Wall-clock timeout for BFS traversal in `find_callers_callees`.
/// Prevents infinite loops if the LSP keeps returning more references.
const BFS_TIMEOUT_SECS: u64 = 30;

/// Maximum consecutive LSP failures before aborting BFS traversal.
/// When the LSP is non-responsive, this provides a fast exit path
/// without waiting for the full wall-clock timeout on each step.
/// A responsive LSP may occasionally fail once (e.g., transient error),
/// but 2 consecutive failures strongly indicate a hung/stuck LSP.
const BFS_CONSECUTIVE_FAILURE_LIMIT: u32 = 2;

/// Direction for call hierarchy BFS traversal in `find_callers_callees`.
///
/// `Incoming` traverses callers (who calls this symbol).
/// `Outgoing` traverses callees (what this symbol calls).
#[derive(Debug)]
enum CallDirection {
    Incoming,
    Outgoing,
}

impl PathfinderServer {
    /// SPEC 001 + SPEC 008: Grep-based reference search fallback for `find_callers_callees`.
    ///
    /// When LSP is unavailable, warming up, or timed out, use this helper to find
    /// symbol references using ripgrep with Tree-sitter enrichment (SPEC 008).
    ///
    /// SPEC 008: Uses `search_codebase_impl` with `filter_mode=CodeOnly` to exclude
    /// matches in comments and string literals.
    ///
    /// Returns `Some(refs)` if references found, `None` if none found.
    /// Updates `files_referenced` with the files containing matches.
    async fn grep_reference_fallback(
        &self,
        symbol_name: &str,
        definition_path: &std::path::Path,
        files_referenced: &mut std::collections::HashSet<String>,
    ) -> Option<Vec<crate::server::types::ImpactReference>> {
        let search_params = crate::server::types::SearchParams {
            query: format!("\\b{}\\b", regex::escape(symbol_name)),
            mode: crate::server::types::SearchMode::Regex,
            path_glob: "**/*".to_string(),
            max_results: 20,
            context_lines: 0,
            known_files: vec![],

            offset: 0,
            kind: None,
            ..Default::default()
        };

        let result = match self.search_codebase_impl(search_params).await {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    tool = "grep_reference_fallback",
                    symbol = %symbol_name,
                    error = %e,
                    "search_codebase_impl failed during grep fallback"
                );
                return None;
            }
        };

        if result.0.matches.is_empty() {
            return None;
        }

        let refs: Vec<crate::server::types::ImpactReference> = result
            .0
            .matches
            .into_iter()
            .filter(|m| {
                let m_path = std::path::Path::new(&m.file);
                super::is_source_file(&m.file) && m_path != definition_path
            })
            .take(10)
            .map(|m| {
                files_referenced.insert(m.file.clone());
                let semantic_path = m
                    .enclosing_semantic_path
                    .clone()
                    .unwrap_or_else(|| format!("{}::{symbol_name}", m.file));
                crate::server::types::ImpactReference {
                    semantic_path,
                    file: m.file,
                    line: usize::try_from(m.line).unwrap_or(usize::MAX),
                    snippet: m.content,
                    direction: "incoming_heuristic".to_string(),
                    depth: 0,
                    confidence: Some("heuristic".to_owned()),
                }
            })
            .collect();

        if refs.is_empty() {
            None
        } else {
            Some(refs)
        }
    }

    /// DELIVERABLE F: Grep-based outgoing dependency discovery for `find_callers_callees`.
    ///
    /// When LSP is unavailable, extract call candidates from the symbol's source code
    /// and resolve each candidate to its definition using grep search.
    ///
    /// Returns `Some(refs)` if outgoing dependencies found, `None` if none found.
    /// Updates `files_referenced` with the files containing matches.
    // This function is ~102 lines because the search loop is one cohesive unit.
    // Splitting it into helpers would require passing many local state variables
    // without a meaningful reduction in complexity.
    #[allow(clippy::too_many_lines)]
    async fn grep_outgoing_fallback(
        &self,
        scope_content: &str,
        scope_language: &str,
        definition_path: &std::path::Path,
        max_results: u32,
        project_only: bool,
        files_referenced: &mut std::collections::HashSet<String>,
    ) -> Option<Vec<crate::server::types::ImpactReference>> {
        let candidates = super::extract_call_candidates(scope_content, scope_language);

        if candidates.is_empty() {
            tracing::info!(
                tool = "grep_outgoing_fallback",
                language = %scope_language,
                "no call candidates found in symbol body"
            );
            return None;
        }

        tracing::info!(
            tool = "grep_outgoing_fallback",
            candidate_count = candidates.len(),
            language = %scope_language,
            "resolving {} outgoing candidates",
            candidates.len()
        );

        let max_deps = usize::try_from(max_results).unwrap_or(usize::MAX);
        let mut refs = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for candidate in candidates {
            if refs.len() >= max_deps {
                break;
            }

            let pattern = super::candidate_definition_pattern(scope_language, &candidate);
            let path_glob = super::language_to_file_glob(scope_language);

            let result = self
                .scout
                .search(&pathfinder_search::SearchParams {
                    workspace_root: self.workspace_root.path().to_path_buf(),
                    query: pattern,
                    is_regex: true,
                    max_results: 4,
                    path_glob: path_glob.to_string(),
                    exclude_glob: Vec::new(),
                    context_lines: 0,
                    offset: 0,
                })
                .await;

            match result {
                Ok(search_result) => {
                    let mut found = false;
                    for m in &search_result.matches {
                        if found {
                            break;
                        }
                        if project_only
                            && (!super::is_source_file(&m.file)
                                || !super::is_workspace_file(&m.file))
                        {
                            continue;
                        }

                        let m_path = std::path::Path::new(&m.file);
                        if m_path == definition_path {
                            continue;
                        }

                        // Enrich the grep-resolved candidate name to a qualified treesitter path.
                        // Falls back to `file::candidate` when Surgeon returns None or errors.
                        let semantic_path = self
                            .enrich_semantic_path(
                                &m.file,
                                u32::try_from(m.line).unwrap_or(0),
                                &candidate,
                            )
                            .await;

                        if seen.contains(&semantic_path) {
                            continue;
                        }
                        seen.insert(semantic_path.clone());

                        files_referenced.insert(m.file.clone());

                        refs.push(crate::server::types::ImpactReference {
                            semantic_path,
                            file: m.file.clone(),
                            line: usize::try_from(m.line).unwrap_or(usize::MAX),
                            snippet: m.content.clone(),
                            direction: "outgoing_heuristic".to_string(),
                            depth: 0,
                            confidence: Some("heuristic".to_owned()),
                        });
                        found = true;
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        tool = "grep_outgoing_fallback",
                        candidate = %candidate,
                        error = %e,
                        "search failed for candidate"
                    );
                }
            }
        }

        if refs.is_empty() {
            None
        } else {
            tracing::info!(
                tool = "grep_outgoing_fallback",
                resolved_count = refs.len(),
                "resolved {} outgoing dependencies",
                refs.len()
            );
            Some(refs)
        }
    }

    /// Runs sequential grep reference fallback and grep outgoing fallback.
    #[allow(clippy::too_many_arguments)]
    async fn run_grep_fallbacks(
        &self,
        symbol_name: &str,
        definition_path: &std::path::Path,
        scope_content: &str,
        scope_language: &str,
        remaining_outgoing: u32,
        project_only: bool,
        files_referenced: &mut std::collections::HashSet<String>,
    ) -> (
        Option<Vec<crate::server::types::ImpactReference>>,
        Option<Vec<crate::server::types::ImpactReference>>,
    ) {
        let incoming = self
            .grep_reference_fallback(symbol_name, definition_path, files_referenced)
            .await;

        let outgoing = self
            .grep_outgoing_fallback(
                scope_content,
                scope_language,
                definition_path,
                remaining_outgoing,
                project_only,
                files_referenced,
            )
            .await;

        (incoming, outgoing)
    }

    /// Helper to process grep fallback results, log matches, and update degraded reasons.
    fn process_grep_fallback_results(
        grep_in: Option<Vec<crate::server::types::ImpactReference>>,
        grep_out: Option<Vec<crate::server::types::ImpactReference>>,
        incoming: &mut Option<Vec<crate::server::types::ImpactReference>>,
        outgoing: &mut Option<Vec<crate::server::types::ImpactReference>>,
        degraded_reason: &mut Option<DegradedReason>,
        fallback_reason: Option<DegradedReason>,
        log_suffix: &str,
    ) {
        let mut grep_fallback_found = false;
        if let Some(refs) = grep_in {
            let count = refs.len();
            *incoming = Some(refs);
            grep_fallback_found = true;
            tracing::info!(
                tool = "find_callers_callees",
                references_found = count,
                "find_callers_callees: grep-based fallback references found{}",
                log_suffix
            );
        }
        if let Some(refs) = grep_out {
            let count = refs.len();
            *outgoing = Some(refs);
            grep_fallback_found = true;
            tracing::info!(
                tool = "find_callers_callees",
                outgoing_found = count,
                "find_callers_callees: grep-based outgoing deps found{}",
                log_suffix
            );
        }
        if grep_fallback_found {
            if let Some(reason) = fallback_reason {
                *degraded_reason = Some(reason);
            }
        }
    }

    /// Performs BFS traversal of the call hierarchy in the specified direction.
    ///
    /// Added wall-clock timeout to prevent infinite loops when LSP keeps returning references.
    ///
    /// Returns the collected references and the maximum depth reached during traversal.
    #[allow(clippy::too_many_lines)]
    async fn bfs_call_hierarchy(
        &self,
        initial_item: &pathfinder_lsp::types::CallHierarchyItem,
        direction: CallDirection,
        max_depth: u32,
        files_referenced: &mut std::collections::HashSet<String>,
        project_only: bool,
        remaining_references: &mut u32,
    ) -> (Vec<crate::server::types::ImpactReference>, u32) {
        let timeout = tokio::time::Duration::from_secs(BFS_TIMEOUT_SECS);
        let deadline = tokio::time::Instant::now() + timeout;

        let mut queue = std::collections::VecDeque::new();
        queue.push_back((initial_item.clone(), 0));
        let mut seen = std::collections::HashSet::new();
        seen.insert((
            initial_item.file.clone(),
            initial_item.line,
            initial_item.name.clone(),
        ));
        files_referenced.insert(initial_item.file.clone());

        let mut references = Vec::new();
        let mut max_depth_reached = 0;
        let mut consecutive_failures: u32 = 0;

        while let Some((item, current_depth)) = queue.pop_front() {
            max_depth_reached = std::cmp::max(max_depth_reached, current_depth);
            if current_depth >= max_depth {
                continue;
            }
            if *remaining_references == 0 {
                break;
            }

            // Check wall-clock timeout
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    direction = ?direction,
                    timeout_secs = BFS_TIMEOUT_SECS,
                    "BFS traversal exceeded wall-clock timeout, returning partial results"
                );
                break;
            }

            // Check consecutive failure limit — fast exit when LSP is hung
            if consecutive_failures >= BFS_CONSECUTIVE_FAILURE_LIMIT {
                tracing::warn!(
                    direction = ?direction,
                    consecutive_failures,
                    limit = BFS_CONSECUTIVE_FAILURE_LIMIT,
                    "BFS aborted: too many consecutive LSP failures, returning partial results"
                );
                break;
            }

            let hierarchy_result = match direction {
                CallDirection::Incoming => {
                    self.lawyer
                        .call_hierarchy_incoming(self.workspace_root.path(), &item)
                        .await
                }
                CallDirection::Outgoing => {
                    self.lawyer
                        .call_hierarchy_outgoing(self.workspace_root.path(), &item)
                        .await
                }
            };

            match hierarchy_result {
                Ok(calls) => {
                    consecutive_failures = 0;
                    for call in calls {
                        if *remaining_references == 0 {
                            break;
                        }

                        let referenced_item = call.item;

                        // Filter out non-workspace files when project_only:
                        // - Must have a source code extension
                        // - Must be a relative path (not absolute like stdlib/SDK paths)
                        // - Must not be in node_modules/ or vendor/
                        if project_only
                            && (!super::is_source_file(&referenced_item.file)
                                || !super::is_workspace_file(&referenced_item.file))
                        {
                            continue;
                        }

                        files_referenced.insert(referenced_item.file.clone());

                        let key = (
                            referenced_item.file.clone(),
                            referenced_item.line,
                            referenced_item.name.clone(),
                        );
                        if seen.insert(key) {
                            queue.push_back((referenced_item.clone(), current_depth + 1));

                            // Enrich the flat LSP name to a qualified treesitter path.
                            // Falls back to `file::flat_name` when Surgeon returns None or errors.
                            let semantic_path = self
                                .enrich_semantic_path(
                                    &referenced_item.file,
                                    referenced_item.line,
                                    &referenced_item.name,
                                )
                                .await;

                            references.push(crate::server::types::ImpactReference {
                                semantic_path,
                                file: referenced_item.file.clone(),
                                line: usize::try_from(referenced_item.line).unwrap_or(usize::MAX),
                                snippet: referenced_item
                                    .detail
                                    .unwrap_or_else(|| referenced_item.name.clone()),
                                direction: match direction {
                                    CallDirection::Incoming => "incoming".to_owned(),
                                    CallDirection::Outgoing => "outgoing".to_owned(),
                                },
                                depth: usize::try_from(current_depth).unwrap_or(0),
                                confidence: Some("lsp".to_owned()),
                            });
                            *remaining_references -= 1;
                        }
                    }
                }
                Err(e) => {
                    consecutive_failures += 1;
                    let direction_name = match direction {
                        CallDirection::Incoming => "call_hierarchy_incoming",
                        CallDirection::Outgoing => "call_hierarchy_outgoing",
                    };
                    tracing::warn!(
                        tool = "find_callers_callees",
                        error = %e,
                        file = %item.file,
                        line = item.line,
                        depth = current_depth,
                        "{direction_name} failed during BFS (partial impact graph)"
                    );
                }
            }
        }

        (references, max_depth_reached)
    }

    /// Core logic for the `find_callers_callees` tool.
    ///
    /// Returns callers (incoming) and callees (outgoing) for the target symbol.
    /// Degrades gracefully to empty results when no LSP is configured.
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential pipeline (parse→sandbox→tree-sitter→LSP→BFS→version hash)."
    )]
    pub(crate) async fn find_callers_callees_impl(
        &self,
        params: TraceParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

        // Cap max_depth to prevent unbounded BFS traversal (PRD §5.1 maximum).
        // Also floor at 1 to guarantee at least one level of traversal.
        let max_depth = params.max_depth.clamp(1, 5);
        let project_only = true;
        // Clamp max_references to [1, 500] to prevent unbounded memory growth and LSP calls.
        // Floor at 1 prevents silently empty results; ceiling at 500 bounds BFS traversal.
        let max_references = params.max_references.clamp(1, 500);
        // Split budget between incoming and outgoing. Give any odd slot to incoming.
        let half = max_references / 2;
        let mut remaining_incoming = half + max_references % 2;
        let mut remaining_outgoing = half;

        tracing::info!(
            tool = "find_callers_callees",
            semantic_path = %params.semantic_path,
            max_depth = max_depth,
            "find_callers_callees: start"
        );

        // Parse and validate the semantic path
        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;
        let symbol_name = super::last_symbol_name(&semantic_path).unwrap_or_default();

        // Sandbox check
        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "find_callers_callees",
                error_code = e.error_code(),
                duration_ms,
                "sandbox check failed"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // Early file existence check — avoid tree-sitter parse on nonexistent files
        let abs_file = self.workspace_root.path().join(&semantic_path.file_path);
        if !abs_file.exists() {
            let err = pathfinder_common::error::PathfinderError::FileNotFound {
                path: abs_file.clone(),
            };
            tracing::warn!(
                tool = "find_callers_callees",
                path = %abs_file.display(),
                "file not found"
            );
            return Err(pathfinder_to_error_data(&err));
        }

        // 1. Fetch the symbol scope (Tree-sitter) to get start line
        let ts_start = std::time::Instant::now();
        let scope = match self
            .read_symbol_scope_enriched(&semantic_path, &params.semantic_path)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "find_callers_callees",
                    error = %e,
                    duration_ms,
                    "tree-sitter read failed"
                );
                return Err(e);
            }
        };
        let tree_sitter_ms = ts_start.elapsed().as_millis();

        // IW-3 (DS-1 gap fix): RAII document lifecycle — did_close fires on all exits.
        let file_path = self.workspace_root.path().join(&semantic_path.file_path);
        let file_content = match tokio::fs::read_to_string(&file_path).await {
            Ok(content) => content,
            Err(e) => {
                tracing::warn!(
                    tool = "find_callers_callees",
                    path = %file_path.display(),
                    error = %e,
                    "file read failed — LSP will receive empty content"
                );
                String::new()
            }
        };
        // `_doc_guard` fires did_close automatically when this function returns.
        let _doc_guard = match self
            .lawyer
            .open_document(
                self.workspace_root.path(),
                &semantic_path.file_path,
                &file_content,
            )
            .await
        {
            Ok(guard) => Some(guard),
            Err(e) => {
                tracing::warn!(
                    tool = "find_callers_callees",
                    semantic_path = %semantic_path,
                    error = %e,
                    "open_document failed — LSP queries may return degraded results"
                );
                None
            }
        };

        let lsp_start = std::time::Instant::now();
        // Use Option<Vec> to distinguish "unknown" (LSP unavailable) from "verified empty" (LSP confirmed zero).
        // None = degraded (LSP was down — callers are unknown, do NOT treat as zero)
        // Some([]) = LSP responded with confirmed zero callers/callees
        let mut incoming: Option<Vec<crate::server::types::ImpactReference>> = None;
        let mut outgoing: Option<Vec<crate::server::types::ImpactReference>> = None;
        let mut degraded = true;
        let mut degraded_reason = Some(DegradedReason::NoLsp);
        let mut engines = vec!["tree-sitter"];
        let mut files_referenced = std::collections::HashSet::new();
        let mut max_depth_reached = 0;

        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(scope.start_line + 1).unwrap_or(1),
                // Position cursor on the symbol's name identifier (e.g., the 'd' in 'dedent'),
                // not the 'pub' keyword. rust-analyzer requires this for symbol resolution.
                u32::try_from(scope.name_column + 1).unwrap_or(1),
            )
            .await;

        match lsp_result {
            Ok(items) if !items.is_empty() => {
                engines.push("lsp");
                degraded = false;
                degraded_reason = None;

                let initial_item = &items[0];
                files_referenced.insert(initial_item.file.clone());

                // --- INCOMING BFS ---
                let (incoming_refs, depth_in) = self
                    .bfs_call_hierarchy(
                        initial_item,
                        CallDirection::Incoming,
                        max_depth,
                        &mut files_referenced,
                        project_only,
                        &mut remaining_incoming,
                    )
                    .await;
                incoming = Some(incoming_refs);
                max_depth_reached = std::cmp::max(max_depth_reached, depth_in);

                // --- OUTGOING BFS ---
                let (outgoing_refs, depth_out) = self
                    .bfs_call_hierarchy(
                        initial_item,
                        CallDirection::Outgoing,
                        max_depth,
                        &mut files_referenced,
                        project_only,
                        &mut remaining_outgoing,
                    )
                    .await;
                outgoing = Some(outgoing_refs);
                max_depth_reached = std::cmp::max(max_depth_reached, depth_out);

                // Check for false negatives when BFS call hierarchy traversal returns 0 callers/callees
                if incoming.as_ref().is_none_or(Vec::is_empty)
                    && outgoing.as_ref().is_none_or(Vec::is_empty)
                {
                    let (grep_in, grep_out) = self
                        .run_grep_fallbacks(
                            &symbol_name,
                            &semantic_path.file_path,
                            &scope.content,
                            &scope.language,
                            remaining_outgoing,
                            project_only,
                            &mut files_referenced,
                        )
                        .await;

                    degraded = true;
                    degraded_reason = Some(DegradedReason::LspWarmupGrepFallback);
                    if grep_in.is_some() {
                        incoming = grep_in;
                    }
                    if grep_out.is_some() {
                        outgoing = grep_out;
                    }
                }
            }
            Ok(_) => {
                // LSP responded with empty items — but this is ambiguous:
                //   - Genuine "zero callers": LSP is warm and the symbol truly has no references.
                //   - LSP warmup: LSP hasn't finished indexing and returned [] for everything.
                //
                // Probe goto_definition at the same position. A warm LSP can resolve a symbol
                // to its definition; a cold LSP returns None even for well-known symbols.
                // If the probe returns Ok(Some(_)) the LSP is warm → confirmed zero callers.
                // If the probe returns Ok(None) or Err, we degrade rather than lying to the agent.
                let probe = self
                    .lawyer
                    .goto_definition(
                        self.workspace_root.path(),
                        &semantic_path.file_path,
                        u32::try_from(scope.start_line + 1).unwrap_or(1),
                        u32::try_from(scope.name_column + 1).unwrap_or(1),
                    )
                    .await;

                if matches!(probe, Ok(Some(_))) {
                    // LSP is warm — definition resolved. But let's check for false negatives (indexing incomplete, etc.)
                    // by running grep fallback.
                    let (grep_in, grep_out) = self
                        .run_grep_fallbacks(
                            &symbol_name,
                            &semantic_path.file_path,
                            &scope.content,
                            &scope.language,
                            remaining_outgoing,
                            project_only,
                            &mut files_referenced,
                        )
                        .await;

                    engines.push("lsp");
                    degraded = true;
                    degraded_reason = Some(DegradedReason::LspWarmupGrepFallback);
                    incoming = grep_in.or(Some(Vec::new()));
                    outgoing = grep_out.or(Some(Vec::new()));
                } else {
                    // LSP likely still warming up — empty call hierarchy is not reliable.
                    // Degrade so agents know to verify before acting on "zero references".
                    tracing::info!(
                        tool = "find_callers_callees",
                        symbol = %semantic_path,
                        "find_callers_callees: call_hierarchy_prepare returned [] but goto_definition \
                         probe returned no result — LSP likely warming up, attempting grep-based reference fallback"
                    );
                    engines.push("lsp");
                    degraded = true;
                    degraded_reason = Some(DegradedReason::LspWarmupEmptyUnverified);

                    let (grep_in, grep_out) = self
                        .run_grep_fallbacks(
                            &symbol_name,
                            &semantic_path.file_path,
                            &scope.content,
                            &scope.language,
                            remaining_outgoing,
                            project_only,
                            &mut files_referenced,
                        )
                        .await;

                    Self::process_grep_fallback_results(
                        grep_in,
                        grep_out,
                        &mut incoming,
                        &mut outgoing,
                        &mut degraded_reason,
                        Some(DegradedReason::LspWarmupGrepFallback),
                        " during LSP warmup",
                    );
                }
            }
            Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }) => {
                // Degraded mode — LSP not available. Use grep-based reference search
                // as a heuristic fallback. Results may over-count (string references)
                // or under-count (indirect calls), but give the agent a starting point.
                tracing::info!(
                    tool = "find_callers_callees",
                    symbol = %semantic_path,
                    "find_callers_callees: no LSP — attempting grep-based reference fallback"
                );

                let (grep_in, grep_out) = self
                    .run_grep_fallbacks(
                        &symbol_name,
                        &semantic_path.file_path,
                        &scope.content,
                        &scope.language,
                        remaining_outgoing,
                        project_only,
                        &mut files_referenced,
                    )
                    .await;

                Self::process_grep_fallback_results(
                    grep_in,
                    grep_out,
                    &mut incoming,
                    &mut outgoing,
                    &mut degraded_reason,
                    Some(DegradedReason::NoLspGrepFallback),
                    "",
                );
            }
            Err(LspError::Timeout { .. }) => {
                // LSP timed out — attempt grep-based reference fallback.
                // Set reason unconditionally: timeout is always the cause, whether or not
                // grep succeeds. Without this, empty grep results would fall through to the
                // initial NoLsp reason, misleading agents into thinking no LSP exists.
                degraded_reason = Some(DegradedReason::LspTimeoutGrepFallback);

                tracing::info!(
                    tool = "find_callers_callees",
                    symbol = %semantic_path,
                    "find_callers_callees: LSP timed out — attempting grep-based reference fallback"
                );

                let (grep_in, grep_out) = self
                    .run_grep_fallbacks(
                        &symbol_name,
                        &semantic_path.file_path,
                        &scope.content,
                        &scope.language,
                        remaining_outgoing,
                        project_only,
                        &mut files_referenced,
                    )
                    .await;

                Self::process_grep_fallback_results(
                    grep_in,
                    grep_out,
                    &mut incoming,
                    &mut outgoing,
                    &mut degraded_reason,
                    None,
                    " after timeout",
                );
            }
            Err(e) => {
                // LSP returned an unexpected error — not "no LSP" but an operational failure.
                // Set reason unconditionally: LspErrorGrepFallback describes the cause whether
                // or not grep finds anything. NoLsp would be misleading — the LSP exists but
                // failed, which is a different agent guidance scenario (retry vs. install).
                degraded = true;
                degraded_reason = Some(DegradedReason::LspErrorGrepFallback);

                tracing::warn!(
                    tool = "find_callers_callees",
                    error = %e,
                    "call_hierarchy_prepare failed"
                );

                let (grep_in, grep_out) = self
                    .run_grep_fallbacks(
                        &symbol_name,
                        &semantic_path.file_path,
                        &scope.content,
                        &scope.language,
                        remaining_outgoing,
                        project_only,
                        &mut files_referenced,
                    )
                    .await;

                Self::process_grep_fallback_results(
                    grep_in,
                    grep_out,
                    &mut incoming,
                    &mut outgoing,
                    &mut degraded_reason,
                    None,
                    " after LSP error",
                );
            }
        }

        // Note: `_doc_guard` still alive here; did_close fires at function return.
        let lsp_ms = lsp_start.elapsed().as_millis();
        let duration_ms = start.elapsed().as_millis();

        let inc_count = incoming.as_ref().map_or(0, Vec::len);
        let out_count = outgoing.as_ref().map_or(0, Vec::len);
        let degraded_reason_cloned = degraded_reason;
        let degraded_reason_str = degraded_reason.as_ref().map(ToString::to_string);

        let lsp_readiness = if degraded {
            match degraded_reason_cloned {
                Some(
                    DegradedReason::LspWarmupEmptyUnverified
                    | DegradedReason::LspWarmupGrepFallback
                    | DegradedReason::LspTimeoutGrepFallback,
                ) => Some("warming_up".to_owned()),
                _ => Some("unavailable".to_owned()),
            }
        } else {
            Some("ready".to_owned())
        };
        let warm_start_in_progress = match lsp_readiness.as_deref() {
            Some("warming_up") => Some(true),
            Some("ready") => Some(false),
            _ => None,
        };

        tracing::info!(
            tool = "find_callers_callees",
            semantic_path = %params.semantic_path,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            degraded,
            degraded_reason = ?degraded_reason_str,
            engines_used = ?engines,
            "find_callers_callees: complete"
        );
        // Item 2: Report truncation only when the total budget was actually exhausted,
        // not when a single direction hits its cap. Check total returned vs total budget.
        let total_returned = inc_count + out_count;
        let max_refs_usize = usize::try_from(max_references).unwrap_or(usize::MAX);
        let references_truncated = max_references > 0 && total_returned >= max_refs_usize;

        let resolution_strategy = if !degraded && engines.contains(&"lsp") {
            Some("lsp_call_hierarchy".to_owned())
        } else if degraded {
            // Check which grep fallback was used based on degraded_reason
            match &degraded_reason {
                Some(
                    DegradedReason::LspWarmupGrepFallback
                    | DegradedReason::LspTimeoutGrepFallback
                    | DegradedReason::LspErrorGrepFallback
                    | DegradedReason::NoLspGrepFallback,
                ) => Some("grep_file_scoped".to_owned()),
                _ => Some("treesitter_fallback".to_owned()),
            }
        } else {
            Some("treesitter_direct".to_owned())
        };

        // Spec 4.2: Test coverage search (deactivated, keeping fields None for backwards compatibility)
        let (test_callers, test_coverage_status) = (None, None);

        // P2-7: Generate a hint when zero incoming callers and not degraded.
        // Non-degraded + empty incoming = LSP confirmed zero callers, which could be
        // an entry point, unused code, or dynamic dispatch that LSP can't trace.
        //
        // NOTE: The "both incoming AND outgoing empty" case is always degraded because
        // the grep fallback at lines 570-615 sets `degraded=true` when both BFS results
        // are empty (to guard against false negatives from LSP errors during traversal).
        // Therefore we only hint on "incoming empty, outgoing non-empty" here.
        let hint = if !degraded && incoming.as_ref().is_some_and(Vec::is_empty) {
            Some(
                "LSP confirmed zero incoming callers. This symbol may be an entry point, \
                 unused, or only called via dynamic dispatch/reflection."
                    .to_owned(),
            )
        } else {
            None
        };

        let metadata = crate::server::types::FindCallersCalleesMetadata {
            incoming,
            outgoing,
            depth_reached: max_depth_reached,
            files_referenced: files_referenced.len(),
            degraded,
            degraded_reason,
            actionable_guidance: degraded_reason.as_ref().map(DegradedReason::guidance),
            lsp_readiness,
            warm_start_in_progress,
            references_truncated,
            resolution_strategy,
            test_callers,
            test_coverage_status,
            duration_ms: Some(millis_to_u64(duration_ms)),
            hint,
        };

        // Build honest text output based on actual results listing every
        // reference so agents can act without parsing structured_content.
        let mut text_parts = Vec::new();
        if degraded {
            let notice = degraded_reason_cloned
                .as_ref()
                .map_or_else(|| "DEGRADED (unknown)".to_owned(), format_degraded_notice);

            let symbol_name = symbol_name.clone();

            text_parts.push(notice);
            text_parts.push(String::new());
            text_parts.push("   Common causes:".to_owned());
            text_parts.push("   - Interface types without concrete implementations in source (JPA repositories)".to_owned());
            text_parts.push(
                "   - Annotation-driven dependency injection (Spring proxies at runtime)"
                    .to_owned(),
            );
            text_parts.push("   - LSP still warming up (wait 30s, try again)".to_owned());
            text_parts.push(String::new());
            if symbol_name.is_empty() {
                text_parts.push("   Workaround: Use search to find usages manually.".to_owned());
            } else {
                text_parts.push(format!(
                    "   Workaround: Use search(query=\"{symbol_name}\") to find usages manually."
                ));
            }
            text_parts.push("   Reference counts below are heuristic only:".to_owned());
            text_parts.push(String::new());
        } else if inc_count == 0 && out_count == 0 {
            text_parts.push("LSP confirmed: zero callers/callees for this symbol.".to_string());
        } else if inc_count == 0 {
            text_parts
                .push("LSP confirmed: zero incoming callers (callees found below).".to_string());
        } else if out_count == 0 {
            text_parts
                .push("LSP confirmed: zero outgoing callees (callers found below).".to_string());
        }
        // Incoming
        text_parts.push(format!("Incoming references: {inc_count}"));
        if let Some(refs) = &metadata.incoming {
            for r in refs {
                text_parts.push(format!(
                    "  [depth={}] {} ({}:L{})",
                    r.depth, r.semantic_path, r.file, r.line
                ));
                if !r.snippet.is_empty() {
                    text_parts.push(format!("    > {}", r.snippet.trim()));
                }
            }
        }
        // Outgoing
        text_parts.push(format!("Outgoing references: {out_count}"));
        if let Some(refs) = &metadata.outgoing {
            for r in refs {
                text_parts.push(format!(
                    "  [depth={}] {} ({}:L{})",
                    r.depth, r.semantic_path, r.file, r.line
                ));
                if !r.snippet.is_empty() {
                    text_parts.push(format!("    > {}", r.snippet.trim()));
                }
            }
        }

        // Spec 4.2: Test coverage section
        if let Some(test_refs) = &metadata.test_callers {
            if !test_refs.is_empty() {
                text_parts.push(String::new());
                text_parts.push(format!(
                    "TEST COVERAGE: {} test functions cover this symbol",
                    test_refs.len()
                ));
                for r in test_refs {
                    text_parts.push(format!(
                        "  - {}::{} ({}:L{})",
                        r.file, r.semantic_path, r.file, r.line
                    ));
                }
            }
        } else if let Some(status) = &metadata.test_coverage_status {
            if status == "not_found" {
                text_parts.push(String::new());
                text_parts
                    .push("TEST COVERAGE: no test functions found for this symbol".to_owned());
            } else if status == "unknown_degraded" {
                text_parts.push(String::new());
                text_parts.push("TEST COVERAGE: unknown (search degraded)".to_owned());
            }
        }

        text_parts.push(format!("[completed in {duration_ms}ms]"));
        let text = text_parts.join("\n");
        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "impact_test.rs"]
mod tests;
