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
#[derive(Default)]
struct VerificationStatus {
    incoming: bool,
    outgoing: bool,
}

struct CallHierarchyDegradedStatus {
    degraded: bool,
    degraded_reason: Option<DegradedReason>,
    lsp_readiness: Option<String>,
    warm_start_in_progress: Option<bool>,
    status: pathfinder_common::types::ToolStatus,
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
    verification: VerificationStatus,
    references_truncated: bool,
    resolution_block: Option<String>,
    /// Set when `ensure_ready` reported `Readiness::Unavailable` for this
    /// language *before* any call-hierarchy resolution was attempted --
    /// `(language, install_hint, alternative)`. Distinct from `degraded`:
    /// this is a terminal, config-level fact ("no LSP binary on PATH"), not
    /// a transient failure, and surfaces as `ToolStatus::Unavailable`
    /// (which tells the agent not to retry) rather than `ToolStatus::
    /// Degraded` (which implies retrying might help).
    unavailable: Option<(String, String, String)>,
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
            verification: VerificationStatus::default(),
            references_truncated: false,
            resolution_block: None,
            unavailable: None,
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
    /// LSP language id for `semantic_path.file_path`, used to look up the
    /// negotiated position encoding (TS-A2 / LSP-A5) before every LSP
    /// handoff below.
    language_id: &'a str,
}

/// Parameters for [`PathfinderServer::compute_callers_callees_hint`].
struct HintParams<'a> {
    used_impl_expansion: bool,
    trait_impl_count: usize,
    trait_impl_file_count: usize,
    degraded: bool,
    incoming: &'a Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>>,
    outgoing: &'a Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>>,
}

/// Parameters for [`PathfinderServer::finalize_call_hierarchy_result`].
struct FinalizeCallHierarchyParams {
    state: CallHierarchyState,
    trait_method_degraded: Option<DegradedReason>,
    used_impl_expansion: bool,
    trait_impl_count: usize,
    trait_impl_file_count: usize,
    requested_semantic_path: String,
    tree_sitter_ms: u128,
    lsp_ms: u128,
    duration_ms: u128,
    max_references: u32,
    offset: u32,
    retry_attempt: u32,
}

/// Parameters for [`PathfinderServer::render_callers_callees_text`].
struct RenderCallersCalleesParams<'a> {
    degraded: bool,
    degraded_reason: Option<DegradedReason>,
    inc_count: usize,
    out_count: usize,
    total_items: usize,
    resolution_block: Option<String>,
    metadata: &'a crate::server::types::FindCallersCalleesMetadata,
    duration_ms: u128,
}

struct GrepOutgoingFallbackParams<'a> {
    scope_content: &'a str,
    scope_language: &'a str,
    definition_path: &'a std::path::Path,
    max_results: u32,
    project_only: bool,
    files_referenced: &'a mut std::collections::HashSet<String>,
    budget: &'a super::GrepFallbackBudget,
}

