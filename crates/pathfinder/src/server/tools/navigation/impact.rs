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
#[derive(Debug, Clone, Copy)]
enum CallDirection {
    Incoming,
    Outgoing,
}

#[derive(Debug)]
pub(crate) struct GrepFallbackParams<'a> {
    pub symbol_name: &'a str,
    pub definition_path: &'a std::path::Path,
    pub scope_content: &'a str,
    pub scope_language: &'a str,
    pub remaining_outgoing: u32,
    pub project_only: bool,
}

/// Mutable accumulator threaded through the call-hierarchy resolution phases
/// of [`PathfinderServer::find_callers_callees_impl`]. Consolidates state
/// that would otherwise require a long list of `&mut` parameters across the
/// trait-method-expansion and normal-flow helpers.
///
/// `None` for `incoming`/`outgoing` means "unknown" (LSP was down — do NOT
/// treat as zero). `Some(vec)` means the LSP (or a grep fallback) produced a
/// result, possibly empty.
struct CallHierarchyState {
    incoming: Option<Vec<crate::server::types::ImpactReference>>,
    outgoing: Option<Vec<crate::server::types::ImpactReference>>,
    degraded: bool,
    degraded_reason: Option<DegradedReason>,
    engines: Vec<&'static str>,
    files_referenced: std::collections::HashSet<String>,
    max_depth_reached: u32,
    /// Set when the LSP BFS itself produced the current `incoming`/`outgoing`
    /// values (as opposed to a grep-based heuristic fallback). Used to
    /// compute `incoming_verified` / `outgoing_verified` accurately.
    lsp_bfs_ran: bool,
}

impl CallHierarchyState {
    /// Pessimistic default: assume degraded/no-LSP until a phase proves
    /// otherwise.
    fn new() -> Self {
        Self {
            incoming: None,
            outgoing: None,
            degraded: true,
            degraded_reason: Some(DegradedReason::NoLsp),
            engines: vec!["tree-sitter"],
            files_referenced: std::collections::HashSet::new(),
            max_depth_reached: 0,
            lsp_bfs_ran: false,
        }
    }
}

/// Read-only context shared by the call-hierarchy resolution helpers for
/// `find_callers_callees_impl`. Bundles the several by-reference parameters
/// that would otherwise need to be threaded through each helper
/// individually, keeping their argument counts within `too_many_arguments`.
struct CallerCalleeContext<'a> {
    semantic_path: &'a pathfinder_common::types::SemanticPath,
    scope: &'a pathfinder_common::types::SymbolScope,
    symbol_name: &'a str,
    max_depth: u32,
    project_only: bool,
}

/// Parameters for [`PathfinderServer::compute_callers_callees_hint`].
struct HintParams<'a> {
    used_impl_expansion: bool,
    trait_impl_count: usize,
    trait_impl_file_count: usize,
    degraded: bool,
    incoming: &'a Option<Vec<crate::server::types::ImpactReference>>,
    outgoing: &'a Option<Vec<crate::server::types::ImpactReference>>,
    symbol_name: &'a str,
}

/// Parameters for [`PathfinderServer::finalize_call_hierarchy_result`].
struct FinalizeCallHierarchyParams {
    state: CallHierarchyState,
    trait_method_degraded: Option<DegradedReason>,
    used_impl_expansion: bool,
    trait_impl_count: usize,
    trait_impl_file_count: usize,
    symbol_name: String,
    requested_semantic_path: String,
    tree_sitter_ms: u128,
    lsp_ms: u128,
    duration_ms: u128,
    max_references: u32,
}

