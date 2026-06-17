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
            query: symbol_name.to_string(),
            mode: crate::server::types::SearchMode::Text,
            path_glob: "**/*".to_string(),
            max_results: 20,
            context_lines: 0,
            known_files: vec![],
            exclude_glob: String::new(),
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

        let max_deps = max_results as usize;
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
                    exclude_glob: String::default(),
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

                        let semantic_path = format!("{}::{}", m.file, candidate);

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

                            references.push(crate::server::types::ImpactReference {
                                semantic_path: format!(
                                    "{}::{}",
                                    referenced_item.file, referenced_item.name
                                ),
                                file: referenced_item.file.clone(),
                                line: referenced_item.line as usize,
                                snippet: referenced_item
                                    .detail
                                    .unwrap_or_else(|| referenced_item.name.clone()),
                                direction: match direction {
                                    CallDirection::Incoming => "incoming".to_owned(),
                                    CallDirection::Outgoing => "outgoing".to_owned(),
                                },
                                depth: current_depth as usize,
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
        // Clamp max_references to minimum 1 to prevent silently empty results.
        let max_references = params.max_references.max(1);
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
                    let symbol_name = super::last_symbol_name(&semantic_path).unwrap_or_default();
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
                    let symbol_name = super::last_symbol_name(&semantic_path).unwrap_or_default();
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

                    let symbol_name = super::last_symbol_name(&semantic_path).unwrap_or_default();
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

                    let mut grep_fallback_found = false;
                    if let Some(refs) = grep_in {
                        incoming = Some(refs);
                        grep_fallback_found = true;
                        tracing::info!(
                            tool = "find_callers_callees",
                            references_found = incoming.as_ref().map_or(0, Vec::len),
                            "find_callers_callees: grep-based fallback references found during LSP warmup"
                        );
                    }
                    if let Some(refs) = grep_out {
                        outgoing = Some(refs);
                        grep_fallback_found = true;
                        tracing::info!(
                            tool = "find_callers_callees",
                            outgoing_found = outgoing.as_ref().map_or(0, Vec::len),
                            "find_callers_callees: grep-based outgoing deps found during LSP warmup"
                        );
                    }
                    if grep_fallback_found {
                        degraded_reason = Some(DegradedReason::LspWarmupGrepFallback);
                    }
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

                let symbol_name = super::last_symbol_name(&semantic_path).unwrap_or_default();
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

                let mut grep_fallback_found = false;
                if let Some(refs) = grep_in {
                    incoming = Some(refs);
                    grep_fallback_found = true;
                    tracing::info!(
                        tool = "find_callers_callees",
                        references_found = incoming.as_ref().map_or(0, Vec::len),
                        "find_callers_callees: grep-based fallback references found"
                    );
                }
                if let Some(refs) = grep_out {
                    outgoing = Some(refs);
                    grep_fallback_found = true;
                    tracing::info!(
                        tool = "find_callers_callees",
                        outgoing_found = outgoing.as_ref().map_or(0, Vec::len),
                        "find_callers_callees: grep-based outgoing deps found"
                    );
                }
                if grep_fallback_found {
                    degraded_reason = Some(DegradedReason::NoLspGrepFallback);
                }
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

                let symbol_name = super::last_symbol_name(&semantic_path).unwrap_or_default();
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

                if let Some(refs) = grep_in {
                    incoming = Some(refs);
                    tracing::info!(
                        tool = "find_callers_callees",
                        references_found = incoming.as_ref().map_or(0, Vec::len),
                        "find_callers_callees: grep-based fallback references found after timeout"
                    );
                }
                if let Some(refs) = grep_out {
                    outgoing = Some(refs);
                    tracing::info!(
                        tool = "find_callers_callees",
                        outgoing_found = outgoing.as_ref().map_or(0, Vec::len),
                        "find_callers_callees: grep-based outgoing deps found after timeout"
                    );
                }
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

                let symbol_name = super::last_symbol_name(&semantic_path).unwrap_or_default();
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

                if let Some(refs) = grep_in {
                    incoming = Some(refs);
                    tracing::info!(
                        tool = "find_callers_callees",
                        references_found = incoming.as_ref().map_or(0, Vec::len),
                        "find_callers_callees: grep-based fallback references found after LSP error"
                    );
                }
                if let Some(refs) = grep_out {
                    outgoing = Some(refs);
                    tracing::info!(
                        tool = "find_callers_callees",
                        outgoing_found = outgoing.as_ref().map_or(0, Vec::len),
                        "find_callers_callees: grep-based outgoing deps found after LSP error"
                    );
                }
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

            let symbol_name = super::last_symbol_name(&semantic_path).unwrap_or_default();

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
mod tests {
    use super::super::test_helpers::{make_scope, make_server_with_lawyer, make_temp_workspace};
    use super::*;
    use crate::server::types::TraceParams;
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::{DegradedReason, WorkspaceRoot};
    use pathfinder_lsp::types::{CallHierarchyCall, CallHierarchyItem};
    use pathfinder_lsp::{DefinitionLocation, MockLawyer};
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    // ── find_callers_callees ────────────────────────────────────────────

    #[tokio::test]
    async fn test_find_callers_callees_returns_empty_degraded() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            surgeon,
            lawyer,
        );

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(
            val.incoming.is_none(),
            "incoming must be null (not empty) when degraded"
        );
        assert!(
            val.outgoing.is_none(),
            "outgoing must be null (not empty) when degraded"
        );
        assert!(val.degraded);
        assert_eq!(val.degraded_reason, Some(DegradedReason::NoLsp));
    }

    #[tokio::test]
    async fn test_find_callers_callees_lsp_populates_incoming_and_outgoing() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));

        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "handle_request".into(),
                kind: "function".into(),
                detail: Some("fn handle_request()".into()),
                file: "src/server.rs".into(),
                line: 20,
                column: 4,
                data: None,
            },
            call_sites: vec![25],
        }]));

        lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "validate_token".into(),
                kind: "function".into(),
                detail: Some("fn validate_token() -> bool".into()),
                file: "src/token.rs".into(),
                line: 15,
                column: 4,
                data: None,
            },
            call_sites: vec![9],
        }]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        assert_eq!(val.degraded_reason, None);
        assert_eq!(val.depth_reached, 1); // BFS pops level 1, updates max_depth_reached, then continues
        assert_eq!(val.files_referenced, 3); // initial + caller + callee
        let incoming = val
            .incoming
            .as_ref()
            .expect("incoming must be Some when not degraded");
        let outgoing = val
            .outgoing
            .as_ref()
            .expect("outgoing must be Some when not degraded");
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].file, "src/server.rs");
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].file, "src/token.rs");
    }

    // ── find_callers_callees with empty hierarchy (confirmed zero callers) ───────

    #[tokio::test]
    async fn test_find_callers_callees_empty_hierarchy_confirmed_zero() {
        // call_hierarchy_prepare returns Ok([]) AND goto_definition probe returns Ok(Some(...))
        // → LSP is warm, confirmed zero callers. Must NOT be degraded.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Empty call hierarchy — ambiguous on its own
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));
        // Probe: goto_definition succeeds → LSP is warm → confirmed zero
        lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
            file: "src/auth.rs".into(),
            line: 10,
            column: 4,
            preview: "fn login() {}".into(),
        })));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // DEGRADED — LSP warm but call hierarchy empty
        assert!(
            val.degraded,
            "must be degraded when call hierarchy is empty"
        );
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspWarmupGrepFallback)
        );
        let incoming = val.incoming.as_ref().expect("must be Some when degraded");
        let outgoing = val.outgoing.as_ref().expect("must be Some when degraded");
        assert!(incoming.is_empty(), "confirmed zero callers");
        assert!(outgoing.is_empty(), "confirmed zero callees");
    }

    #[tokio::test]
    async fn test_find_callers_callees_empty_hierarchy_warmup_degrades() {
        // call_hierarchy_prepare returns Ok([]) AND goto_definition probe returns Ok(None)
        // → LSP is warming up. Must be degraded with "lsp_warmup_empty_unverified".
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Empty call hierarchy
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));
        // Probe: goto_definition returns Ok(None) → LSP is still warming up
        // MockLawyer::default() already returns Ok(None) for goto_definition, so no extra setup needed.

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // DEGRADED — LSP warmup detected
        assert!(
            val.degraded,
            "must be degraded when goto_definition probe also returns None"
        );
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspWarmupEmptyUnverified),
            "degraded_reason must indicate warmup ambiguity"
        );
        // incoming/outgoing must be None — do NOT mislead agent with Some([])
        assert!(
            val.incoming.is_none(),
            "incoming must be None (unknown) during warmup, not Some([]) (confirmed-zero)"
        );
        assert!(
            val.outgoing.is_none(),
            "outgoing must be None (unknown) during warmup, not Some([]) (confirmed-zero)"
        );
    }

    // ── find_callers_callees with LSP error on call_hierarchy_prepare ────────────

    #[tokio::test]
    async fn test_find_callers_callees_lsp_error_degrades() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Simulate LSP protocol error
        lawyer
            .push_prepare_call_hierarchy_result(Err(LspError::Protocol("LSP crashed".to_string())));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Degraded due to LSP error — must report LspErrorGrepFallback, not NoLsp.
        // NoLsp would mislead agents into "install LSP" when the real cause is a transient error.
        assert!(val.degraded);
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspErrorGrepFallback)
        );
    }

    // ── find_callers_callees BFS depth limiting ────────────────────────────────

    #[tokio::test]
    async fn test_find_callers_callees_bfs_respects_max_depth() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));

        // Incoming: one caller that itself has a caller (depth 2 chain)
        let caller_item = CallHierarchyItem {
            name: "caller".into(),
            kind: "function".into(),
            detail: None,
            file: "src/caller.rs".into(),
            line: 5,
            column: 4,
            data: None,
        };
        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: caller_item.clone(),
            call_sites: vec![9],
        }]));
        // Second level incoming (would only be reached if max_depth > 1)
        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "top_level".into(),
                kind: "function".into(),
                detail: None,
                file: "src/main.rs".into(),
                line: 1,
                column: 0,
                data: None,
            },
            call_sites: vec![5],
        }]));

        // Outgoing: empty
        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1, // Should stop after first level
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        let _incoming = val.incoming.as_ref().expect("must be Some");
        // With max_depth=1, BFS processes the initial item at depth 0, finds caller at depth 1,
        // but the second-level caller (depth 2) should NOT be included
        // However depth_reached should be 1
        assert_eq!(val.depth_reached, 1);
    }

    // ── CG-3: sandbox check error in find_callers_callees ──────────────────────

    #[tokio::test]
    async fn test_find_callers_callees_rejects_sandbox_denied_path() {
        let surgeon = Arc::new(MockSurgeon::new());
        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: ".git/objects/abc::def".to_owned(),
            max_depth: 2,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let Err(err) = result else {
            panic!("expected error but got Ok");
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "ACCESS_DENIED");
    }

    // ── CG-4: Tree-sitter error in find_callers_callees ──────────────────────────

    #[tokio::test]
    async fn test_find_callers_callees_tree_sitter_error() {
        let surgeon = Arc::new(MockSurgeon::new());
        // Push an error result
        surgeon.read_symbol_scope_results.lock().unwrap().push(Err(
            pathfinder_treesitter::SurgeonError::ParseError {
                path: std::path::PathBuf::from("src/auth.rs"),
                reason: "parse failed".to_string(),
            },
        ));

        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        assert!(result.is_err(), "tree-sitter error should propagate");
    }

    // ── CG-5: LSP error during BFS traversal ───────────────────────────────

    #[tokio::test]
    async fn test_find_callers_callees_bfs_lsp_error_graceful_partial_graph() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
        // Incoming succeeds with one caller
        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "caller".into(),
                kind: "function".into(),
                detail: None,
                file: "src/server.rs".into(),
                line: 20,
                column: 4,
                data: None,
            },
            call_sites: vec![9],
        }]));
        // Outgoing fails with LSP error
        lawyer.push_outgoing_call_result(Err(LspError::Protocol(
            "LSP crashed during outgoing".to_string(),
        )));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed despite partial failure");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // NOT degraded — prepare succeeded, incoming succeeded, only outgoing had error
        assert!(!val.degraded);
        let incoming = val.incoming.as_ref().expect("incoming must be Some");
        assert_eq!(incoming.len(), 1, "incoming caller should be present");
        let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
        assert!(outgoing.is_empty(), "outgoing should be empty due to error");
    }

    // ── CG-1: Grep fallback path in find_callers_callees ─────────────────────────

    #[tokio::test]
    async fn test_find_callers_callees_grep_fallback_with_mock_scout() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));
        // SPEC 008: search_codebase_impl calls enclosing_symbol_detail for each match
        // We have 1 match, so push 1 enclosing_symbol_detail_result
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create a file so the version hash computation has something to read
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        // Create a caller file (different from the definition file)
        std::fs::write(
            ws_dir.path().join("src/caller.rs"),
            "fn handle_request() { login(); }",
        )
        .unwrap();
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/caller.rs".to_string(),
                line: 1,
                column: 1,
                content: "fn handle_request() { login(); }".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:abc".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded);
        assert_eq!(val.degraded_reason, Some(DegradedReason::NoLspGrepFallback));
        let incoming = val.incoming.as_ref().expect("must be Some from grep");
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].file, "src/caller.rs");
        assert_eq!(incoming[0].direction, "incoming_heuristic");
    }

    // ── PATCH-002: Non-source file filtering in grep fallback ───────────

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn test_find_callers_callees_grep_fallback_filters_non_source_files() {
        // Issue: grep fallback was returning matches from .md, .json, .txt, etc.
        // causing false positives. This test verifies that non-source files
        // are filtered out of the results.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));
        // SPEC 008: search_codebase_impl calls enclosing_symbol_detail for each match
        // We have 4 matches, so push 4 results
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .extend([Ok(None), Ok(None), Ok(None), Ok(None)]);

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create the definition file
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        // Return a mix of source and non-source files that match the symbol name
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![
                // Legitimate source file caller
                pathfinder_search::SearchMatch {
                    file: "src/caller.rs".to_string(),
                    line: 1,
                    column: 1,
                    content: "fn call() { login(); }".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:a".to_string(),
                    known: Some(false),
                },
                // Documentation file - should be filtered OUT
                pathfinder_search::SearchMatch {
                    file: "docs/README.md".to_string(),
                    line: 10,
                    column: 1,
                    content: "call login() to authenticate".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:b".to_string(),
                    known: Some(false),
                },
                // Config file - should be filtered OUT
                pathfinder_search::SearchMatch {
                    file: "config.json".to_string(),
                    line: 5,
                    column: 1,
                    content: "\"login\": \"/api/auth\"".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:c".to_string(),
                    known: Some(false),
                },
                // TypeScript source - should be KEPT
                pathfinder_search::SearchMatch {
                    file: "web/src/auth.ts".to_string(),
                    line: 20,
                    column: 1,
                    content: "import { login } from './api';".to_string(),
                    context_before: vec![],
                    context_after: vec![],
                    enclosing_semantic_path: None,
                    is_definition: None,
                    version_hash: "sha256:d".to_string(),
                    known: Some(false),
                },
            ],
            total_matches: 4,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded);
        assert_eq!(val.degraded_reason, Some(DegradedReason::NoLspGrepFallback));
        let incoming = val.incoming.as_ref().expect("must be Some from grep");

        // Only the 2 source files should remain (.rs and .ts)
        // .md and .json should be filtered out
        assert_eq!(
            incoming.len(),
            2,
            "non-source files should be filtered, got: {:?}",
            incoming.iter().map(|r| &r.file).collect::<Vec<_>>()
        );

        // Verify the correct files are kept
        let files: std::collections::HashSet<_> =
            incoming.iter().map(|r| r.file.as_str()).collect();
        assert!(files.contains("src/caller.rs"), "should keep .rs file");
        assert!(files.contains("web/src/auth.ts"), "should keep .ts file");
        assert!(!files.contains("docs/README.md"), "should filter .md file");
        assert!(!files.contains("config.json"), "should filter .json file");
    }

    // ── DS-1: DocumentGuard lifecycle tests ──────────────────────────────────

    #[tokio::test]
    async fn test_find_callers_callees_closes_document_on_success() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());
        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
            ..Default::default()
        };

        let _ = server.find_callers_callees_impl(params).await;

        tokio::task::yield_now().await;

        assert_eq!(
            lawyer.did_open_call_count(),
            lawyer.did_close_call_count(),
            "DS-1: did_open and did_close must be symmetric in find_callers_callees"
        );
    }

    // ── TASK-2: project_only filter ───────────────────────────────────────────

    /// With `project_only = true` (the default), absolute stdlib paths should be
    /// silently dropped from the BFS impact graph.
    #[tokio::test]
    async fn test_find_callers_callees_project_only_true_filters_stdlib_refs() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));

        // No incoming callers
        lawyer.push_incoming_call_result(Ok(vec![]));

        // Outgoing: an absolute stdlib path — should be filtered when project_only=true
        lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "write_all".into(),
                kind: "function".into(),
                detail: None,
                file: "/home/user/.rustup/toolchains/stable/lib/std/io.rs".into(),
                line: 100,
                column: 4,
                data: None,
            },
            call_sites: vec![10],
        }]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
            // project_only defaults to true via Default::default()
            ..Default::default()
        };
        let result = server
            .find_callers_callees_impl(params)
            .await
            .expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
        assert_eq!(
            outgoing.len(),
            0,
            "project_only=true (default) must filter out stdlib absolute paths"
        );
    }

    // ── TASK-6: max_references truncation ─────────────────────────────────────

    /// When the number of BFS-found references exceeds `max_references`, the
    /// result must be truncated and `references_truncated = true`.
    #[tokio::test]
    async fn test_find_callers_callees_max_references_truncates_results() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));

        // Push 5 incoming callers (each on a unique file to avoid dedup)
        let incoming_calls: Vec<CallHierarchyCall> = (1..=5)
            .map(|i| CallHierarchyCall {
                item: CallHierarchyItem {
                    name: format!("caller_{i}"),
                    kind: "function".into(),
                    detail: None,
                    file: format!("src/caller_{i}.rs"),
                    line: i * 10,
                    column: 4,
                    data: None,
                },
                call_sites: vec![i * 10],
            })
            .collect();
        lawyer.push_incoming_call_result(Ok(incoming_calls));

        // Push 3 outgoing callees to also exhaust outgoing budget
        let outgoing_calls: Vec<CallHierarchyCall> = (1..=3)
            .map(|i| CallHierarchyCall {
                item: CallHierarchyItem {
                    name: format!("callee_{i}"),
                    kind: "function".into(),
                    detail: None,
                    file: format!("src/callee_{i}.rs"),
                    line: i * 10,
                    column: 4,
                    data: None,
                },
                call_sites: vec![i * 10],
            })
            .collect();
        lawyer.push_outgoing_call_result(Ok(outgoing_calls));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
            max_references: 2, // Budget split: incoming gets 1, outgoing gets 1. Total budget=2.
            ..Default::default()
        };
        let result = server
            .find_callers_callees_impl(params)
            .await
            .expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        let incoming = val.incoming.as_ref().expect("incoming must be Some");
        assert_eq!(
            incoming.len(),
            1,
            "incoming refs must be capped at max_references/2=1"
        );
        assert!(
            val.references_truncated,
            "references_truncated must be true when total budget is exhausted"
        );
    }

    /// Verify that the `default_max_references()` constant is 50.
    ///
    /// This ensures the plan's specified default wasn't accidentally changed.
    #[test]
    fn test_find_callers_callees_default_max_references_is_50() {
        use crate::server::types::default_max_references;
        assert_eq!(
            default_max_references(),
            50,
            "default_max_references must be 50 per the remediation plan spec"
        );
    }

    // ── find_callers_callees edge cases ─────────────────────────────────

    #[tokio::test]
    async fn test_find_callers_callees_handles_empty_incoming_and_outgoing() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Empty call hierarchy results
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 3,
            max_references: 50,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.incoming.is_none() || val.incoming.as_ref().unwrap().is_empty());
        assert!(val.outgoing.is_none() || val.outgoing.as_ref().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_find_callers_callees_respects_max_depth() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Provide incoming calls at depth 1
        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));
        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "main".into(),
                kind: "function".into(),
                detail: None,
                file: "src/main.rs".into(),
                line: 5,
                column: 4,
                data: None,
            },
            call_sites: vec![5],
        }]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1, // Limit depth to 1
            max_references: 50,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Should have incoming call from main
        let incoming = val
            .incoming
            .as_ref()
            .expect("incoming must be Some when not degraded");
        assert!(!incoming.is_empty(), "should have incoming calls");
        assert!(
            incoming.iter().all(|r| r.depth <= 1),
            "all refs should be within max_depth"
        );
    }

    // ── Phase 4C: Navigation Residual Gaps ───────────────────────────────

    #[tokio::test]
    async fn test_find_callers_callees_bfs_handles_cycle_in_call_graph() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        // Create a cycle: A -> B -> A using existing test files
        let item_a = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item_a.clone()]));

        // A calls validate_token
        let item_b = CallHierarchyItem {
            name: "validate_token".into(),
            kind: "function".into(),
            detail: None,
            file: "src/token.rs".into(),
            line: 20,
            column: 4,
            data: None,
        };
        lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
            item: item_b.clone(),
            call_sites: vec![15],
        }]));

        // validate_token calls login (cycle back)
        lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
            item: item_a.clone(),
            call_sites: vec![25],
        }]));

        lawyer.push_incoming_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 3,
            ..Default::default()
        };

        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Should not hang or panic
        assert!(!val.degraded);
        let outgoing = val.outgoing.as_ref().expect("must be Some");
        // Should deduplicate: login should not appear in its own outgoing
        assert!(
            !outgoing
                .iter()
                .any(|r| r.file == "src/auth.rs" && r.semantic_path.contains("login")),
            "cycle should be deduplicated"
        );
    }

    #[tokio::test]
    async fn test_find_callers_callees_bfs_deduplicates_cross_referenced_symbols() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));

        // Create duplicate references: same symbol referenced twice
        let caller_item = CallHierarchyItem {
            name: "handler".into(),
            kind: "function".into(),
            detail: None,
            file: "src/handler.rs".into(),
            line: 10,
            column: 4,
            data: None,
        };
        // Push same item twice with different call sites
        lawyer.push_incoming_call_result(Ok(vec![
            CallHierarchyCall {
                item: caller_item.clone(),
                call_sites: vec![20],
            },
            CallHierarchyCall {
                item: caller_item.clone(),
                call_sites: vec![35],
            },
        ]));

        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
            ..Default::default()
        };

        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        let incoming = val.incoming.as_ref().expect("must be Some");
        // Should deduplicate based on item (not call sites)
        // Check by semantic path since name is not available in ImpactReference
        let handler_count = incoming
            .iter()
            .filter(|r| r.semantic_path.contains("handler") || r.file == "src/handler.rs")
            .count();
        assert_eq!(
            handler_count, 1,
            "cross-referenced symbol should be deduplicated"
        );
    }

    #[tokio::test]
    async fn test_find_callers_callees_grep_fallback_provides_incoming_heuristic() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // SPEC 008: search_codebase_impl calls enclosing_symbol_detail for each match
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create files
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();
        std::fs::write(
            ws_dir.path().join("src/caller.rs"),
            "fn handle_request() { login(); }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/caller.rs".to_string(),
                line: 1,
                column: 1,
                content: "fn handle_request() { login(); }".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:abc".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        // Use NoOpLawyer to force grep fallback
        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = crate::server::types::TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
            ..Default::default()
        };

        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded);
        assert_eq!(val.degraded_reason, Some(DegradedReason::NoLspGrepFallback));
        let incoming = val.incoming.as_ref().expect("must be Some from grep");
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].file, "src/caller.rs");
        assert_eq!(incoming[0].direction, "incoming_heuristic");
    }

    #[tokio::test]
    async fn test_find_callers_callees_grep_fallback_no_results_stays_none() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // SPEC 008: search_codebase_impl calls enclosing_symbol_detail for each match
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Create files — login calls validate_token
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { validate_token() }",
        )
        .unwrap();
        std::fs::write(
            ws_dir.path().join("src/token.rs"),
            "fn validate_token() -> bool { true }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        // Search for "login" finds the definition in auth.rs (which is filtered out)
        // and no other references, so grep fallback returns None for incoming.
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![],
            total_matches: 0,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        // Use NoOpLawyer to force grep fallback
        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = crate::server::types::TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
            ..Default::default()
        };

        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded);
        // When grep fallback returns no results, degraded_reason stays at default NoLsp
        assert_eq!(val.degraded_reason, Some(DegradedReason::NoLsp));
        // make_scope() returns "fn login() { }" with empty body — no function calls
        // to extract. Grep outgoing fallback exists but finds zero candidates.
        assert!(
            val.outgoing.is_none(),
            "outgoing should be None — empty function body has no call candidates"
        );
        // No search results means no incoming either
        assert!(
            val.incoming.is_none(),
            "incoming should be None when search returns no matches"
        );
    }

    // ── GAP 3: method call extraction for Rust/Go/Java ──────────────────────

    #[tokio::test]
    async fn test_extract_call_candidates_captures_method_calls() {
        // Verify that call_pattern_full() now used for ALL languages captures
        // method calls like self.validate(), s.HandleRequest(), service.process().
        use super::super::extract_call_candidates;

        // Rust method call
        let rust_code = "fn login(&self) { self.validate_token(); self.hash_password(); }";
        let rust_candidates = extract_call_candidates(rust_code, "rust");
        assert!(
            rust_candidates.contains(&"validate_token".to_string()),
            "should capture self.validate_token() in Rust"
        );
        assert!(
            rust_candidates.contains(&"hash_password".to_string()),
            "should capture self.hash_password() in Rust"
        );

        // Go method call
        let go_code = "func (h *Handler) Login() { h.service.Validate(); }";
        let go_candidates = extract_call_candidates(go_code, "go");
        assert!(
            go_candidates.contains(&"Validate".to_string()),
            "should capture h.service.Validate() in Go"
        );

        // Java method call
        let java_code = "public void login() { this.service.process(); }";
        let java_candidates = extract_call_candidates(java_code, "java");
        assert!(
            java_candidates.contains(&"process".to_string()),
            "should capture this.service.process() in Java"
        );
    }

    // ── GAP 6: per-language method call extraction tests ──────────────────────

    #[tokio::test]
    async fn test_extract_call_candidates_rust_method_calls() {
        use super::super::extract_call_candidates;

        let code = "fn login(&self) { self.validate(); self.hash_password(); self.save(); }";
        let candidates = extract_call_candidates(code, "rust");
        assert!(
            candidates.contains(&"validate".to_string()),
            "should capture self.validate() in Rust"
        );
        assert!(
            candidates.contains(&"hash_password".to_string()),
            "should capture self.hash_password() in Rust"
        );
        assert!(
            candidates.contains(&"save".to_string()),
            "should capture self.save() in Rust"
        );
    }

    #[tokio::test]
    async fn test_extract_call_candidates_go_method_calls() {
        use super::super::extract_call_candidates;

        let code = "func (s *Server) Handle() { s.Validate(); s.Process(); }";
        let candidates = extract_call_candidates(code, "go");
        assert!(
            candidates.contains(&"Validate".to_string()),
            "should capture s.Validate() in Go"
        );
        assert!(
            candidates.contains(&"Process".to_string()),
            "should capture s.Process() in Go"
        );
    }

    #[tokio::test]
    async fn test_extract_call_candidates_java_method_calls() {
        use super::super::extract_call_candidates;

        let code = "public void login() { this.service.process(); this.dao.save(); }";
        let candidates = extract_call_candidates(code, "java");
        assert!(
            candidates.contains(&"process".to_string()),
            "should capture this.service.process() in Java"
        );
        assert!(
            candidates.contains(&"save".to_string()),
            "should capture this.dao.save() in Java"
        );
    }

    // ── GAP 7: outgoing fallback end-to-end tests ──────────────────────────
    //
    // Strategy for handling non-deterministic HashSet iteration:
    // extract_call_candidates extracts both the fn name from the signature and
    // body calls. With set_results, we queue enough results so that regardless
    // of candidate ordering, the correct matches are returned.
    //
    // For the happy-path test with scope "fn handle(&self) { self.validate(); }":
    //   Candidates: {"handle", "validate"} (HashSet, order varies)
    //   We queue results where EVERY search returns the "validate" match from
    //   token.rs. The "handle" candidate search gets a match in token.rs which
    //   won't form a valid fn definition but still gets added as outgoing_heuristic.
    //   We verify outgoing is Some with at least one entry having the right direction.

    #[tokio::test]
    async fn test_outgoing_fallback_happy_path() {
        // When LSP is unavailable and the function body has calls,
        // outgoing should be Some with direction "outgoing_heuristic".
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
            pathfinder_common::types::SymbolScope {
                content: "fn handle(&self) { self.validate(); }".to_string(),
                start_line: 9,
                end_line: 9,
                name_column: 0,
                language: "rust".to_string(),
            },
        ));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/handler.rs"),
            "fn handle(&self) { self.validate(); }",
        )
        .unwrap();
        std::fs::write(
            ws_dir.path().join("src/validator.rs"),
            "fn validate() -> bool { true }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());

        let validate_match = pathfinder_search::SearchMatch {
            file: "src/validator.rs".to_string(),
            line: 1,
            column: 0,
            content: "fn validate() -> bool { true }".to_string(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "sha256:abc".to_string(),
            known: Some(false),
        };
        let empty_result = Ok(pathfinder_search::SearchResult {
            matches: vec![],
            total_matches: 0,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        });
        let validate_result = Ok(pathfinder_search::SearchResult {
            matches: vec![validate_match],
            total_matches: 1,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        });

        // Queue: 1st = incoming search (empty), then enough for outgoing candidates
        // Candidates from HashSet: "handle" + "validate" in unknown order.
        // Both get validate_result so we don't depend on order.
        scout.set_results(vec![
            empty_result.clone(),    // incoming search
            validate_result.clone(), // 1st outgoing candidate (handle or validate)
            validate_result.clone(), // 2nd outgoing candidate (the other)
        ]);

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = crate::server::types::TraceParams {
            semantic_path: "src/handler.rs::handle".to_owned(),
            max_depth: 2,
            ..Default::default()
        };

        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded);
        assert_eq!(val.degraded_reason, Some(DegradedReason::NoLspGrepFallback));
        let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
        assert!(
            !outgoing.is_empty(),
            "should have at least one outgoing ref"
        );
        assert_eq!(
            outgoing[0].direction, "outgoing_heuristic",
            "direction must be outgoing_heuristic"
        );
    }

    #[tokio::test]
    async fn test_outgoing_fallback_dedup_by_semantic_path() {
        // Same function called multiple times should appear once in outgoing.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
            pathfinder_common::types::SymbolScope {
                content: "fn process(&self) { self.run(); self.run(); self.run(); }".to_string(),
                start_line: 5,
                end_line: 5,
                name_column: 0,
                language: "rust".to_string(),
            },
        ));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/worker.rs"),
            "fn process(&self) { self.run(); }",
        )
        .unwrap();
        std::fs::write(ws_dir.path().join("src/runner.rs"), "fn run() { }").unwrap();

        let scout = Arc::new(MockScout::default());

        let run_match = pathfinder_search::SearchMatch {
            file: "src/runner.rs".to_string(),
            line: 1,
            column: 0,
            content: "fn run() { }".to_string(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "sha256:abc".to_string(),
            known: Some(false),
        };
        let empty_result = Ok(pathfinder_search::SearchResult {
            matches: vec![],
            total_matches: 0,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        });
        let run_result = Ok(pathfinder_search::SearchResult {
            matches: vec![run_match],
            total_matches: 1,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        });

        // Candidates: "process" + "run" (deduped by HashSet in extract_call_candidates)
        // But grep_outgoing_fallback also deduplicates by semantic_path.
        // Both candidates get run_result, but only the "run" match adds
        // "src/runner.rs::run" (the "process" candidate search also gets run_result
        // but forms "src/runner.rs::process" which is a different semantic_path).
        // The seen set ensures no dupes regardless.
        scout.set_results(vec![
            empty_result.clone(), // incoming
            run_result.clone(),   // 1st outgoing candidate
            run_result.clone(),   // 2nd outgoing candidate
        ]);

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = crate::server::types::TraceParams {
            semantic_path: "src/worker.rs::process".to_owned(),
            max_depth: 2,
            ..Default::default()
        };

        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
        // All outgoing refs should have unique semantic_paths
        let paths: std::collections::HashSet<&str> =
            outgoing.iter().map(|r| r.semantic_path.as_str()).collect();
        assert_eq!(
            paths.len(),
            outgoing.len(),
            "all outgoing refs must have unique semantic_paths"
        );
    }

    #[tokio::test]
    async fn test_outgoing_fallback_definition_file_exclusion() {
        // When a candidate resolves to the definition file, it should be excluded.
        // Test uses scope with only one candidate (validate) and sets up search
        // to return a match in the definition file, which gets skipped.
        // With GAP 5 fix, the 2nd match (in another file) should be used instead.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon.read_symbol_scope_results.lock().unwrap().push(Ok(
            pathfinder_common::types::SymbolScope {
                content: "fn do_work() { validate(); }".to_string(),
                start_line: 10,
                end_line: 10,
                name_column: 0,
                language: "rust".to_string(),
            },
        ));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/worker.rs"),
            "fn do_work() { validate(); }\nfn validate() { }",
        )
        .unwrap();
        std::fs::write(ws_dir.path().join("src/lib.rs"), "fn validate() { }").unwrap();

        let scout = Arc::new(MockScout::default());

        // GAP 5 scenario: first match is in definition file (skipped),
        // second match is in another file (should be used).
        let local_match = pathfinder_search::SearchMatch {
            file: "src/worker.rs".to_string(),
            line: 2,
            column: 0,
            content: "fn validate() { }".to_string(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "sha256:abc".to_string(),
            known: Some(false),
        };
        let external_match = pathfinder_search::SearchMatch {
            file: "src/lib.rs".to_string(),
            line: 1,
            column: 0,
            content: "fn validate() { }".to_string(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "sha256:def".to_string(),
            known: Some(false),
        };
        let empty_result = Ok(pathfinder_search::SearchResult {
            matches: vec![],
            total_matches: 0,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        });
        // Candidates: {"do_work", "validate"} — order unknown
        // For "do_work": search gets the two-match result (neither is a valid fn do_work)
        // For "validate": search gets the two-match result (first=definition, second=external)
        // With GAP 5 fix, the second match (src/lib.rs) is used.
        let two_match_result = Ok(pathfinder_search::SearchResult {
            matches: vec![local_match, external_match],
            total_matches: 2,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        });

        scout.set_results(vec![
            empty_result,             // incoming
            two_match_result.clone(), // 1st outgoing candidate
            two_match_result,         // 2nd outgoing candidate
        ]);

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = crate::server::types::TraceParams {
            semantic_path: "src/worker.rs::do_work".to_owned(),
            max_depth: 2,
            ..Default::default()
        };

        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
        // No outgoing ref should be in the definition file (src/worker.rs)
        for reference in outgoing {
            assert_ne!(
                reference.file, "src/worker.rs",
                "outgoing refs should exclude the definition file"
            );
        }
        // Should have at least one outgoing ref (src/lib.rs::validate)
        assert!(
            outgoing.iter().any(|r| r.file == "src/lib.rs"),
            "should have resolved validate to src/lib.rs"
        );
    }

    // ── BFS multi-node continuation after error ──────────────────────

    #[tokio::test]
    async fn test_find_callers_callees_bfs_continues_after_single_node_error() {
        // When queue has items A, B and querying A fails, B should still be processed.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));

        // Incoming: first call fails (error for initial item), second succeeds
        lawyer.push_incoming_call_result(Err(LspError::Protocol("transient error".to_string())));
        // Outgoing succeeds with one callee
        lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "validate_token".into(),
                kind: "function".into(),
                detail: Some("fn validate_token()".into()),
                file: "src/token.rs".into(),
                line: 15,
                column: 4,
                data: None,
            },
            call_sites: vec![9],
        }]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
            max_references: 50,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed despite BFS error");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Not degraded — the LSP prepare succeeded, BFS errors are partial failures
        assert!(!val.degraded);
        // Incoming errored → empty vec (not None)
        let incoming = val.incoming.as_ref().expect("incoming must be Some");
        assert!(incoming.is_empty(), "incoming should be empty after error");
        // Outgoing succeeded
        let outgoing = val.outgoing.as_ref().expect("outgoing must be Some");
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].file, "src/token.rs");
    }

    // ── BFS text output format ──────────────────────────────────────

    #[tokio::test]
    async fn test_find_callers_callees_bfs_formats_response_correctly() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));

        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "handle_request".into(),
                kind: "function".into(),
                detail: Some("fn handle_request()".into()),
                file: "src/server.rs".into(),
                line: 20,
                column: 4,
                data: None,
            },
            call_sites: vec![25],
        }]));

        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");

        // Verify text output format
        let text = match &call_res.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        assert!(text.contains("Incoming references: 1"), "text: {text}");
        assert!(text.contains("Outgoing references: 0"), "text: {text}");
        assert!(text.contains("[depth="), "text: {text}");
        assert!(text.contains("src/server.rs:L20"), "text: {text}");
        assert!(text.contains("[completed in"), "text: {text}");
    }

    #[tokio::test]
    async fn test_find_callers_callees_bfs_aborts_on_consecutive_failures() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));

        let caller_item = CallHierarchyItem {
            name: "caller".into(),
            kind: "function".into(),
            detail: None,
            file: "src/caller.rs".into(),
            line: 5,
            column: 4,
            data: None,
        };

        // Incoming: first call returns a caller (so BFS has something to traverse),
        // then every subsequent call for deeper levels fails.
        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: caller_item.clone(),
            call_sites: vec![9],
        }]));
        // Next BFS step: incoming for caller fails
        lawyer.push_incoming_call_result(Err(LspError::Protocol("hung".to_string())));
        // Next BFS step: incoming fails again → 2 consecutive failures → abort
        lawyer.push_incoming_call_result(Err(LspError::Protocol("still hung".to_string())));

        // Outgoing: empty
        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 4,
            max_references: 50,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed with partial results");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        let incoming = val.incoming.as_ref().expect("must be Some");
        assert_eq!(incoming.len(), 1, "should have 1 caller before abort");
        assert_eq!(incoming[0].semantic_path, "src/caller.rs::caller");
    }

    #[tokio::test]
    async fn test_find_callers_callees_bfs_cycle_detection_incoming() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        let item_a = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item_a.clone()]));

        let item_b = CallHierarchyItem {
            name: "validate_token".into(),
            kind: "function".into(),
            detail: None,
            file: "src/token.rs".into(),
            line: 20,
            column: 4,
            data: None,
        };
        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: item_b.clone(),
            call_sites: vec![15],
        }]));

        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: item_a.clone(),
            call_sites: vec![25],
        }]));

        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 3,
            ..Default::default()
        };

        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        let incoming = val.incoming.as_ref().expect("must be Some");
        assert!(
            !incoming
                .iter()
                .any(|r| r.file == "src/auth.rs" && r.semantic_path.contains("login")),
            "cycle should be deduplicated"
        );
    }

    #[tokio::test]
    async fn test_find_callers_callees_empty_callee_intermediate() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        let item_a = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item_a.clone()]));

        let item_b = CallHierarchyItem {
            name: "validate_token".into(),
            kind: "function".into(),
            detail: None,
            file: "src/token.rs".into(),
            line: 20,
            column: 4,
            data: None,
        };
        lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
            item: item_b.clone(),
            call_sites: vec![15],
        }]));

        lawyer.push_outgoing_call_result(Ok(vec![]));
        lawyer.push_incoming_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 3,
            ..Default::default()
        };

        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        let outgoing = val.outgoing.as_ref().expect("must be Some");
        assert_eq!(outgoing.len(), 1);
        assert_eq!(outgoing[0].file, "src/token.rs");
    }

    #[tokio::test]
    async fn test_find_callers_callees_bfs_partial_resolution_failure() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        let item_a = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item_a.clone()]));

        let item_b = CallHierarchyItem {
            name: "caller_b".into(),
            kind: "function".into(),
            detail: None,
            file: "src/caller_b.rs".into(),
            line: 10,
            column: 4,
            data: None,
        };
        let item_c = CallHierarchyItem {
            name: "caller_c".into(),
            kind: "function".into(),
            detail: None,
            file: "src/caller_c.rs".into(),
            line: 10,
            column: 4,
            data: None,
        };

        lawyer.push_incoming_call_result(Ok(vec![
            CallHierarchyCall {
                item: item_b.clone(),
                call_sites: vec![5],
            },
            CallHierarchyCall {
                item: item_c.clone(),
                call_sites: vec![6],
            },
        ]));

        lawyer.push_incoming_call_result(Ok(vec![]));
        lawyer.push_incoming_call_result(Err(LspError::Protocol("failed".to_string())));
        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 3,
            ..Default::default()
        };

        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        let incoming = val.incoming.as_ref().expect("must be Some");
        assert_eq!(incoming.len(), 2);
    }

    #[tokio::test]
    async fn test_find_callers_callees_lsp_error_triggers_grep_fallback() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();
        std::fs::write(
            ws_dir.path().join("src/caller.rs"),
            "fn handle_request() { login(); }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/caller.rs".to_string(),
                line: 1,
                column: 1,
                content: "fn handle_request() { login(); }".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:abc".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 0,
            files_in_scope: 0,
            binary_skipped: 0,
            gitignored_skipped: 0,
            other_skipped: 0,
        }));

        let lawyer = Arc::new(MockLawyer::default());
        lawyer.push_prepare_call_hierarchy_result(Err(LspError::Protocol("LSP error".to_string())));

        let server =
            PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
            ..Default::default()
        };

        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed despite LSP error");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded);
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspErrorGrepFallback)
        );
        let incoming = val.incoming.as_ref().expect("must be Some from grep");
        assert_eq!(incoming.len(), 1);
        assert_eq!(incoming[0].file, "src/caller.rs");
    }

    #[tokio::test]
    async fn test_find_callers_callees_invalid_semantic_path() {
        let surgeon = Arc::new(MockSurgeon::new());
        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "invalid_path_format".to_owned(),
            ..Default::default()
        };

        let result = server.find_callers_callees_impl(params).await;
        let Err(err) = result else {
            panic!("expected error");
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "INVALID_SEMANTIC_PATH");
    }

    #[tokio::test]
    async fn test_find_callers_callees_max_depth_boundaries() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));
        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "caller".into(),
                kind: "function".into(),
                detail: Some("fn caller()".into()),
                file: "src/caller.rs".into(),
                line: 20,
                column: 4,
                data: None,
            },
            call_sites: vec![25],
        }]));
        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon.clone(), lawyer.clone());

        let zero_depth_params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 0,
            ..Default::default()
        };
        let zero_depth_res = server
            .find_callers_callees_impl(zero_depth_params)
            .await
            .expect("success");
        let zero_depth_val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(zero_depth_res.structured_content.unwrap()).unwrap();
        assert!(!zero_depth_val.degraded);

        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "caller".into(),
                kind: "function".into(),
                detail: Some("fn caller()".into()),
                file: "src/caller.rs".into(),
                line: 20,
                column: 4,
                data: None,
            },
            call_sites: vec![25],
        }]));
        lawyer.push_outgoing_call_result(Ok(vec![]));

        let large_depth_params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 10,
            ..Default::default()
        };
        let large_depth_res = server
            .find_callers_callees_impl(large_depth_params)
            .await
            .expect("success");
        let large_depth_val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(large_depth_res.structured_content.unwrap()).unwrap();
        assert!(!large_depth_val.degraded);
    }

    #[tokio::test]
    async fn test_find_callers_callees_empty_results_text_formatting() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
        lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
            file: "src/auth.rs".into(),
            line: 10,
            column: 4,
            preview: "fn login() {}".into(),
        })));
        lawyer.push_incoming_call_result(Ok(vec![]));
        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            ..Default::default()
        };

        let result = server
            .find_callers_callees_impl(params)
            .await
            .expect("success");
        let text = result.content[0].as_text().expect("must be text");

        // BFS empty + grep empty = confirmed zero, should NOT be degraded.
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();
        assert!(
            val.degraded,
            "both BFS results empty → grep fallback fires → must be degraded"
        );

        // Text should show DEGRADED notice
        assert!(
            text.text.contains("DEGRADED (lsp_warmup_grep_fallback)"),
            "Text output did not format zero results correctly: {}",
            text.text
        );
    }

    #[tokio::test]
    async fn test_find_callers_callees_references_truncated() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));

        lawyer.push_incoming_call_result(Ok(vec![
            CallHierarchyCall {
                item: CallHierarchyItem {
                    name: "caller_1".into(),
                    kind: "function".into(),
                    detail: None,
                    file: "src/caller_1.rs".into(),
                    line: 10,
                    column: 4,
                    data: None,
                },
                call_sites: vec![10],
            },
            CallHierarchyCall {
                item: CallHierarchyItem {
                    name: "caller_2".into(),
                    kind: "function".into(),
                    detail: None,
                    file: "src/caller_2.rs".into(),
                    line: 20,
                    column: 4,
                    data: None,
                },
                call_sites: vec![20],
            },
        ]));
        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_references: 1,
            ..Default::default()
        };

        let result = server
            .find_callers_callees_impl(params)
            .await
            .expect("success");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert!(val.references_truncated);
    }

    #[tokio::test]
    async fn test_find_callers_callees_unusual_symbol_types() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
        lawyer.push_incoming_call_result(Ok(vec![]));
        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon.clone(), lawyer);

        let params_macro = TraceParams {
            semantic_path: "src/auth.rs::my_macro!".to_owned(),
            ..Default::default()
        };
        let result_macro = server.find_callers_callees_impl(params_macro).await;
        assert!(result_macro.is_ok(), "Macro symbol type should succeed");

        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));
        let params_trait = TraceParams {
            semantic_path: "src/auth.rs::<impl User>::login".to_owned(),
            ..Default::default()
        };
        let result_trait = server.find_callers_callees_impl(params_trait).await;
        assert!(
            result_trait.is_ok(),
            "Trait impl symbol type should succeed"
        );
    }

    // ── Regression: degraded_reason must reflect actual failure cause ────────

    /// Regression: LSP timeout with empty grep results must report `LspTimeoutGrepFallback`,
    /// NOT `NoLsp`. Previously the initial `degraded_reason` = `NoLsp` was never overridden when
    /// grep found nothing, causing agents to think no LSP existed instead of "retry later".
    #[tokio::test]
    async fn test_lsp_timeout_empty_grep_reports_timeout_reason() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));
        // empty enclosing so grep_outgoing_fallback finds nothing
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));

        let lawyer = Arc::new(MockLawyer::default());
        // Simulate LSP timeout on prepare
        lawyer.push_prepare_call_hierarchy_result(Err(LspError::Timeout {
            operation: "callHierarchy/incomingCalls".to_string(),
            timeout_ms: 5000,
        }));

        // Use make_server_with_lawyer (workspace has no src/ files, so grep finds nothing)
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            ..Default::default()
        };

        let result = server
            .find_callers_callees_impl(params)
            .await
            .expect("should succeed degraded");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert!(val.degraded, "must be degraded on timeout");
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspTimeoutGrepFallback),
            "timeout must report LspTimeoutGrepFallback even when grep finds nothing"
        );
    }

    /// Regression: LSP protocol error with empty grep results must report `LspErrorGrepFallback`,
    /// NOT `NoLsp`. Previously `degraded_reason` = `NoLsp` was set and never overridden when grep
    /// found nothing, misleading agents into "LSP not installed" guidance.
    #[tokio::test]
    async fn test_lsp_protocol_error_empty_grep_reports_error_reason() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));

        let lawyer = Arc::new(MockLawyer::default());
        lawyer.push_prepare_call_hierarchy_result(Err(LspError::Protocol(
            "internal server error".to_string(),
        )));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            ..Default::default()
        };

        let result = server
            .find_callers_callees_impl(params)
            .await
            .expect("should succeed degraded");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert!(val.degraded, "must be degraded on LSP error");
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspErrorGrepFallback),
            "LSP error must report LspErrorGrepFallback even when grep finds nothing"
        );
    }

    // ── P2-7: Hint logic tests ──────────────────────────────────────────────────

    #[tokio::test]
    async fn test_hint_absent_when_both_callers_and_callees_empty() {
        // When BFS returns empty for BOTH incoming AND outgoing, the grep fallback
        // fires and always sets degraded=true (to guard against BFS errors vs genuine
        // zero callers). Therefore hint must be None in this case.
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
        lawyer.push_incoming_call_result(Ok(vec![]));
        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(
            val.degraded,
            "must be degraded when both BFS results are empty (grep fallback triggered)"
        );
        assert!(val.hint.is_none(), "hint must be absent when degraded");
    }

    #[tokio::test]
    async fn test_hint_present_when_only_incoming_callers_empty() {
        // LSP succeeds, incoming empty but outgoing has items → hint about "zero incoming callers"
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
        lawyer.push_incoming_call_result(Ok(vec![]));
        lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "validate".into(),
                kind: "function".into(),
                detail: None,
                file: "src/validate.rs".into(),
                line: 5,
                column: 0,
                data: None,
            },
            call_sites: vec![10],
        }]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded, "should not be degraded");
        assert!(
            val.hint.is_some(),
            "hint must be present when incoming callers are empty"
        );
        let hint = val.hint.unwrap();
        assert!(
            hint.contains("zero incoming callers"),
            "hint must mention zero incoming callers, got: {hint}"
        );
    }

    #[tokio::test]
    async fn test_hint_absent_when_callers_exist() {
        // LSP succeeds, incoming has items → no hint
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        let item = CallHierarchyItem {
            name: "login".into(),
            kind: "function".into(),
            detail: None,
            file: "src/auth.rs".into(),
            line: 9,
            column: 4,
            data: None,
        };
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
        lawyer.push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "main".into(),
                kind: "function".into(),
                detail: None,
                file: "src/main.rs".into(),
                line: 1,
                column: 0,
                data: None,
            },
            call_sites: vec![5],
        }]));
        lawyer.push_outgoing_call_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded, "should not be degraded");
        assert!(val.hint.is_none(), "hint must be absent when callers exist");
    }

    #[tokio::test]
    async fn test_hint_absent_when_degraded() {
        // Degraded path — hint should always be None regardless of empty lists
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = TraceParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
            ..Default::default()
        };
        let result = server.find_callers_callees_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::FindCallersCalleesMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded, "should be degraded (no LSP)");
        assert!(
            val.hint.is_none(),
            "hint must be absent when degraded, even if lists are empty"
        );
    }
}