struct ResolveGrepCandidatesParams<'a> {
    candidates: &'a [String],
    scope_language: &'a str,
    result: &'a pathfinder_search::SearchResult,
    definition_path: &'a std::path::Path,
    project_only: bool,
    max_deps: usize,
    files_referenced: &'a mut std::collections::HashSet<String>,
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
    /// DELIVERABLE E: Grep-based reference search fallback for `find_callers_callees`.
    ///
    /// When LSP is unavailable, search workspace for word-boundary matches of `symbol_name`.
    ///
    /// Returns `Some(refs)` if references found, `None` if none found.
    /// Updates `files_referenced` with the files containing matches.
    async fn grep_reference_fallback(
        &self,
        symbol_name: &str,
        definition_path: &std::path::Path,
        files_referenced: &mut std::collections::HashSet<String>,
        budget: &super::GrepFallbackBudget,
    ) -> Option<Vec<crate::server::types::ImpactReference>> {
        let deadline = budget.acquire_search()?;

        let search_params = pathfinder_search::SearchParams {
            workspace_root: self.workspace_root.path().to_path_buf(),
            query: format!("\\b{}\\b", regex::escape(symbol_name)),
            is_regex: true,
            path_glob: "**/*".to_string(),
            max_results: 20,
            offset: 0,
            context_lines: 0,
            exclude_glob: vec![],
            deadline: Some(deadline),
        };

        let result = match self
            .grep_candidates(
                &search_params,
                pathfinder_common::types::FilterMode::CodeOnly,
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    tool = "grep_reference_fallback",
                    symbol = %symbol_name,
                    error = %e,
                    "grep_candidates failed during grep fallback"
                );
                return None;
            }
        };

        if result.is_empty() {
            return None;
        }

        let refs: Vec<crate::server::types::ImpactReference> = result
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
                    confidence: pathfinder_common::types::Confidence::Heuristic,
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
    /// and resolve candidates to their definitions using batch alternation grep search (RSP-A10).
    async fn grep_outgoing_fallback(
        &self,
        params: GrepOutgoingFallbackParams<'_>,
    ) -> Option<Vec<crate::server::types::ImpactReference>> {
        let GrepOutgoingFallbackParams {
            scope_content,
            scope_language,
            definition_path,
            max_results,
            project_only,
            files_referenced,
            budget,
        } = params;
        let candidates = super::extract_call_candidates(scope_content, scope_language);
        if candidates.is_empty() {
            return None;
        }

        let deadline = budget.acquire_search()?;
        let max_deps = usize::try_from(max_results).unwrap_or(usize::MAX);

        let alt = candidates
            .iter()
            .map(|c| regex::escape(c))
            .collect::<Vec<_>>()
            .join("|");
        let pattern = super::candidate_definition_pattern(scope_language, &alt);
        let path_glob = super::language_to_file_glob(scope_language);

        let search_res = self
            .scout
            .search(&pathfinder_search::SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: pattern,
                is_regex: true,
                max_results: max_deps.max(50),
                path_glob,
                exclude_glob: Vec::new(),
                context_lines: 0,
                offset: 0,
                deadline: Some(deadline),
            })
            .await;

        let refs = if let Ok(result) = search_res {
            self.resolve_grep_candidates(ResolveGrepCandidatesParams {
                candidates: &candidates,
                scope_language,
                result: &result,
                definition_path,
                project_only,
                max_deps,
                files_referenced,
            })
            .await
        } else {
            Vec::new()
        };

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

    async fn resolve_grep_candidates(
        &self,
        params: ResolveGrepCandidatesParams<'_>,
    ) -> Vec<crate::server::types::ImpactReference> {
        let ResolveGrepCandidatesParams {
            candidates,
            scope_language,
            result,
            definition_path,
            project_only,
            max_deps,
            files_referenced,
        } = params;
        let mut refs = Vec::new();
        let mut seen = std::collections::HashSet::new();

        for candidate in candidates {
            if refs.len() >= max_deps {
                break;
            }

            let cand_pat = super::candidate_definition_pattern(scope_language, candidate);
            let cand_re = regex::Regex::new(&cand_pat).ok();

            for m in &result.matches {
                if refs.len() >= max_deps {
                    break;
                }

                if !cand_re.as_ref().is_some_and(|re| re.is_match(&m.content)) {
                    continue;
                }

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
                    .enrich_semantic_path(&m.file, u32::try_from(m.line).unwrap_or(0), candidate)
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
                    confidence: pathfinder_common::types::Confidence::Heuristic,
                });
                break;
            }
        }
        refs
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
        let budget = super::GrepFallbackBudget::new();
        let incoming = self
            .grep_reference_fallback(
                params.symbol_name,
                params.definition_path,
                files_referenced,
                &budget,
            )
            .await;

        let outgoing = self
            .grep_outgoing_fallback(GrepOutgoingFallbackParams {
                scope_content: params.scope_content,
                scope_language: params.scope_language,
                definition_path: params.definition_path,
                max_results: params.remaining_outgoing,
                project_only: params.project_only,
                files_referenced,
                budget: &budget,
            })
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
            snippet: String::new(),
            direction: match direction {
                CallDirection::Incoming => "incoming".to_owned(),
                CallDirection::Outgoing => "outgoing".to_owned(),
            },
            depth: usize::try_from(current_depth).unwrap_or(0),
            confidence: pathfinder_common::types::Confidence::Verified,
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
}

struct BfsTraverseState<'a> {
    files_referenced: &'a mut std::collections::HashSet<String>,
    project_only: bool,
    remaining_references: &'a mut u32,
    truncated: &'a mut bool,
}

struct CallHierarchyPagination {
    paginated_inc:
        Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>>,
    paginated_out:
        Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>>,
    inc_count: usize,
    out_count: usize,
    total_items: usize,
    truncated: bool,
    next_offset: Option<u32>,
}

struct CallersCalleesMetaParams<'a> {
    status: pathfinder_common::types::ToolStatus,
    pagination: &'a CallHierarchyPagination,
    max_depth_reached: u32,
    files_referenced_len: usize,
    degraded: bool,
    degraded_reason: Option<DegradedReason>,
    lsp_readiness: Option<String>,
    warm_start_in_progress: Option<bool>,
    resolution_strategy: Option<String>,
    duration_ms: u128,
    hint: Option<String>,
}