/// Parameters for [`PathfinderServer::render_callers_callees_text`].
struct RenderCallersCalleesParams<'a> {
    degraded: bool,
    degraded_reason: Option<DegradedReason>,
    symbol_name: &'a str,
    inc_count: usize,
    out_count: usize,
    metadata: &'a crate::server::types::FindCallersCalleesMetadata,
    duration_ms: u128,
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
    async fn resolve_outgoing_candidate(
        &self,
        candidate: &str,
        scope_language: &str,
        definition_path: &std::path::Path,
        project_only: bool,
        seen: &mut std::collections::HashSet<String>,
    ) -> Option<crate::server::types::ImpactReference> {
        let pattern = super::candidate_definition_pattern(scope_language, candidate);
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
                for m in &search_result.matches {
                    if project_only
                        && (!super::is_source_file(&m.file) || !super::is_workspace_file(&m.file))
                    {
                        continue;
                    }

                    let m_path = std::path::Path::new(&m.file);
                    if m_path == definition_path {
                        continue;
                    }

                    let semantic_path = self
                        .enrich_semantic_path(
                            &m.file,
                            u32::try_from(m.line).unwrap_or(0),
                            candidate,
                        )
                        .await;

                    if seen.contains(&semantic_path) {
                        continue;
                    }
                    seen.insert(semantic_path.clone());

                    return Some(crate::server::types::ImpactReference {
                        semantic_path,
                        file: m.file.clone(),
                        line: usize::try_from(m.line).unwrap_or(usize::MAX),
                        snippet: m.content.clone(),
                        direction: "outgoing_heuristic".to_string(),
                        depth: 0,
                        confidence: Some("heuristic".to_owned()),
                    });
                }
                None
            }
            Err(e) => {
                tracing::warn!(
                    tool = "grep_outgoing_fallback",
                    candidate = %candidate,
                    error = %e,
                    "search failed for candidate"
                );
                None
            }
        }
    }
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

            if let Some(reference) = self
                .resolve_outgoing_candidate(
                    &candidate,
                    scope_language,
                    definition_path,
                    project_only,
                    &mut seen,
                )
                .await
            {
                files_referenced.insert(reference.file.clone());
                refs.push(reference);
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
    async fn run_grep_fallbacks(
        &self,
        params: &GrepFallbackParams<'_>,
        files_referenced: &mut std::collections::HashSet<String>,
    ) -> (
        Option<Vec<crate::server::types::ImpactReference>>,
        Option<Vec<crate::server::types::ImpactReference>>,
    ) {
        let incoming = self
            .grep_reference_fallback(params.symbol_name, params.definition_path, files_referenced)
            .await;

        let outgoing = self
            .grep_outgoing_fallback(
                params.scope_content,
                params.scope_language,
                params.definition_path,
                params.remaining_outgoing,
                params.project_only,
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
    async fn build_impact_reference(
        &self,
        referenced_item: &pathfinder_lsp::types::CallHierarchyItem,
        direction: &CallDirection,
        current_depth: u32,
    ) -> crate::server::types::ImpactReference {
        let semantic_path = self
            .enrich_semantic_path(
                &referenced_item.file,
                referenced_item.line,
                &referenced_item.name,
            )
            .await;

        crate::server::types::ImpactReference {
            semantic_path,
            file: referenced_item.file.clone(),
            line: usize::try_from(referenced_item.line).unwrap_or(usize::MAX),
            snippet: referenced_item
                .detail
                .clone()
                .unwrap_or_else(|| referenced_item.name.clone()),
            direction: match direction {
                CallDirection::Incoming => "incoming".to_owned(),
                CallDirection::Outgoing => "outgoing".to_owned(),
            },
            depth: usize::try_from(current_depth).unwrap_or(0),
            confidence: Some("lsp".to_owned()),
        }
    }
    async fn fetch_call_hierarchy(
        &self,
        item: &pathfinder_lsp::types::CallHierarchyItem,
        direction: CallDirection,
    ) -> Result<Vec<pathfinder_lsp::types::CallHierarchyCall>, LspError> {
        match direction {
            CallDirection::Incoming => {
                self.lawyer
                    .call_hierarchy_incoming(self.workspace_root.path(), item)
                    .await
            }
            CallDirection::Outgoing => {
                self.lawyer
                    .call_hierarchy_outgoing(self.workspace_root.path(), item)
                    .await
            }
        }
    }

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

            let hierarchy_result = self.fetch_call_hierarchy(&item, direction).await;

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

                            let reference = self
                                .build_impact_reference(&referenced_item, &direction, current_depth)
                                .await;
                            references.push(reference);
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

    /// PATCH-002: Trait Method Resolution.
    ///
    /// If `scope` describes a method inside a trait/interface, calls
    /// `goto_implementation` to find all concrete impls, then BFS's each
    /// impl's call hierarchy and merges (dedup by semantic path) the results
    /// into `state`.
    ///
    /// Returns `(used_impl_expansion, impl_count, impl_file_count, degraded_override)`.
    /// `degraded_override` is set when `goto_implementation` itself failed,
    /// so the caller can OR it into the final degraded state even if the
    /// subsequent normal-flow fallback succeeds.
    async fn try_trait_method_expansion(
        &self,
        ctx: &CallerCalleeContext<'_>,
        remaining_incoming: &mut u32,
        remaining_outgoing: &mut u32,
        state: &mut CallHierarchyState,
    ) -> (bool, usize, usize, Option<DegradedReason>) {
        match self
            .lawyer
            .goto_implementation(
                self.workspace_root.path(),
                &ctx.semantic_path.file_path,
                u32::try_from(ctx.scope.start_line + 1).unwrap_or(1),
                u32::try_from(ctx.scope.name_column + 1).unwrap_or(1),
            )
            .await
        {
            Ok(impl_locations) if !impl_locations.is_empty() => {
                state.engines.push("lsp");
                state.degraded = false;
                state.degraded_reason = None;
                let impl_count = impl_locations.len();
                let impl_file_count = impl_locations
                    .iter()
                    .map(|l| l.file.clone())
                    .collect::<std::collections::HashSet<_>>()
                    .len();

                let mut all_incoming = Vec::new();
                let mut all_outgoing = Vec::new();

                for impl_loc in &impl_locations {
                    state.files_referenced.insert(impl_loc.file.clone());

                    let Ok(items) = self
                        .lawyer
                        .call_hierarchy_prepare(
                            self.workspace_root.path(),
                            std::path::Path::new(&impl_loc.file),
                            impl_loc.line,
                            impl_loc.column,
                        )
                        .await
                    else {
                        continue;
                    };
                    let Some(item) = items.first() else {
                        continue;
                    };
                    state.files_referenced.insert(item.file.clone());

                    let (incoming_refs, depth_in) = self
                        .bfs_call_hierarchy(
                            item,
                            CallDirection::Incoming,
                            ctx.max_depth,
                            &mut state.files_referenced,
                            ctx.project_only,
                            remaining_incoming,
                        )
                        .await;
                    all_incoming.extend(incoming_refs);
                    state.max_depth_reached = state.max_depth_reached.max(depth_in);

                    let (outgoing_refs, depth_out) = self
                        .bfs_call_hierarchy(
                            item,
                            CallDirection::Outgoing,
                            ctx.max_depth,
                            &mut state.files_referenced,
                            ctx.project_only,
                            remaining_outgoing,
                        )
                        .await;
                    all_outgoing.extend(outgoing_refs);
                    state.max_depth_reached = state.max_depth_reached.max(depth_out);
                }

                // Dedup by semantic_path to avoid duplicates across multiple impls
                all_incoming.sort_by(|a, b| a.semantic_path.cmp(&b.semantic_path));
                all_incoming.dedup_by(|a, b| a.semantic_path == b.semantic_path);
                all_outgoing.sort_by(|a, b| a.semantic_path.cmp(&b.semantic_path));
                all_outgoing.dedup_by(|a, b| a.semantic_path == b.semantic_path);

                state.incoming = Some(all_incoming);
                state.outgoing = Some(all_outgoing);
                // PATCH-003: BFS ran for each impl and produced the merged
                // results. Mark as BFS-verified so incoming_verified /
                // outgoing_verified reflect this even if `degraded` is later
                // flipped to true by `trait_method_degraded`.
                state.lsp_bfs_ran = true;

                (true, impl_count, impl_file_count, None)
            }
            Ok(_) => {
                // PATCH-002 §B.5: goto_implementation returned no implementations.
                // Trace this and fall through to normal BFS so the agent still gets
                // a result (the LSP may simply not know about any concrete impl).
                tracing::debug!(
                    tool = "find_callers_callees",
                    semantic_path = %ctx.semantic_path,
                    parent = ?ctx.scope.parent_name,
                    "no implementations found for trait method, falling back to normal BFS"
                );
                (false, 0, 0, None)
            }
            Err(e) => {
                // PATCH-002 §B.5: goto_implementation itself failed (e.g. server doesn't
                // support textDocument/implementation, or transient LSP error).
                // Mark the result as degraded so the agent knows the trace is incomplete
                // and still fall through to normal BFS on the trait method itself.
                tracing::warn!(
                    tool = "find_callers_callees",
                    semantic_path = %ctx.semantic_path,
                    error = %e,
                    "goto_implementation failed for trait method, falling back to normal BFS"
                );
                // Store the reason separately; the caller ORs it with the final
                // degraded state to avoid being clobbered by the normal flow's success.
                (false, 0, 0, Some(DegradedReason::LspErrorGrepFallback))
            }
        }
    }

    /// Handles the `Ok(items)` (non-empty) branch of `call_hierarchy_prepare`:
    /// runs BFS in both directions from the first returned item, then — if
    /// both directions come back empty — retries with a grep-based fallback
    /// to guard against false negatives (e.g. a still-partially-indexed LSP).
    async fn handle_call_hierarchy_items(
        &self,
        items: &[pathfinder_lsp::types::CallHierarchyItem],
        ctx: &CallerCalleeContext<'_>,
        remaining_incoming: &mut u32,
        remaining_outgoing: &mut u32,
        state: &mut CallHierarchyState,
    ) {
        state.engines.push("lsp");
        state.degraded = false;
        state.degraded_reason = None;

        let initial_item = &items[0];
        state.files_referenced.insert(initial_item.file.clone());

        // --- INCOMING BFS ---
        let (incoming_refs, depth_in) = self
            .bfs_call_hierarchy(
                initial_item,
                CallDirection::Incoming,
                ctx.max_depth,
                &mut state.files_referenced,
                ctx.project_only,
                remaining_incoming,
            )
            .await;
        state.incoming = Some(incoming_refs);
        state.max_depth_reached = state.max_depth_reached.max(depth_in);

        // --- OUTGOING BFS ---
        let (outgoing_refs, depth_out) = self
            .bfs_call_hierarchy(
                initial_item,
                CallDirection::Outgoing,
                ctx.max_depth,
                &mut state.files_referenced,
                ctx.project_only,
                remaining_outgoing,
            )
            .await;
        state.outgoing = Some(outgoing_refs);
        state.max_depth_reached = state.max_depth_reached.max(depth_out);
        // PATCH-003: track that the BFS actually produced the current
        // incoming/outgoing values, so incoming_verified/outgoing_verified
        // are computed accurately downstream.
        state.lsp_bfs_ran = true;

        // Check for false negatives when BFS call hierarchy traversal returns 0 callers/callees
        if state.incoming.as_ref().is_none_or(Vec::is_empty)
            && state.outgoing.as_ref().is_none_or(Vec::is_empty)
        {
            let (grep_in, grep_out) = self
                .run_grep_fallbacks(
                    &GrepFallbackParams {
                        symbol_name: ctx.symbol_name,
                        definition_path: &ctx.semantic_path.file_path,
                        scope_content: &ctx.scope.content,
                        scope_language: &ctx.scope.language,
                        remaining_outgoing: *remaining_outgoing,
                        project_only: ctx.project_only,
                    },
                    &mut state.files_referenced,
                )
                .await;

            state.degraded = true;
            state.degraded_reason = Some(DegradedReason::LspWarmupGrepFallback);
            if grep_in.is_some() {
                // PATCH-003: BFS data was replaced by heuristic grep
                // results. The flag must reflect this so `incoming_verified`
                // correctly returns false.
                state.lsp_bfs_ran = false;
                state.incoming = grep_in;
            }
            if grep_out.is_some() {
                state.lsp_bfs_ran = false;
                state.outgoing = grep_out;
            }
        }
    }

    /// Handles the `Ok(_)` (empty items) branch of `call_hierarchy_prepare`.
    ///
    /// An empty response is ambiguous: it could mean the symbol genuinely
    /// has zero callers, or that the LSP hasn't finished indexing yet.
    /// Probes `goto_definition` at the same position to disambiguate — a
    /// warm LSP can resolve a well-known symbol's definition; a cold one
    /// returns `None` even for symbols it will eventually index. Either way,
    /// also runs the grep-based fallback to guard against false negatives.
    async fn handle_empty_call_hierarchy(
        &self,
        ctx: &CallerCalleeContext<'_>,
        remaining_outgoing: &mut u32,
        state: &mut CallHierarchyState,
    ) {
        let probe = self
            .lawyer
            .goto_definition(
                self.workspace_root.path(),
                &ctx.semantic_path.file_path,
                u32::try_from(ctx.scope.start_line + 1).unwrap_or(1),
                u32::try_from(ctx.scope.name_column + 1).unwrap_or(1),
            )
            .await;

        if matches!(probe, Ok(Some(_))) {
            // LSP is warm — definition resolved. But let's check for false negatives (indexing incomplete, etc.)
            // by running grep fallback.
            let (grep_in, grep_out) = self
                .run_grep_fallbacks(
                    &GrepFallbackParams {
                        symbol_name: ctx.symbol_name,
                        definition_path: &ctx.semantic_path.file_path,
                        scope_content: &ctx.scope.content,
                        scope_language: &ctx.scope.language,
                        remaining_outgoing: *remaining_outgoing,
                        project_only: ctx.project_only,
                    },
                    &mut state.files_referenced,
                )
                .await;

            state.engines.push("lsp");
            state.degraded = true;
            state.degraded_reason = Some(DegradedReason::LspWarmupGrepFallback);
            state.incoming = grep_in.or(Some(Vec::new()));
            state.outgoing = grep_out.or(Some(Vec::new()));
        } else {
            // LSP likely still warming up — empty call hierarchy is not reliable.
            // Degrade so agents know to verify before acting on "zero references".
            tracing::info!(
                tool = "find_callers_callees",
                symbol = %ctx.semantic_path,
                "find_callers_callees: call_hierarchy_prepare returned [] but goto_definition \
                 probe returned no result — LSP likely warming up, attempting grep-based reference fallback"
            );
            state.engines.push("lsp");
            state.degraded = true;
            state.degraded_reason = Some(DegradedReason::LspWarmupEmptyUnverified);

            let (grep_in, grep_out) = self
                .run_grep_fallbacks(
                    &GrepFallbackParams {
                        symbol_name: ctx.symbol_name,
                        definition_path: &ctx.semantic_path.file_path,
                        scope_content: &ctx.scope.content,
                        scope_language: &ctx.scope.language,
                        remaining_outgoing: *remaining_outgoing,
                        project_only: ctx.project_only,
                    },
                    &mut state.files_referenced,
                )
                .await;

            Self::process_grep_fallback_results(
                grep_in,
                grep_out,
                &mut state.incoming,
                &mut state.outgoing,
                &mut state.degraded_reason,
                Some(DegradedReason::LspWarmupGrepFallback),
                " during LSP warmup",
            );
        }
    }

    /// Shared grep-fallback + result-processing step used by the
    /// `NoLspAvailable`, `Timeout`, and generic-error arms of
    /// [`Self::run_normal_call_hierarchy_flow`].
    async fn grep_fallback_for_callers(
        &self,
        ctx: &CallerCalleeContext<'_>,
        remaining_outgoing: &mut u32,
        state: &mut CallHierarchyState,
        fallback_reason: Option<DegradedReason>,
        log_suffix: &str,
    ) {
        let (grep_in, grep_out) = self
            .run_grep_fallbacks(
                &GrepFallbackParams {
                    symbol_name: ctx.symbol_name,
                    definition_path: &ctx.semantic_path.file_path,
                    scope_content: &ctx.scope.content,
                    scope_language: &ctx.scope.language,
                    remaining_outgoing: *remaining_outgoing,
                    project_only: ctx.project_only,
                },
                &mut state.files_referenced,
            )
            .await;

        Self::process_grep_fallback_results(
            grep_in,
            grep_out,
            &mut state.incoming,
            &mut state.outgoing,
            &mut state.degraded_reason,
            fallback_reason,
            log_suffix,
        );
    }

    /// Handles `LspError::NoLspAvailable` / `UnsupportedCapability`: degraded
    /// mode — uses grep-based reference search as a heuristic fallback.
    async fn handle_no_lsp_for_callers(
        &self,
        ctx: &CallerCalleeContext<'_>,
        remaining_outgoing: &mut u32,
        state: &mut CallHierarchyState,
    ) {
        tracing::info!(
            tool = "find_callers_callees",
            symbol = %ctx.semantic_path,
            "find_callers_callees: no LSP — attempting grep-based reference fallback"
        );
        self.grep_fallback_for_callers(
            ctx,
            remaining_outgoing,
            state,
            Some(DegradedReason::NoLspGrepFallback),
            "",
        )
        .await;
    }

    /// Handles `LspError::Timeout`: attempts a grep-based reference fallback.
    async fn handle_lsp_timeout_for_callers(
        &self,
        ctx: &CallerCalleeContext<'_>,
        remaining_outgoing: &mut u32,
        state: &mut CallHierarchyState,
    ) {
        // Set reason unconditionally: timeout is always the cause, whether or not
        // grep succeeds. Without this, empty grep results would fall through to the
        // initial NoLsp reason, misleading agents into thinking no LSP exists.
        state.degraded_reason = Some(DegradedReason::LspTimeoutGrepFallback);

        tracing::info!(
            tool = "find_callers_callees",
            symbol = %ctx.semantic_path,
            "find_callers_callees: LSP timed out — attempting grep-based reference fallback"
        );
        self.grep_fallback_for_callers(ctx, remaining_outgoing, state, None, " after timeout")
            .await;
    }

    /// Handles any other `LspError` (connection resets, protocol errors,
    /// etc.): not "no LSP" but an operational failure. Attempts a grep-based
    /// fallback before giving up.
    async fn handle_lsp_error_for_callers(
        &self,
        e: LspError,
        ctx: &CallerCalleeContext<'_>,
        remaining_outgoing: &mut u32,
        state: &mut CallHierarchyState,
    ) {
        // Set reason unconditionally: LspErrorGrepFallback describes the cause whether
        // or not grep finds anything. NoLsp would be misleading — the LSP exists but
        // failed, which is a different agent guidance scenario (retry vs. install).
        state.degraded = true;
        state.degraded_reason = Some(DegradedReason::LspErrorGrepFallback);

        tracing::warn!(
            tool = "find_callers_callees",
            error = %e,
            "call_hierarchy_prepare failed"
        );
        self.grep_fallback_for_callers(ctx, remaining_outgoing, state, None, " after LSP error")
            .await;
    }

    /// Runs the "normal" (non-trait-expansion) call-hierarchy resolution
    /// flow: queries `call_hierarchy_prepare` and dispatches to the
    /// appropriate handler based on the outcome (items found, empty, or one
    /// of the LSP error variants), each of which may fall back to
    /// grep-based heuristics.
    async fn run_normal_call_hierarchy_flow(
        &self,
        ctx: &CallerCalleeContext<'_>,
        remaining_incoming: &mut u32,
        remaining_outgoing: &mut u32,
        state: &mut CallHierarchyState,
    ) {
        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &ctx.semantic_path.file_path,
                // Position cursor on the symbol's name identifier (e.g., the 'd' in 'dedent'),
                // not the 'pub' keyword. rust-analyzer requires this for symbol resolution.
                u32::try_from(ctx.scope.start_line + 1).unwrap_or(1),
                u32::try_from(ctx.scope.name_column + 1).unwrap_or(1),
            )
            .await;

        match lsp_result {
            Ok(items) if !items.is_empty() => {
                self.handle_call_hierarchy_items(
                    &items,
                    ctx,
                    remaining_incoming,
                    remaining_outgoing,
                    state,
                )
                .await;
            }
            Ok(_) => {
                self.handle_empty_call_hierarchy(ctx, remaining_outgoing, state)
                    .await;
            }
            Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }) => {
                self.handle_no_lsp_for_callers(ctx, remaining_outgoing, state)
                    .await;
            }
            Err(LspError::Timeout { .. }) => {
                self.handle_lsp_timeout_for_callers(ctx, remaining_outgoing, state)
                    .await;
            }
            Err(e) => {
                self.handle_lsp_error_for_callers(e, ctx, remaining_outgoing, state)
                    .await;
            }
        }
    }

    /// Derives `(lsp_readiness, warm_start_in_progress)` from the final
    /// degraded state.
    fn compute_lsp_readiness(
        degraded: bool,
        degraded_reason: Option<DegradedReason>,
    ) -> (Option<String>, Option<bool>) {
        let lsp_readiness = if degraded {
            match degraded_reason {
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
        (lsp_readiness, warm_start_in_progress)
    }

    /// Derives the `resolution_strategy` field describing which engine
    /// ultimately produced the results.
    fn compute_resolution_strategy(
        used_impl_expansion: bool,
        engines: &[&str],
        degraded: bool,
        degraded_reason: Option<&DegradedReason>,
    ) -> String {
        if used_impl_expansion {
            return "lsp_call_hierarchy_with_impl_expansion".to_owned();
        }
        if engines.contains(&"lsp") {
            // PATCH-002 Fix 2: When degraded is true due ONLY to a trait method error
            // (goto_implementation failed) but the normal LSP call_hierarchy flow still ran,
            // we used LSP for the actual results. Don't claim "grep_file_scoped".
            return "lsp_call_hierarchy".to_owned();
        }
        if degraded {
            // Check which grep fallback was used based on degraded_reason
            return match degraded_reason {
                Some(
                    DegradedReason::LspWarmupGrepFallback
                    | DegradedReason::LspTimeoutGrepFallback
                    | DegradedReason::LspErrorGrepFallback
                    | DegradedReason::NoLspGrepFallback,
                ) => "grep_file_scoped".to_owned(),
                _ => "treesitter_fallback".to_owned(),
            };
        }
        "treesitter_direct".to_owned()
    }

    /// Derives the per-field `(incoming_verified, outgoing_verified)` flags.
    ///
    /// Semantics:
    /// - `Some(true)`  = list is verified by the LSP (BFS returned data).
    /// - `Some(false)` = list is UNKNOWN or heuristic (either `None` with no
    ///   heuristic fallback, or heuristic grep results that may include
    ///   false positives).
    /// - `None`        = field not applicable for this scope.
    ///
    /// `lsp_bfs_ran` distinguishes "BFS ran and confirmed empty" from "grep
    /// fallback produced candidates" — both leave the field as `Some(vec)`
    /// but only the first is LSP-verified.
    fn compute_verified_flags(
        incoming: Option<&Vec<crate::server::types::ImpactReference>>,
        outgoing: Option<&Vec<crate::server::types::ImpactReference>>,
        degraded: bool,
        lsp_bfs_ran: bool,
    ) -> (Option<bool>, Option<bool>) {
        let verify = |field: Option<&Vec<crate::server::types::ImpactReference>>| match (
            field,
            degraded,
            lsp_bfs_ran,
        ) {
            (Some(_), false, _) | (Some(_), true, true) => Some(true),
            (Some(_), true, false) | (None, true, _) => Some(false),
            (None, false, _) => None,
        };
        (verify(incoming), verify(outgoing))
    }

    /// Generates the agent-facing `hint` field.
    ///
    /// P2-7: hints when zero incoming callers and not degraded (possible
    /// entry point/unused code/dynamic dispatch). PATCH-002 §B.4: hints when
    /// impl expansion was used. PATCH-003 Deliverable A: spells out the
    /// "UNKNOWN, not zero" warning explicitly for prose-reading consumers
    /// that ignore the per-field verified flags.
    ///
    /// NOTE: The "both incoming AND outgoing empty" case is always degraded
    /// (the grep fallback sets `degraded=true` when both BFS results are
    /// empty, to guard against false negatives). So we only hint on
    /// "incoming empty, outgoing non-empty" here.
    fn compute_callers_callees_hint(params: &HintParams<'_>) -> Option<String> {
        let unknown_search_hint = if params.symbol_name.is_empty() {
            "Use search(query='<symbol>') to find usages manually.".to_owned()
        } else {
            format!(
                "Use search(query=\"{}\") to find usages manually.",
                params.symbol_name
            )
        };
        if params.used_impl_expansion {
            return Some(format!(
                "Symbol is a trait/interface method. Results include callers of all \
                 implementations found via goto_implementation. \
                 {} implementation(s) found across {} file(s).",
                params.trait_impl_count, params.trait_impl_file_count
            ));
        }
        if params.degraded && params.incoming.is_none() && params.outgoing.is_none() {
            // Catastrophic footgun: both fields UNKNOWN. Spell it out.
            return Some(format!(
                "Callers AND callees are UNKNOWN — LSP unavailable and no heuristic \
                 fallback matched. Do NOT treat null as zero callers/callees. \
                 {unknown_search_hint}"
            ));
        }
        if params.degraded && params.incoming.is_none() {
            return Some(format!(
                "Callers UNKNOWN — LSP unavailable or warming up. \
                 Do NOT treat null as zero callers. \
                 {unknown_search_hint}"
            ));
        }
        if params.degraded && params.outgoing.is_none() {
            return Some(format!(
                "Callees UNKNOWN — LSP unavailable or warming up. \
                 Do NOT treat null as zero callees. \
                 {unknown_search_hint}"
            ));
        }
        if params.degraded {
            // Heuristic results — degraded but we have candidate refs.
            return Some(format!(
                "Results are heuristic grep-based candidates (LSP unavailable). \
                 Each reference may be a false positive (dynamic dispatch, \
                 same-name symbols). {unknown_search_hint}"
            ));
        }
        if params.incoming.as_ref().is_some_and(Vec::is_empty) {
            return Some(
                "LSP confirmed zero incoming callers. This symbol may be an entry point, \
                 unused, or only called via dynamic dispatch/reflection."
                    .to_owned(),
            );
        }
        None
    }

    /// Builds the honest, human-readable text summary listing every
    /// reference so agents can act without parsing `structured_content`.
    fn render_callers_callees_text(params: &RenderCallersCalleesParams<'_>) -> String {
        let mut text_parts = Vec::new();
        if params.degraded {
            let notice = params
                .degraded_reason
                .as_ref()
                .map_or_else(|| "DEGRADED (unknown)".to_owned(), format_degraded_notice);

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
            if params.symbol_name.is_empty() {
                text_parts.push("   Workaround: Use search to find usages manually.".to_owned());
            } else {
                text_parts.push(format!(
                    "   Workaround: Use search(query=\"{}\") to find usages manually.",
                    params.symbol_name
                ));
            }
            text_parts.push("   Reference counts below are heuristic only:".to_owned());
            text_parts.push(String::new());
        } else if params.inc_count == 0 && params.out_count == 0 {
            text_parts.push("LSP confirmed: zero callers/callees for this symbol.".to_string());
        } else if params.inc_count == 0 {
            text_parts
                .push("LSP confirmed: zero incoming callers (callees found below).".to_string());
        } else if params.out_count == 0 {
            text_parts
                .push("LSP confirmed: zero outgoing callees (callers found below).".to_string());
        }
        // Incoming
        text_parts.push(format!("Incoming references: {}", params.inc_count));
        if let Some(refs) = &params.metadata.incoming {
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
        text_parts.push(format!("Outgoing references: {}", params.out_count));
        if let Some(refs) = &params.metadata.outgoing {
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
        if let Some(test_refs) = &params.metadata.test_callers {
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
        } else if let Some(status) = &params.metadata.test_coverage_status {
            if status == "not_found" {
                text_parts.push(String::new());
                text_parts
                    .push("TEST COVERAGE: no test functions found for this symbol".to_owned());
            } else if status == "unknown_degraded" {
                text_parts.push(String::new());
                text_parts.push("TEST COVERAGE: unknown (search degraded)".to_owned());
            }
        }

        text_parts.push(format!("[completed in {}ms]", params.duration_ms));
        text_parts.join("\n")
    }

    /// Parses/validates `semantic_path_str`, enforces the sandbox and
    /// file-existence checks, and resolves the symbol's scope via
    /// Tree-sitter (enriched with did-you-mean / separator-correction
    /// retries via [`Self::read_symbol_scope_enriched`]).
    ///
    /// Returns the parsed `SemanticPath`, its resolved `symbol_name`, its
    /// `SymbolScope`, and how long the Tree-sitter resolution took (in ms).
    async fn locate_symbol_for_callers_callees(
        &self,
        semantic_path_str: &str,
        start: std::time::Instant,
    ) -> Result<
        (
            pathfinder_common::types::SemanticPath,
            String,
            pathfinder_common::types::SymbolScope,
            u128,
        ),
        ErrorData,
    > {
        let semantic_path = parse_semantic_path(semantic_path_str)?;
        require_symbol_target(&semantic_path, semantic_path_str)?;
        let symbol_name = super::last_symbol_name(&semantic_path).unwrap_or_default();

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
            .read_symbol_scope_enriched(&semantic_path, semantic_path_str)
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

        Ok((semantic_path, symbol_name, scope, tree_sitter_ms))
    }

    /// Opens `semantic_path`'s file in the LSP's document buffer via an RAII
    /// guard, so `did_close` fires automatically when the guard drops (see
    /// [`PathfinderServer::open_document_for_definition`] for the full
    /// rationale). Returns `None` (degraded) when the file can't be read or
    /// the LSP rejects the open.
    async fn open_document_for_callers_callees(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
    ) -> Option<Box<dyn pathfinder_lsp::lawyer::DocumentLease>> {
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
        match self
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
        }
    }

    /// Runs trait-method impl expansion (PATCH-002), when applicable,
    /// followed by the normal call-hierarchy flow, mutating `state` with the
    /// resolved incoming/outgoing references.
    ///
    /// Returns `(used_impl_expansion, trait_impl_count, trait_impl_file_count,
    /// trait_method_degraded)`. `trait_method_degraded` is set when
    /// `goto_implementation` itself failed, and is OR'd into the final
    /// degraded state by [`Self::finalize_call_hierarchy_result`] even if the
    /// normal-flow fallback subsequently succeeds.
    async fn run_call_hierarchy_resolution(
        &self,
        ctx: &CallerCalleeContext<'_>,
        remaining_incoming: &mut u32,
        remaining_outgoing: &mut u32,
        state: &mut CallHierarchyState,
    ) -> (bool, usize, usize, Option<DegradedReason>) {
        // If the symbol is a method inside a trait/interface (parent_kind =
        // "interface"), call goto_implementation first to find all concrete
        // impls, then BFS each impl's call hierarchy.
        let is_trait_method = ctx.scope.parent_kind.as_deref() == Some("interface");
        let mut used_impl_expansion = false;
        let mut trait_impl_count = 0;
        let mut trait_impl_file_count = 0;
        let mut trait_method_degraded = None;

        if is_trait_method {
            let (expanded, impl_count, impl_file_count, degraded_override) = self
                .try_trait_method_expansion(ctx, remaining_incoming, remaining_outgoing, state)
                .await;
            used_impl_expansion = expanded;
            trait_impl_count = impl_count;
            trait_impl_file_count = impl_file_count;
            trait_method_degraded = degraded_override;
        }

        // Normal flow (skipped only if used_impl_expansion is true)
        if !used_impl_expansion {
            self.run_normal_call_hierarchy_flow(ctx, remaining_incoming, remaining_outgoing, state)
                .await;
        }

        (
            used_impl_expansion,
            trait_impl_count,
            trait_impl_file_count,
            trait_method_degraded,
        )
    }

    /// Assembles the final [`crate::server::types::FindCallersCalleesMetadata`]
    /// and human-readable text summary from the accumulated resolution state.
    fn finalize_call_hierarchy_result(
        params: FinalizeCallHierarchyParams,
    ) -> (crate::server::types::FindCallersCalleesMetadata, String) {
        let FinalizeCallHierarchyParams {
            state,
            trait_method_degraded,
            used_impl_expansion,
            trait_impl_count,
            trait_impl_file_count,
            symbol_name,
            requested_semantic_path,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            max_references,
        } = params;

        let CallHierarchyState {
            incoming,
            outgoing,
            degraded,
            degraded_reason,
            engines,
            files_referenced,
            max_depth_reached,
            lsp_bfs_ran,
        } = state;

        let inc_count = incoming.as_ref().map_or(0, Vec::len);
        let out_count = outgoing.as_ref().map_or(0, Vec::len);
        // PATCH-002 Fix 2: If goto_implementation errored, the normal flow's success
        // path may have reset `degraded` to false. Re-apply the trait-method error
        // to ensure the agent sees a partial result warning.
        let degraded_reason = trait_method_degraded.or(degraded_reason);
        let degraded = degraded || trait_method_degraded.is_some();
        let degraded_reason_cloned = degraded_reason;
        let degraded_reason_str = degraded_reason.as_ref().map(ToString::to_string);

        let (lsp_readiness, warm_start_in_progress) =
            Self::compute_lsp_readiness(degraded, degraded_reason_cloned);

        tracing::info!(
            tool = "find_callers_callees",
            semantic_path = %requested_semantic_path,
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

        let resolution_strategy = Some(Self::compute_resolution_strategy(
            used_impl_expansion,
            &engines,
            degraded,
            degraded_reason.as_ref(),
        ));

        // Spec 4.2: Test coverage search (deactivated, keeping fields None for backwards compatibility)
        let (test_callers, test_coverage_status) = (None, None);

        // P2-7 / PATCH-002 §B.4 / PATCH-003 Deliverable A: see
        // `compute_callers_callees_hint` doc comment for the hint policy.
        let hint = Self::compute_callers_callees_hint(&HintParams {
            used_impl_expansion,
            trait_impl_count,
            trait_impl_file_count,
            degraded,
            incoming: &incoming,
            outgoing: &outgoing,
            symbol_name: &symbol_name,
        });

        // PATCH-003 DELIVERABLE B: Per-field verification flags.
        let (incoming_verified, outgoing_verified) = Self::compute_verified_flags(
            incoming.as_ref(),
            outgoing.as_ref(),
            degraded,
            lsp_bfs_ran,
        );

        let metadata = crate::server::types::FindCallersCalleesMetadata {
            incoming,
            outgoing,
            incoming_verified,
            outgoing_verified,
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
        let text = Self::render_callers_callees_text(&RenderCallersCalleesParams {
            degraded,
            degraded_reason: degraded_reason_cloned,
            symbol_name: &symbol_name,
            inc_count,
            out_count,
            metadata: &metadata,
            duration_ms,
        });

        (metadata, text)
    }

    /// Core logic for the `find_callers_callees` tool.
    ///
    /// Returns callers (incoming) and callees (outgoing) for the target symbol.
    /// Degrades gracefully to empty results when no LSP is configured.
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

        let (semantic_path, symbol_name, scope, tree_sitter_ms) = self
            .locate_symbol_for_callers_callees(&params.semantic_path, start)
            .await?;

        // `_doc_guard` fires did_close automatically when this function returns.
        let _doc_guard = self.open_document_for_callers_callees(&semantic_path).await;

        let lsp_start = std::time::Instant::now();
        // Use Option<Vec> to distinguish "unknown" (LSP unavailable) from "verified empty" (LSP confirmed zero).
        // None = degraded (LSP was down — callers are unknown, do NOT treat as zero)
        // Some([]) = LSP responded with confirmed zero callers/callees
        let mut state = CallHierarchyState::new();

        let ctx = CallerCalleeContext {
            semantic_path: &semantic_path,
            scope: &scope,
            symbol_name: &symbol_name,
            max_depth,
            project_only,
        };

        let (used_impl_expansion, trait_impl_count, trait_impl_file_count, trait_method_degraded) =
            self.run_call_hierarchy_resolution(
                &ctx,
                &mut remaining_incoming,
                &mut remaining_outgoing,
                &mut state,
            )
            .await;

        // Note: `_doc_guard` still alive here; did_close fires at function return.
        let lsp_ms = lsp_start.elapsed().as_millis();
        let duration_ms = start.elapsed().as_millis();

        let (metadata, text) = Self::finalize_call_hierarchy_result(FinalizeCallHierarchyParams {
            state,
            trait_method_degraded,
            used_impl_expansion,
            trait_impl_count,
            trait_impl_file_count,
            symbol_name,
            requested_semantic_path: params.semantic_path,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            max_references,
        });

        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "impact_test.rs"]
mod tests;