impl PathfinderServer {
    async fn bfs_call_hierarchy(
        &self,
        initial_item: &pathfinder_lsp::types::CallHierarchyItem,
        direction: CallDirection,
        max_depth: u32,
        state: &mut BfsTraverseState<'_>,
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
        state.files_referenced.insert(initial_item.file.clone());

        let mut references = Vec::new();
        let mut max_depth_reached = 0;
        let mut consecutive_failures: u32 = 0;

        while let Some((item, current_depth)) = queue.pop_front() {
            max_depth_reached = std::cmp::max(max_depth_reached, current_depth);
            if current_depth >= max_depth {
                continue;
            }
            if *state.remaining_references == 0 {
                *state.truncated = true;
                break;
            }

            // Check wall-clock timeout
            if tokio::time::Instant::now() >= deadline {
                tracing::warn!(
                    direction = ?direction,
                    timeout_secs = BFS_TIMEOUT_SECS,
                    "BFS traversal exceeded wall-clock timeout, returning partial results"
                );
                *state.truncated = true;
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
                *state.truncated = true;
                break;
            }

            let hierarchy_result = self.fetch_call_hierarchy(&item, direction).await;

            match hierarchy_result {
                Ok(calls) => {
                    consecutive_failures = 0;
                    for call in calls {
                        if *state.remaining_references == 0 {
                            *state.truncated = true;
                            break;
                        }

                        let referenced_item = call.item;

                        // Filter out non-workspace files when project_only:
                        // - Must have a source code extension
                        // - Must be a relative path (not absolute like stdlib/SDK paths)
                        // - Must not be in node_modules/ or vendor/
                        if state.project_only
                            && (!super::is_source_file(&referenced_item.file)
                                || !super::is_workspace_file(&referenced_item.file))
                        {
                            continue;
                        }

                        state.files_referenced.insert(referenced_item.file.clone());

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
                            *state.remaining_references -= 1;
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
    async fn traverse_trait_impl_locations(
        &self,
        ctx: &CallerCalleeContext<'_>,
        impl_locations: &[pathfinder_lsp::types::DefinitionLocation],
        remaining_incoming: &mut u32,
        remaining_outgoing: &mut u32,
        state: &mut CallHierarchyState,
    ) -> (
        Vec<crate::server::types::ImpactReference>,
        Vec<crate::server::types::ImpactReference>,
    ) {
        let mut all_incoming = Vec::new();
        let mut all_outgoing = Vec::new();

        for impl_loc in impl_locations {
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

            let mut in_state = BfsTraverseState {
                files_referenced: &mut state.files_referenced,
                project_only: ctx.project_only,
                remaining_references: remaining_incoming,
                truncated: &mut state.references_truncated,
            };
            let (incoming_refs, depth_in) = self
                .bfs_call_hierarchy(item, CallDirection::Incoming, ctx.max_depth, &mut in_state)
                .await;
            all_incoming.extend(incoming_refs);
            state.max_depth_reached = state.max_depth_reached.max(depth_in);

            let mut out_state = BfsTraverseState {
                files_referenced: &mut state.files_referenced,
                project_only: ctx.project_only,
                remaining_references: remaining_outgoing,
                truncated: &mut state.references_truncated,
            };
            let (outgoing_refs, depth_out) = self
                .bfs_call_hierarchy(item, CallDirection::Outgoing, ctx.max_depth, &mut out_state)
                .await;
            all_outgoing.extend(outgoing_refs);
            state.max_depth_reached = state.max_depth_reached.max(depth_out);
        }

        all_incoming.sort_by(|a, b| a.semantic_path.cmp(&b.semantic_path));
        all_incoming.dedup_by(|a, b| a.semantic_path == b.semantic_path);
        all_outgoing.sort_by(|a, b| a.semantic_path.cmp(&b.semantic_path));
        all_outgoing.dedup_by(|a, b| a.semantic_path == b.semantic_path);

        (all_incoming, all_outgoing)
    }

    fn format_trait_resolution_block(
        ctx: &CallerCalleeContext<'_>,
        impl_locations: &[pathfinder_lsp::types::DefinitionLocation],
    ) -> String {
        let queried = if let Some(parent) = &ctx.scope.parent_name {
            if ctx.symbol_name == parent {
                parent.clone()
            } else {
                format!("{parent}.{}", ctx.symbol_name)
            }
        } else {
            ctx.symbol_name.to_string()
        };
        let declared = format!(
            "{}:{}",
            ctx.semantic_path.file_path.display(),
            ctx.scope.name_line + 1
        );
        let mut res_lines = vec![
            "resolution:".to_string(),
            format!("  queried:  {queried}"),
            format!("  declared: {declared}"),
        ];
        for impl_loc in impl_locations {
            res_lines.push(format!(
                "  impl:     {}:{}   <- the runtime type",
                impl_loc.file, impl_loc.line
            ));
        }
        res_lines.push(
            "  note:     name-matching tools commonly return the declaration site here".to_string(),
        );
        res_lines.join("\n")
    }

    async fn try_trait_method_expansion(
        &self,
        ctx: &CallerCalleeContext<'_>,
        remaining_incoming: &mut u32,
        remaining_outgoing: &mut u32,
        state: &mut CallHierarchyState,
    ) -> (bool, usize, usize, Option<DegradedReason>) {
        let (position_line, position_char) = self
            .compute_lsp_position(
                &ctx.semantic_path.file_path,
                ctx.scope.name_line,
                ctx.scope.name_column,
                ctx.language_id,
            )
            .await;
        match self
            .lawyer
            .goto_implementation(
                self.workspace_root.path(),
                &ctx.semantic_path.file_path,
                position_line,
                position_char,
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

                let (all_incoming, all_outgoing) = self
                    .traverse_trait_impl_locations(
                        ctx,
                        &impl_locations,
                        remaining_incoming,
                        remaining_outgoing,
                        state,
                    )
                    .await;

                state.incoming = Some(all_incoming);
                state.outgoing = Some(all_outgoing);
                state.verification.incoming = true;
                state.verification.outgoing = true;
                state.resolution_block =
                    Some(Self::format_trait_resolution_block(ctx, &impl_locations));

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
    /// executes incoming and outgoing BFS traversals and runs grep fallbacks
    /// if both return empty.
    async fn handle_call_hierarchy_items(
        &self,
        items: &[pathfinder_lsp::types::CallHierarchyItem],
        ctx: &CallerCalleeContext<'_>,
        remaining_incoming: &mut u32,
        remaining_outgoing: &mut u32,
        state: &mut CallHierarchyState,
    ) {
        let initial_item = &items[0];
        state.engines.push("lsp");
        state.degraded = false;
        state.degraded_reason = None;

        let mut in_state = BfsTraverseState {
            files_referenced: &mut state.files_referenced,
            project_only: ctx.project_only,
            remaining_references: remaining_incoming,
            truncated: &mut state.references_truncated,
        };
        let (incoming_refs, depth_in) = self
            .bfs_call_hierarchy(
                initial_item,
                CallDirection::Incoming,
                ctx.max_depth,
                &mut in_state,
            )
            .await;
        state.incoming = Some(incoming_refs);
        state.max_depth_reached = depth_in;
        state.verification.incoming = true;

        let mut out_state = BfsTraverseState {
            files_referenced: &mut state.files_referenced,
            project_only: ctx.project_only,
            remaining_references: remaining_outgoing,
            truncated: &mut state.references_truncated,
        };
        let (outgoing_refs, depth_out) = self
            .bfs_call_hierarchy(
                initial_item,
                CallDirection::Outgoing,
                ctx.max_depth,
                &mut out_state,
            )
            .await;
        state.outgoing = Some(outgoing_refs);
        state.max_depth_reached = state.max_depth_reached.max(depth_out);
        state.verification.outgoing = true;

        // LSP-A1: check for false negatives whenever EITHER direction's BFS
        // traversal returned empty — not only when both are. Outgoing calls
        // resolve from the file-local syntax tree; incoming calls need the
        // global index, so a mid-index server routinely yields
        // `incoming=[]` while `outgoing` is already populated. The old
        // both-empty-only gate never re-checked that lone empty direction,
        // so it shipped as a BFS-verified "confirmed zero" without ever
        // being cross-checked. Each direction is re-verified independently:
        // a direction that already has real BFS results is never re-queried
        // or overwritten.
        let incoming_empty = state.incoming.as_ref().is_none_or(Vec::is_empty);
        let outgoing_empty = state.outgoing.as_ref().is_none_or(Vec::is_empty);

        if incoming_empty || outgoing_empty {
            let budget = super::GrepFallbackBudget::new();
            let mut any_grep_found = false;

            if incoming_empty {
                let grep_in = self
                    .grep_reference_fallback(
                        ctx.symbol_name,
                        &ctx.semantic_path.file_path,
                        &mut state.files_referenced,
                        &budget,
                    )
                    .await;
                if grep_in.is_some() {
                    state.verification.incoming = false;
                    state.incoming = grep_in;
                    any_grep_found = true;
                }
            }

            if outgoing_empty {
                let grep_out = self
                    .grep_outgoing_fallback(GrepOutgoingFallbackParams {
                        scope_content: &ctx.scope.content,
                        scope_language: &ctx.scope.language,
                        definition_path: &ctx.semantic_path.file_path,
                        max_results: *remaining_outgoing,
                        project_only: ctx.project_only,
                        files_referenced: &mut state.files_referenced,
                        budget: &budget,
                    })
                    .await;
                if grep_out.is_some() {
                    state.verification.outgoing = false;
                    state.outgoing = grep_out;
                    any_grep_found = true;
                }
            }

            if any_grep_found {
                state.degraded = true;
                state.degraded_reason = Some(DegradedReason::LspWarmupGrepFallback);
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
        let (position_line, position_char) = self
            .compute_lsp_position(
                &ctx.semantic_path.file_path,
                ctx.scope.name_line,
                ctx.scope.name_column,
                ctx.language_id,
            )
            .await;
        let probe = self
            .lawyer
            .goto_definition(
                self.workspace_root.path(),
                &ctx.semantic_path.file_path,
                position_line,
                position_char,
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
        // Position cursor on the symbol's name identifier (e.g., the 'd' in 'dedent'),
        // not the 'pub' keyword, converted into the server's negotiated
        // position encoding (TS-A2 / LSP-A5). rust-analyzer requires this for
        // symbol resolution.
        let (position_line, position_char) = self
            .compute_lsp_position(
                &ctx.semantic_path.file_path,
                ctx.scope.name_line,
                ctx.scope.name_column,
                ctx.language_id,
            )
            .await;
        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &ctx.semantic_path.file_path,
                position_line,
                position_char,
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

    /// Generates the agent-facing `hint` field.
    fn compute_callers_callees_hint(params: &HintParams<'_>) -> Option<String> {
        let unknown_search_hint = "Use your host's grep tool to find usages manually.".to_owned();
        if params.used_impl_expansion {
            return Some(format!(
                "Symbol is a trait/interface method. Results include callers of all \
                 implementations found via goto_implementation. \
                 {} implementation(s) found across {} file(s).",
                params.trait_impl_count, params.trait_impl_file_count
            ));
        }
        if params.degraded && params.incoming.is_none() && params.outgoing.is_none() {
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
            return Some(format!(
                "Results are heuristic grep-based candidates (LSP unavailable). \
                 Each reference may be a false positive (dynamic dispatch, \
                 same-name symbols). {unknown_search_hint}"
            ));
        }
        if params
            .incoming
            .as_ref()
            .is_some_and(|ev| ev.items.is_empty())
        {
            return Some(
                "LSP confirmed zero incoming callers. This symbol may be an entry point, \
                 unused, or only called via dynamic dispatch/reflection."
                    .to_owned(),
            );
        }
        None
    }

    fn format_impact_reference_path(r: &crate::server::types::ImpactReference) -> String {
        if r.semantic_path.contains("::") {
            format!("{}:L{}", r.semantic_path, r.line)
        } else if r.semantic_path.is_empty() || r.semantic_path == r.file {
            format!("{}:L{}", r.file, r.line)
        } else {
            format!("{}::{}:L{}", r.file, r.semantic_path, r.line)
        }
    }

    fn render_evidence_section(
        label: &str,
        evidence: Option<
            &pathfinder_common::types::Evidence<crate::server::types::ImpactReference>,
        >,
        text_parts: &mut Vec<String>,
    ) {
        match evidence {
            None => {
                text_parts.push(format!(
                    "{label}: UNKNOWN (not verified — do NOT treat as zero)"
                ));
            }
            Some(ev)
                if ev.confidence == pathfinder_common::types::Confidence::Verified
                    && ev.items.is_empty() =>
            {
                text_parts.push(format!("{label}: 0 (verified zero)"));
            }
            Some(ev) if ev.confidence == pathfinder_common::types::Confidence::Verified => {
                text_parts.push(format!("{label}: {} direct (verified)", ev.items.len()));
                for r in &ev.items {
                    let path = Self::format_impact_reference_path(r);
                    text_parts.push(format!("  [depth={}] {}", r.depth, path));
                }
            }
            Some(ev) => {
                text_parts.push(format!(
                    "{label}: {} CANDIDATES (heuristic, unverified)",
                    ev.items.len()
                ));
                for r in &ev.items {
                    let path = Self::format_impact_reference_path(r);
                    text_parts.push(format!("  [depth={}] {} [HEURISTIC]", r.depth, path));
                    if !r.snippet.is_empty() {
                        text_parts.push(format!("    > {}", r.snippet.trim()));
                    }
                }
            }
        }
    }

    fn render_callers_callees_header(
        params: &RenderCallersCalleesParams<'_>,
        text_parts: &mut Vec<String>,
    ) {
        if let Some(header) =
            crate::server::helpers::format_tool_status_header(&params.metadata.status)
        {
            text_parts.push(header);
            text_parts.push(String::new());
        } else if params.degraded {
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
            text_parts.push(
                "   Workaround: Use your host's grep tool to find usages manually.".to_owned(),
            );
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

        if let Some(res) = &params.resolution_block {
            text_parts.push(res.clone());
            text_parts.push(String::new());
        }
    }

    /// Builds the honest, human-readable text summary listing every
    /// reference so agents can act without parsing `structured_content`.
    fn render_callers_callees_text(params: &RenderCallersCalleesParams<'_>) -> String {
        let mut text_parts = Vec::new();
        Self::render_callers_callees_header(params, &mut text_parts);

        Self::render_evidence_section(
            "Incoming callers",
            params.metadata.incoming.as_ref(),
            &mut text_parts,
        );
        Self::render_evidence_section(
            "Outgoing callees",
            params.metadata.outgoing.as_ref(),
            &mut text_parts,
        );

        if let Some(next) = params.metadata.next_offset {
            let returned_count = params.inc_count + params.out_count;
            text_parts.push(format!(
                "\nShowing {returned_count} of {} total. Reissue with offset={next} for the rest.\n",
                params.total_items
            ));
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
                    let path = if r.semantic_path.contains("::") {
                        format!("{}:L{}", r.semantic_path, r.line)
                    } else if r.semantic_path.is_empty() || r.semantic_path == r.file {
                        format!("{}:L{}", r.file, r.line)
                    } else {
                        format!("{}::{}:L{}", r.file, r.semantic_path, r.line)
                    };
                    text_parts.push(format!("  - {path}"));
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

        if let Some(hint) = &params.metadata.hint {
            if !hint.is_empty() {
                text_parts.push(String::new());
                text_parts.push(format!("HINT: {hint}"));
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

    /// Derives `Evidence` for both directions from the raw BFS/grep result
    /// vectors.
    ///
    /// Confidence is intentionally a whole-response decision, not a
    /// per-direction one: Contract 2 Rule 4 makes `Confidence::Verified`
    /// under `ToolStatus::Degraded`/`Unavailable` an illegal combination
    /// (enforced by `validate_evidence`'s `debug_assert!`), specifically so
    /// an agent reading an admittedly-degraded response can never cherry-pick
    /// one field and treat it as authoritative. LSP-A1's actual defect is
    /// therefore **not** here — it is in whether the *degraded/status*
    /// determination itself correctly distinguishes "both directions
    /// resolved with the global index warm" from "one direction returned an
    /// empty result because the global index isn't warm yet" (see
    /// `VerificationStatus` and the emptiness re-check in
    /// `handle_call_hierarchy_items`, which must treat *either* direction's
    /// emptiness as suspect, not only both together).
    fn compute_raw_evidence(
        status: &pathfinder_common::types::ToolStatus,
        incoming: Option<Vec<crate::server::types::ImpactReference>>,
        outgoing: Option<Vec<crate::server::types::ImpactReference>>,
    ) -> (
        Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>>,
        Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>>,
    ) {
        match status {
            pathfinder_common::types::ToolStatus::NotReady { .. } => (None, None),
            pathfinder_common::types::ToolStatus::Degraded { .. }
            | pathfinder_common::types::ToolStatus::Unavailable { .. } => (
                Self::direction_evidence(incoming, false),
                Self::direction_evidence(outgoing, false),
            ),
            pathfinder_common::types::ToolStatus::Ok => (
                Self::direction_evidence(incoming, true),
                Self::direction_evidence(outgoing, true),
            ),
        }
    }

    /// Builds one direction's `Evidence` from its raw items and the
    /// whole-response confidence decided by [`Self::compute_raw_evidence`].
    ///
    /// `verified && empty` is a genuine LSP-confirmed zero (`Evidence::
    /// verified([])`). `!verified && empty` carries no information (Contract
    /// 2 Rule 2 forbids `Evidence{Heuristic, []}`) and correctly collapses to
    /// `None` — UNKNOWN, not a confirmed zero.
    fn direction_evidence(
        items: Option<Vec<crate::server::types::ImpactReference>>,
        verified: bool,
    ) -> Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>> {
        items.and_then(|mut v| {
            let confidence = if verified {
                pathfinder_common::types::Confidence::Verified
            } else {
                pathfinder_common::types::Confidence::Heuristic
            };
            for item in &mut v {
                item.confidence = confidence;
            }
            if v.is_empty() {
                verified.then(|| pathfinder_common::types::Evidence::verified(v))
            } else if verified {
                Some(pathfinder_common::types::Evidence::verified(v))
            } else {
                Some(pathfinder_common::types::Evidence::heuristic(v))
            }
        })
    }

    fn paginate_call_hierarchy_evidence(
        raw_inc: Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>>,
        raw_out: Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>>,
        offset: u32,
        max_references: u32,
        inc_count: usize,
    ) -> (
        Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>>,
        Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>>,
    ) {
        let offset_usize = usize::try_from(offset).unwrap_or(0);
        let max_refs_usize = usize::try_from(max_references).unwrap_or(50);

        if offset_usize >= inc_count {
            let out_offset = offset_usize - inc_count;
            let inc = raw_inc.map(|ev| pathfinder_common::types::Evidence {
                confidence: ev.confidence,
                items: Vec::new(),
            });
            let out = raw_out.and_then(|ev| {
                Self::evidence_or_none_if_empty(
                    ev.confidence,
                    ev.items
                        .into_iter()
                        .skip(out_offset)
                        .take(max_refs_usize)
                        .collect(),
                )
            });
            (inc, out)
        } else {
            let inc_items: Vec<_> = raw_inc.as_ref().map_or_else(Vec::new, |ev| {
                ev.items
                    .iter()
                    .skip(offset_usize)
                    .take(max_refs_usize)
                    .cloned()
                    .collect()
            });
            let inc_kept = inc_items.len();
            let rem_budget = max_refs_usize.saturating_sub(inc_kept);

            let inc = raw_inc.map(|ev| pathfinder_common::types::Evidence {
                confidence: ev.confidence,
                items: inc_items,
            });
            // Contract 2 Rule 2: `Evidence { confidence: Heuristic, items: [] }`
            // is an illegal state (use `None` instead). The shared
            // `max_references` budget is split between incoming and outgoing;
            // when incoming alone consumes the whole budget (e.g. a common
            // symbol name under a small `max_references`), `rem_budget` can
            // hit 0 and truncate a genuinely non-empty, still-Heuristic
            // `raw_out` down to an empty list. Re-clamp to `None` rather than
            // shipping the illegal shape — the caller must read this as
            // UNKNOWN (budget-starved), not as a confirmed zero.
            let out = raw_out.and_then(|ev| {
                Self::evidence_or_none_if_empty(
                    ev.confidence,
                    ev.items.into_iter().take(rem_budget).collect(),
                )
            });
            (inc, out)
        }
    }

    /// Rewraps a confidence/items pair as `Some(Evidence)`, or `None` if
    /// pagination left the item list empty — never
    /// `Some(Evidence { confidence: Heuristic, items: [] })`, which Contract 2
    /// Rule 2 forbids. A `Verified` evidence with zero items is a legitimate,
    /// distinct signal ("confirmed zero") and is left as-is.
    fn evidence_or_none_if_empty(
        confidence: pathfinder_common::types::Confidence,
        items: Vec<crate::server::types::ImpactReference>,
    ) -> Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>> {
        if items.is_empty() && confidence == pathfinder_common::types::Confidence::Heuristic {
            None
        } else {
            Some(pathfinder_common::types::Evidence { confidence, items })
        }
    }

    fn compute_call_hierarchy_status(
        degraded: bool,
        warm_start_in_progress: Option<bool>,
        has_results: bool,
        duration_ms: u128,
        retry_attempt: u32,
        degraded_reason_str: Option<&str>,
    ) -> pathfinder_common::types::ToolStatus {
        if degraded {
            if warm_start_in_progress == Some(true) && !has_results {
                pathfinder_common::types::ToolStatus::NotReady {
                    elapsed_secs: u32::try_from(duration_ms / 1000).unwrap_or(0),
                    retry_after_secs: 15,
                    eta_secs: None,
                    phase: "indexing".to_string(),
                    attempt: retry_attempt,
                    max_attempts: 3,
                }
            } else {
                pathfinder_common::types::ToolStatus::Degraded {
                    reason: degraded_reason_str.unwrap_or("degraded").into(),
                    retry_after_secs: None,
                }
            }
        } else {
            pathfinder_common::types::ToolStatus::Ok
        }
    }

    fn process_call_hierarchy_pagination(
        raw_inc: Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>>,
        raw_out: Option<pathfinder_common::types::Evidence<crate::server::types::ImpactReference>>,
        offset: u32,
        max_references: u32,
        references_truncated: bool,
    ) -> CallHierarchyPagination {
        let total_inc = raw_inc.as_ref().map_or(0, |ev| ev.items.len());
        let total_out = raw_out.as_ref().map_or(0, |ev| ev.items.len());
        let total_items = total_inc + total_out;

        let offset_usize = usize::try_from(offset).unwrap_or(0);
        let max_refs_usize = usize::try_from(max_references).unwrap_or(50).max(1);
        let truncated =
            references_truncated || total_items > offset_usize.saturating_add(max_refs_usize);
        let next_offset = if truncated {
            Some(u32::try_from(offset_usize.saturating_add(max_refs_usize)).unwrap_or(u32::MAX))
        } else {
            None
        };

        let (paginated_inc, paginated_out) = Self::paginate_call_hierarchy_evidence(
            raw_inc,
            raw_out,
            offset,
            max_references,
            total_inc,
        );

        let inc_count = paginated_inc.as_ref().map_or(0, |ev| ev.items.len());
        let out_count = paginated_out.as_ref().map_or(0, |ev| ev.items.len());

        CallHierarchyPagination {
            paginated_inc,
            paginated_out,
            inc_count,
            out_count,
            total_items,
            truncated,
            next_offset,
        }
    }

    /// Assembles the final [`crate::server::types::FindCallersCalleesMetadata`] and human-readable text
    /// body from the accumulated `state`.
    fn finalize_call_hierarchy_result(
        params: FinalizeCallHierarchyParams,
    ) -> (crate::server::types::FindCallersCalleesMetadata, String) {
        let FinalizeCallHierarchyParams {
            state,
            trait_method_degraded,
            used_impl_expansion,
            trait_impl_count,
            trait_impl_file_count,
            requested_semantic_path,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            max_references,
            offset,
            retry_attempt,
        } = params;

        let CallHierarchyDegradedStatus {
            degraded,
            degraded_reason,
            lsp_readiness,
            warm_start_in_progress,
            status,
        } = Self::compute_degraded_and_status(
            &state,
            trait_method_degraded,
            duration_ms,
            retry_attempt,
        );

        let CallHierarchyState {
            incoming,
            outgoing,
            degraded: _,
            degraded_reason: _,
            engines,
            files_referenced,
            max_depth_reached,
            verification,
            references_truncated,
            resolution_block,
            unavailable: _,
        } = state;

        tracing::info!(
            tool = "find_callers_callees",
            semantic_path = %requested_semantic_path,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            degraded,
            engines_used = ?engines,
            "find_callers_callees: complete"
        );

        let _ = &verification; // consulted upstream in compute_degraded_and_status
        let (raw_inc, raw_out) = Self::compute_raw_evidence(&status, incoming, outgoing);
        let pagination = Self::process_call_hierarchy_pagination(
            raw_inc,
            raw_out,
            offset,
            max_references,
            references_truncated,
        );

        crate::server::helpers::validate_evidence(&status, pagination.paginated_inc.as_ref());
        crate::server::helpers::validate_evidence(&status, pagination.paginated_out.as_ref());

        let resolution_strategy = Some(Self::compute_resolution_strategy(
            used_impl_expansion,
            &engines,
            degraded,
            degraded_reason.as_ref(),
        ));

        let hint = Self::compute_callers_callees_hint(&HintParams {
            used_impl_expansion,
            trait_impl_count,
            trait_impl_file_count,
            degraded,
            incoming: &pagination.paginated_inc,
            outgoing: &pagination.paginated_out,
        });

        let inc_count = pagination.inc_count;
        let out_count = pagination.out_count;
        let total_items = pagination.total_items;

        let metadata = Self::build_callers_callees_metadata(CallersCalleesMetaParams {
            status,
            pagination: &pagination,
            max_depth_reached,
            files_referenced_len: files_referenced.len(),
            degraded,
            degraded_reason,
            lsp_readiness,
            warm_start_in_progress,
            resolution_strategy,
            duration_ms,
            hint,
        });

        Self::render_final_call_hierarchy(
            metadata,
            inc_count,
            out_count,
            total_items,
            resolution_block,
            degraded_reason,
            duration_ms,
        )
    }

    fn compute_degraded_and_status(
        state: &CallHierarchyState,
        trait_method_degraded: Option<DegradedReason>,
        duration_ms: u128,
        retry_attempt: u32,
    ) -> CallHierarchyDegradedStatus {
        let degraded = state.degraded
            || trait_method_degraded.is_some()
            || !state.verification.incoming
            || !state.verification.outgoing;
        let degraded_reason = trait_method_degraded.or(state.degraded_reason).or(
            if !state.verification.incoming || !state.verification.outgoing {
                Some(DegradedReason::GrepFallbackGlobal)
            } else {
                None
            },
        );
        let degraded_reason_cloned = degraded_reason;
        let degraded_reason_str = degraded_reason.as_ref().map(ToString::to_string);

        let (lsp_readiness, warm_start_in_progress) =
            Self::compute_lsp_readiness(degraded, degraded_reason_cloned);

        // `ensure_ready` reported `Unavailable` before any call-hierarchy
        // request was attempted -- a terminal, config-level fact, not a
        // transient failure. Overrides the derived `status` with the
        // dedicated `ToolStatus::Unavailable` variant (install hint +
        // alternative, "do not retry") while leaving `degraded`/
        // `degraded_reason`/`lsp_readiness` as already computed above, since
        // those legacy fields are still read by the text renderer.
        if let Some((language, install_hint, alternative)) = state.unavailable.clone() {
            return CallHierarchyDegradedStatus {
                degraded,
                degraded_reason,
                lsp_readiness,
                warm_start_in_progress,
                status: pathfinder_common::types::ToolStatus::Unavailable {
                    language,
                    install_hint,
                    alternative,
                },
            };
        }

        let has_results = state.incoming.as_ref().is_some_and(|v| !v.is_empty())
            || state.outgoing.as_ref().is_some_and(|v| !v.is_empty());
        let status = Self::compute_call_hierarchy_status(
            degraded,
            warm_start_in_progress,
            has_results,
            duration_ms,
            retry_attempt,
            degraded_reason_str.as_deref(),
        );

        CallHierarchyDegradedStatus {
            degraded,
            degraded_reason,
            lsp_readiness,
            warm_start_in_progress,
            status,
        }
    }

    fn build_callers_callees_metadata(
        params: CallersCalleesMetaParams<'_>,
    ) -> crate::server::types::FindCallersCalleesMetadata {
        crate::server::types::FindCallersCalleesMetadata {
            status: params.status,
            incoming: params.pagination.paginated_inc.clone(),
            outgoing: params.pagination.paginated_out.clone(),
            heuristic_candidates: None,
            depth_reached: params.max_depth_reached,
            files_referenced: params.files_referenced_len,
            degraded: params.degraded,
            degraded_reason: params.degraded_reason,
            actionable_guidance: params
                .degraded_reason
                .as_ref()
                .map(DegradedReason::guidance),
            lsp_readiness: params.lsp_readiness,
            warm_start_in_progress: params.warm_start_in_progress,
            references_truncated: params.pagination.truncated,
            next_offset: params.pagination.next_offset,
            resolution_strategy: params.resolution_strategy,
            test_callers: None,
            test_coverage_status: None,
            duration_ms: Some(millis_to_u64(params.duration_ms)),
            hint: params.hint,
        }
    }

    fn render_final_call_hierarchy(
        metadata: crate::server::types::FindCallersCalleesMetadata,
        inc_count: usize,
        out_count: usize,
        total_items: usize,
        resolution_block: Option<String>,
        degraded_reason_cloned: Option<DegradedReason>,
        duration_ms: u128,
    ) -> (crate::server::types::FindCallersCalleesMetadata, String) {
        let text = Self::render_callers_callees_text(&RenderCallersCalleesParams {
            degraded: metadata.degraded,
            degraded_reason: degraded_reason_cloned,
            inc_count,
            out_count,
            total_items,
            resolution_block,
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

        if params.max_depth > 2 {
            return Err(crate::server::helpers::invalid_params_error(format!(
                "max_depth exceeds maximum allowed depth of 2 (requested: {}). Hint: For deep dependency traversal beyond depth 2, delegate to subagents or issue follow-up trace calls at each boundary.",
                params.max_depth
            )));
        }
        let max_depth = params.max_depth.max(1);
        let project_only = true;
        let max_references = if params.max_references == 0 {
            20
        } else {
            params.max_references.clamp(1, 500)
        };
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

        let ext = semantic_path
            .file_path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("");
        let language_id = pathfinder_lsp::language_id_for_extension(ext).unwrap_or("unknown");
        let readiness = self
            .lawyer
            .ensure_ready(language_id, std::time::Duration::from_secs(5))
            .await
            .unwrap_or(pathfinder_lsp::types::Readiness::Unavailable {
                language: language_id.to_string(),
                install_hint: format!("No LSP configured for {language_id}"),
                alternative: "Built-in fallback tools".to_string(),
            });

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
            language_id,
        };

        let (used_impl_expansion, trait_impl_count, trait_impl_file_count, trait_method_degraded) =
            if let pathfinder_lsp::types::Readiness::Unavailable {
                language,
                install_hint,
                alternative,
            } = readiness
            {
                // ensure_ready already confirmed there is no LSP for this
                // language -- skip straight to the grep fallback instead of
                // attempting a call_hierarchy_prepare guaranteed to fail the
                // same way, and record the terminal Unavailable status
                // (install hint + alternative, "do not retry") rather than
                // letting it collapse into the weaker Degraded signal.
                state.unavailable = Some((language, install_hint, alternative));
                self.handle_no_lsp_for_callers(&ctx, &mut remaining_outgoing, &mut state)
                    .await;
                (false, 0, 0, None)
            } else {
                self.run_call_hierarchy_resolution(
                    &ctx,
                    &mut remaining_incoming,
                    &mut remaining_outgoing,
                    &mut state,
                )
                .await
            };

        // Note: `_doc_guard` still alive here; did_close fires at function return.
        let lsp_ms = lsp_start.elapsed().as_millis();
        let duration_ms = start.elapsed().as_millis();

        let (metadata, text) = Self::finalize_call_hierarchy_result(FinalizeCallHierarchyParams {
            state,
            trait_method_degraded,
            used_impl_expansion,
            trait_impl_count,
            trait_impl_file_count,
            requested_semantic_path: params.semantic_path,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            max_references,
            offset: params.offset,
            retry_attempt: params.retry_attempt,
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
