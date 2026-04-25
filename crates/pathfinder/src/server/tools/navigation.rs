//! Navigation tool handlers: `get_definition`, `analyze_impact`, and
//! `read_with_deep_context`.
//!
//! All three tools are LSP-powered but degrade gracefully when no language
//! server is available. The tool responses include `"degraded": true` and
//! `"degraded_reason"` fields to signal the fallback mode to agents.
//!
//! # Degraded Mode
//! When the `Lawyer` returns `LspError::NoLspAvailable`:
//! - `get_definition` — returns an error response (`LSP_REQUIRED`)
//! - `analyze_impact` — returns `null` caller/callee lists with `degraded: true`
//! - `read_with_deep_context` — returns the symbol scope only, no dependencies

use crate::server::helpers::{
    parse_semantic_path, pathfinder_to_error_data, require_symbol_target,
    treesitter_error_to_error_data,
};
use crate::server::types::{
    AnalyzeImpactParams, GetDefinitionParams, GetDefinitionResponse, ReadWithDeepContextParams,
};
use crate::server::PathfinderServer;
use pathfinder_common::error::PathfinderError;
use pathfinder_lsp::LspError;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::{CallToolResult, ErrorData};

/// Direction for call hierarchy BFS traversal in `analyze_impact`.
///
/// `Incoming` traverses callers (who calls this symbol).
/// `Outgoing` traverses callees (what this symbol calls).
enum CallDirection {
    Incoming,
    Outgoing,
}

/// Result of LSP call-hierarchy resolution for `read_with_deep_context`.
struct LspResolution {
    dependencies: Vec<crate::server::types::DeepContextDependency>,
    degraded: bool,
    degraded_reason: Option<String>,
    engines: Vec<&'static str>,
}

impl PathfinderServer {
    /// Resolve LSP call-hierarchy dependencies for a symbol.
    ///
    /// Extracted from `read_with_deep_context` to reduce nesting depth.
    /// Prepares the call hierarchy, then fetches outgoing calls and
    /// maps them to `DeepContextDependency` entries.
    async fn resolve_lsp_dependencies(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        start_line: usize,
    ) -> LspResolution {
        let mut dependencies = Vec::new();
        let mut degraded = true;
        let mut degraded_reason = Some("no_lsp".to_owned());
        let mut engines = vec!["tree-sitter"];

        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(start_line + 1).unwrap_or(1),
                1,
            )
            .await;

        match lsp_result {
            Ok(items) if !items.is_empty() => {
                self.append_outgoing_deps(
                    &items[0],
                    &mut dependencies,
                    &mut engines,
                    &mut degraded,
                    &mut degraded_reason,
                )
                .await;
            }
            Ok(_) => {
                engines.push("lsp");
                degraded = false;
                degraded_reason = None;
            }
            Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }) => {}
            Err(e) => {
                tracing::warn!(
                    tool = "read_with_deep_context",
                    error = %e,
                    "call_hierarchy_prepare failed"
                );
            }
        }

        LspResolution {
            dependencies,
            degraded,
            degraded_reason,
            engines,
        }
    }

    /// Fetch outgoing call-hierarchy items and append them as dependencies.
    async fn append_outgoing_deps(
        &self,
        item: &pathfinder_lsp::types::CallHierarchyItem,
        dependencies: &mut Vec<crate::server::types::DeepContextDependency>,
        engines: &mut Vec<&'static str>,
        degraded: &mut bool,
        degraded_reason: &mut Option<String>,
    ) {
        match self
            .lawyer
            .call_hierarchy_outgoing(self.workspace_root.path(), item)
            .await
        {
            Ok(outgoing) => {
                engines.push("lsp");
                for call in outgoing {
                    let callee = call.item;
                    let signature = callee.detail.clone().unwrap_or_else(|| callee.name.clone());
                    let sp = format!("{}::{}", callee.file, callee.name);
                    dependencies.push(crate::server::types::DeepContextDependency {
                        semantic_path: sp,
                        signature,
                        file: callee.file,
                        line: callee.line as usize,
                    });
                }
                *degraded = false;
                *degraded_reason = None;
            }
            Err(e) => {
                tracing::warn!(
                    tool = "read_with_deep_context",
                    error = %e,
                    "call_hierarchy_outgoing failed"
                );
            }
        }
    }

    /// Core logic for the `get_definition` tool.
    ///
    /// Resolves the semantic path to a file position, queries the LSP for the
    /// definition location, and returns the result.
    ///
    /// **Degraded mode:** Returns a `LSP_REQUIRED` error when no LSP is configured.
    #[allow(clippy::too_many_lines)]
    pub(crate) async fn get_definition_impl(
        &self,
        params: GetDefinitionParams,
    ) -> Result<Json<GetDefinitionResponse>, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "get_definition",
            semantic_path = %params.semantic_path,
            "get_definition: start"
        );

        // Parse and validate the semantic path
        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;

        // Sandbox check
        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "get_definition",
                error_code = e.error_code(),
                duration_ms,
                "sandbox check failed"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // Resolve the symbol position via Tree-sitter to get line/column
        let ts_start = std::time::Instant::now();
        let symbol_scope = self
            .surgeon
            .read_symbol_scope(self.workspace_root.path(), &semantic_path)
            .await
            .map_err(treesitter_error_to_error_data)?;
        let tree_sitter_ms = ts_start.elapsed().as_millis();

        // Query LSP for the definition location at the symbol's start line
        let lsp_start = std::time::Instant::now();
        let lsp_result = self
            .lawyer
            .goto_definition(
                self.workspace_root.path(),
                &semantic_path.file_path,
                // Convert 0-indexed start_line from SymbolScope to 1-indexed for Lawyer
                u32::try_from(symbol_scope.start_line + 1).unwrap_or(1),
                1, // Column 1 — start of the identifier line
            )
            .await;
        let lsp_ms = lsp_start.elapsed().as_millis();

        let duration_ms = start.elapsed().as_millis();

        match lsp_result {
            Ok(Some(def)) => {
                tracing::info!(
                    tool = "get_definition",
                    file = %def.file,
                    definition_line = def.line,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    engines_used = ?["tree-sitter", "lsp"],
                    "get_definition: complete"
                );
                Ok(Json(GetDefinitionResponse {
                    file: def.file,
                    line: def.line,
                    column: def.column,
                    preview: def.preview,
                    degraded: false,
                    degraded_reason: None,
                }))
            }
            Ok(None) => {
                // Symbol has no definition (e.g., built-in, external) or LSP is still warming up.
                tracing::info!(
                    tool = "get_definition",
                    semantic_path = %params.semantic_path,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    "get_definition: no definition found via LSP — attempting grep-based fallback"
                );

                if let Some(mut def) = self.fallback_definition_grep(&semantic_path).await {
                    def.degraded_reason = Some(
                        "lsp_warmup_grep_fallback: LSP returned no result (likely warming up); \
                         result from Ripgrep pattern search — may not be the canonical definition. \
                         Verify with read_source_file."
                            .to_owned(),
                    );
                    tracing::info!(
                        tool = "get_definition",
                        file = %def.file,
                        line = def.line,
                        duration_ms,
                        degraded = true,
                        degraded_reason = "lsp_warmup_grep_fallback",
                        engines_used = ?["tree-sitter", "lsp", "ripgrep"],
                        "get_definition: degraded complete (grep fallback after LSP None)"
                    );
                    return Ok(Json(def));
                }

                tracing::info!(
                    tool = "get_definition",
                    semantic_path = %params.semantic_path,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    "get_definition: no definition found (LSP None, grep empty)"
                );
                Err(pathfinder_to_error_data(&PathfinderError::SymbolNotFound {
                    semantic_path: params.semantic_path,
                    did_you_mean: vec![],
                }))
            }
            Err(LspError::NoLspAvailable) => {
                // Degraded mode — LSP not available. Use a grep-based heuristic to
                // find a likely definition location. This is not LSP-accurate but
                // gives the agent a starting point without requiring a full
                // `search_codebase` call.
                tracing::info!(
                    tool = "get_definition",
                    symbol = %semantic_path,
                    "get_definition: no LSP — attempting grep-based fallback"
                );

                if let Some(mut def) = self.fallback_definition_grep(&semantic_path).await {
                    def.degraded_reason = Some(
                        "no_lsp_grep_fallback: LSP unavailable; result from Ripgrep \
                         pattern search — may not be the canonical definition. \
                         Verify with read_source_file."
                            .to_owned(),
                    );
                    tracing::info!(
                        tool = "get_definition",
                        file = %def.file,
                        line = def.line,
                        duration_ms,
                        degraded = true,
                        degraded_reason = "no_lsp_grep_fallback",
                        engines_used = ?["tree-sitter", "ripgrep"],
                        "get_definition: degraded complete (grep fallback)"
                    );
                    return Ok(Json(def));
                }

                // No grep match either — return the original LSP error
                tracing::info!(
                    tool = "get_definition",
                    duration_ms,
                    degraded = true,
                    degraded_reason = "no_lsp",
                    engines_used = ?["none"],
                    "get_definition: degraded (no LSP, grep fallback also empty)"
                );
                Err(pathfinder_to_error_data(&PathfinderError::NoLspAvailable {
                    language: symbol_scope.language,
                }))
            }
            Err(e) => {
                tracing::warn!(
                    tool = "get_definition",
                    error = %e,
                    tree_sitter_ms,
                    lsp_ms,
                    duration_ms,
                    engines_used = ?["lsp"],
                    "get_definition: LSP error"
                );
                Err(pathfinder_to_error_data(&PathfinderError::LspError {
                    message: e.to_string(),
                }))
            }
        }
    }

    /// Grep-based fallback for definition resolution when LSP is unavailable or warming up.
    async fn fallback_definition_grep(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
    ) -> Option<GetDefinitionResponse> {
        let symbol_name = semantic_path
            .symbol_chain
            .as_ref()
            .and_then(|c| c.segments.last())
            .map(|s| s.name.clone())
            .unwrap_or_default();

        let pattern =
            format!(r"(?:fn|def|func|class|struct|type|interface|const|let|var)\s+{symbol_name}");

        let search_result = self
            .scout
            .search(&pathfinder_search::SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: pattern,
                is_regex: true,
                max_results: 5,
                path_glob: "**/*".to_owned(),
                exclude_glob: String::new(),
                context_lines: 0,
            })
            .await;

        if let Ok(result) = search_result {
            if !result.matches.is_empty() {
                let m = &result.matches[0];
                return Some(GetDefinitionResponse {
                    file: m.file.clone(),
                    line: u32::try_from(m.line).unwrap_or(u32::MAX),
                    column: u32::try_from(m.column).unwrap_or(1),
                    preview: m.content.clone(),
                    degraded: true,
                    degraded_reason: Some(
                        "grep_fallback: result from Ripgrep pattern search — \
                         may not be the canonical definition. Verify with read_source_file."
                            .to_owned(),
                    ),
                });
            }
        }
        None
    }

    /// Core logic for the `read_with_deep_context` tool.
    ///
    /// Returns the symbol's source code. When LSP is available, appends the
    /// signatures of all called symbols. Degrades gracefully to symbol scope
    /// only when no LSP is configured.
    pub(crate) async fn read_with_deep_context_impl(
        &self,
        params: ReadWithDeepContextParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "read_with_deep_context",
            semantic_path = %params.semantic_path,
            "read_with_deep_context: start"
        );

        // Parse and validate the semantic path
        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;

        // Sandbox check
        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "read_with_deep_context",
                error_code = e.error_code(),
                duration_ms,
                "sandbox check failed"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // Fetch the symbol scope (Tree-sitter)
        let ts_start = std::time::Instant::now();
        let scope = self
            .surgeon
            .read_symbol_scope(self.workspace_root.path(), &semantic_path)
            .await
            .map_err(treesitter_error_to_error_data)?;
        let tree_sitter_ms = ts_start.elapsed().as_millis();

        let lsp_start = std::time::Instant::now();

        let LspResolution {
            dependencies,
            degraded,
            degraded_reason,
            engines,
        } = self
            .resolve_lsp_dependencies(&semantic_path, scope.start_line)
            .await;

        let lsp_ms = lsp_start.elapsed().as_millis();
        let duration_ms = start.elapsed().as_millis();

        tracing::info!(
            tool = "read_with_deep_context",
            semantic_path = %params.semantic_path,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            degraded,
            degraded_reason,
            engines_used = ?engines,
            "read_with_deep_context: complete"
        );

        let dep_count = dependencies.len();
        let metadata = crate::server::types::ReadWithDeepContextMetadata {
            start_line: scope.start_line,
            end_line: scope.end_line,
            version_hash: scope.version_hash.to_string(),
            language: scope.language,
            dependencies,
            degraded,
            degraded_reason: degraded_reason.clone(),
        };

        // Prepend degradation notice when in degraded mode
        let text = if degraded {
            let reason = degraded_reason.as_deref().unwrap_or("unknown");
            format!(
                "DEGRADED MODE ({}) — {dep_count} dependencies loaded (results may be incomplete)\n\n{}",
                reason, scope.content
            )
        } else {
            format!("{dep_count} dependencies loaded\n\n{}", scope.content)
        };
        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = Some(serde_json::to_value(metadata).unwrap_or_default());
        Ok(res)
    }

    /// Performs BFS traversal of the call hierarchy in the specified direction.
    ///
    /// Returns the collected references and the maximum depth reached during traversal.
    async fn bfs_call_hierarchy(
        &self,
        initial_item: &pathfinder_lsp::types::CallHierarchyItem,
        direction: CallDirection,
        max_depth: u32,
        files_referenced: &mut std::collections::HashSet<String>,
    ) -> (Vec<crate::server::types::ImpactReference>, u32) {
        let mut queue = std::collections::VecDeque::new();
        queue.push_back((initial_item.clone(), 0));
        let mut seen = std::collections::HashSet::new();
        seen.insert((initial_item.file.clone(), initial_item.line));
        files_referenced.insert(initial_item.file.clone());

        let mut references = Vec::new();
        let mut max_depth_reached = 0;

        while let Some((item, current_depth)) = queue.pop_front() {
            max_depth_reached = std::cmp::max(max_depth_reached, current_depth);
            if current_depth >= max_depth {
                continue;
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
                    for call in calls {
                        let referenced_item = call.item;
                        files_referenced.insert(referenced_item.file.clone());

                        let key = (referenced_item.file.clone(), referenced_item.line);
                        if !seen.contains(&key) {
                            seen.insert(key);
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
                                version_hash: String::new(), // Populated at higher layer if needed
                                direction: match direction {
                                    CallDirection::Incoming => "incoming".to_owned(),
                                    CallDirection::Outgoing => "outgoing".to_owned(),
                                },
                                depth: current_depth as usize,
                            });
                        }
                    }
                }
                Err(e) => {
                    let direction_name = match direction {
                        CallDirection::Incoming => "call_hierarchy_incoming",
                        CallDirection::Outgoing => "call_hierarchy_outgoing",
                    };
                    tracing::warn!(
                        tool = "analyze_impact",
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

    /// Core logic for the `analyze_impact` tool.
    ///
    /// Returns callers (incoming) and callees (outgoing) for the target symbol.
    /// Degrades gracefully to empty results when no LSP is configured.
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential pipeline (parse→sandbox→tree-sitter→LSP→BFS→version hash)."
    )]
    pub(crate) async fn analyze_impact_impl(
        &self,
        params: AnalyzeImpactParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();

        tracing::info!(
            tool = "analyze_impact",
            semantic_path = %params.semantic_path,
            max_depth = params.max_depth,
            "analyze_impact: start"
        );

        // Parse and validate the semantic path
        let semantic_path = parse_semantic_path(&params.semantic_path)?;
        require_symbol_target(&semantic_path, &params.semantic_path)?;

        // Sandbox check
        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            let duration_ms = start.elapsed().as_millis();
            tracing::warn!(
                tool = "analyze_impact",
                error_code = e.error_code(),
                duration_ms,
                "sandbox check failed"
            );
            return Err(pathfinder_to_error_data(&e));
        }

        // 1. Fetch the symbol scope (Tree-sitter) to get start line
        let ts_start = std::time::Instant::now();
        let scope = match self
            .surgeon
            .read_symbol_scope(self.workspace_root.path(), &semantic_path)
            .await
        {
            Ok(s) => s,
            Err(e) => {
                let duration_ms = start.elapsed().as_millis();
                tracing::warn!(
                    tool = "analyze_impact",
                    error = %e,
                    duration_ms,
                    "tree-sitter read failed"
                );
                return Err(treesitter_error_to_error_data(e));
            }
        };
        let tree_sitter_ms = ts_start.elapsed().as_millis();

        let lsp_start = std::time::Instant::now();
        // Use Option<Vec> to distinguish "unknown" (LSP unavailable) from "verified empty" (LSP confirmed zero).
        // None = degraded (LSP was down — callers are unknown, do NOT treat as zero)
        // Some([]) = LSP responded with confirmed zero callers/callees
        let mut incoming: Option<Vec<crate::server::types::ImpactReference>> = None;
        let mut outgoing: Option<Vec<crate::server::types::ImpactReference>> = None;
        let mut degraded = true;
        let mut degraded_reason = Some("no_lsp".to_owned());
        let mut engines = vec!["tree-sitter"];
        let mut files_referenced = std::collections::HashSet::new();
        let mut max_depth_reached = 0;

        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(scope.start_line + 1).unwrap_or(1),
                1, // Column 1
            )
            .await;

        match lsp_result {
            Ok(items) if !items.is_empty() => {
                engines.push("lsp");
                degraded = false;
                degraded_reason = None;

                let initial_item = &items[0];

                // --- INCOMING BFS ---
                let (incoming_refs, depth_in) = self
                    .bfs_call_hierarchy(
                        initial_item,
                        CallDirection::Incoming,
                        params.max_depth,
                        &mut files_referenced,
                    )
                    .await;
                incoming = Some(incoming_refs);
                max_depth_reached = std::cmp::max(max_depth_reached, depth_in);

                // --- OUTGOING BFS ---
                let (outgoing_refs, depth_out) = self
                    .bfs_call_hierarchy(
                        initial_item,
                        CallDirection::Outgoing,
                        params.max_depth,
                        &mut files_referenced,
                    )
                    .await;
                outgoing = Some(outgoing_refs);
                max_depth_reached = std::cmp::max(max_depth_reached, depth_out);
            }
            Ok(_) => {
                // LSP responded with empty items — confirmed zero callers/callees
                engines.push("lsp");
                degraded = false;
                degraded_reason = None;
                incoming = Some(Vec::new());
                outgoing = Some(Vec::new());
            }
            Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }) => {
                // Degraded mode — LSP not available. Use grep-based reference search
                // as a heuristic fallback. Results may over-count (string references)
                // or under-count (indirect calls), but give the agent a starting point.
                tracing::info!(
                    tool = "analyze_impact",
                    symbol = %semantic_path,
                    "analyze_impact: no LSP — attempting grep-based reference fallback"
                );

                let symbol_name = semantic_path
                    .symbol_chain
                    .as_ref()
                    .and_then(|c| c.segments.last())
                    .map(|s| s.name.clone())
                    .unwrap_or_default();

                let search_result = self
                    .scout
                    .search(&pathfinder_search::SearchParams {
                        workspace_root: self.workspace_root.path().to_path_buf(),
                        query: symbol_name.clone(),
                        is_regex: false,
                        max_results: 50,
                        path_glob: "**/*".to_owned(),
                        exclude_glob: String::new(),
                        context_lines: 0,
                    })
                    .await;

                if let Ok(result) = search_result {
                    if !result.matches.is_empty() {
                        let refs: Vec<crate::server::types::ImpactReference> = result
                            .matches
                            .into_iter()
                            .map(|m| {
                                files_referenced.insert(m.file.clone());
                                crate::server::types::ImpactReference {
                                    semantic_path: format!("{}::{symbol_name}", m.file),
                                    file: m.file,
                                    line: usize::try_from(m.line).unwrap_or(usize::MAX),
                                    snippet: m.content,
                                    version_hash: String::new(),
                                    // Grep fallback: heuristic, direction is assumed incoming
                                    direction: "incoming_heuristic".to_owned(),
                                    depth: 0,
                                }
                            })
                            .collect();
                        incoming = Some(refs);
                        degraded_reason = Some("no_lsp_grep_fallback".to_owned());
                        tracing::info!(
                            tool = "analyze_impact",
                            references_found = incoming.as_ref().map_or(0, Vec::len),
                            "analyze_impact: grep-based fallback references found"
                        );
                    }
                }
                // Keep degraded = true to signal this is heuristic data
            }
            Err(e) => {
                tracing::warn!(
                    tool = "analyze_impact",
                    error = %e,
                    "call_hierarchy_prepare failed"
                );
            }
        }

        let lsp_ms = lsp_start.elapsed().as_millis();
        let duration_ms = start.elapsed().as_millis();

        // Compute version hashes for all referenced files + the target file itself.
        // This allows agents to immediately edit any impacted file without a separate read.
        let mut version_hashes = std::collections::HashMap::new();
        // Always include the target file
        let target_file_path = self.workspace_root.path().join(&semantic_path.file_path);
        if let Ok(bytes) = tokio::fs::read(&target_file_path).await {
            let hash = pathfinder_common::types::VersionHash::compute(&bytes);
            version_hashes.insert(
                semantic_path.file_path.to_string_lossy().to_string(),
                hash.as_str().to_owned(),
            );
        }
        // Include all files from the call graph
        for file_ref in &files_referenced {
            let abs_path = self.workspace_root.path().join(file_ref);
            if let Ok(bytes) = tokio::fs::read(&abs_path).await {
                let hash = pathfinder_common::types::VersionHash::compute(&bytes);
                version_hashes.insert(file_ref.clone(), hash.as_str().to_owned());
            }
        }

        tracing::info!(
            tool = "analyze_impact",
            semantic_path = %params.semantic_path,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            degraded,
            degraded_reason,
            engines_used = ?engines,
            "analyze_impact: complete"
        );

        let inc_count = incoming.as_ref().map_or(0, Vec::len);
        let out_count = outgoing.as_ref().map_or(0, Vec::len);
        let degraded_reason_cloned = degraded_reason.clone();

        let metadata = crate::server::types::AnalyzeImpactMetadata {
            incoming,
            outgoing,
            depth_reached: max_depth_reached,
            files_referenced: files_referenced.len(),
            degraded,
            degraded_reason,
            version_hashes,
        };

        // Build honest text output based on actual results
        let mut text_parts = Vec::new();
        if degraded {
            text_parts.push(format!(
                "Degraded analysis ({}) — LSP unavailable — reference counts are UNRELIABLE. Do NOT trust zero as 'confirmed no callers'. Grep-based heuristic was used if available. Use search_codebase for manual verification.",
                degraded_reason_cloned.as_deref().unwrap_or("unknown")
            ));
        }
        // Add summary
        text_parts.push(format!("Incoming references: {inc_count}"));
        text_parts.push(format!("Outgoing references: {out_count}"));

        let text = text_parts.join("\n");
        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = Some(serde_json::to_value(metadata).unwrap_or_default());
        Ok(res)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::server::types::{
        AnalyzeImpactParams, GetDefinitionParams, ReadWithDeepContextParams,
    };
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::{SymbolScope, VersionHash, WorkspaceRoot};
    use pathfinder_lsp::types::{CallHierarchyCall, CallHierarchyItem};
    use pathfinder_lsp::{DefinitionLocation, MockLawyer};
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn make_server_with_lawyer(
        mock_surgeon: Arc<MockSurgeon>,
        mock_lawyer: Arc<MockLawyer>,
    ) -> (PathfinderServer, tempfile::TempDir) {
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            mock_surgeon,
            mock_lawyer,
        );
        (server, ws_dir)
    }

    fn make_scope() -> SymbolScope {
        SymbolScope {
            content: "fn login() { }".to_owned(),
            start_line: 9,
            end_line: 9,
            version_hash: VersionHash::compute(b"fn login() { }"),
            language: "rust".to_owned(),
        }
    }

    // ── get_definition ───────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_definition_routes_to_lawyer_success() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        lawyer.set_goto_definition_result(Ok(Some(DefinitionLocation {
            file: "src/auth.rs".into(),
            line: 42,
            column: 5,
            preview: "pub fn login() -> bool {".into(),
        })));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer.clone());
        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };

        let result = server.get_definition_impl(params).await;
        let call_res = result.expect("should succeed");
        let val = call_res.0;

        assert_eq!(val.file, "src/auth.rs");
        assert_eq!(val.line, 42);
        assert_eq!(val.preview, "pub fn login() -> bool {");
        assert!(!val.degraded);
        assert_eq!(lawyer.goto_definition_call_count(), 1);
    }

    #[tokio::test]
    async fn test_get_definition_degrades_when_no_lsp() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // Default MockLawyer returns Ok(None); use NoOpLawyer for NoLspAvailable
        let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
        let ws_dir = tempdir().expect("temp dir");
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

        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.get_definition_impl(params).await;
        // Should return NO_LSP_AVAILABLE error
        let Err(err) = result else {
            panic!("expected error but got Ok");
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "NO_LSP_AVAILABLE");
    }

    #[tokio::test]
    async fn test_get_definition_rejects_empty_semantic_path() {
        let surgeon = Arc::new(MockSurgeon::new());
        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = GetDefinitionParams {
            semantic_path: String::new(), // empty is truly invalid
        };
        let result = server.get_definition_impl(params).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_get_definition_rejects_sandbox_denied_path() {
        let surgeon = Arc::new(MockSurgeon::new());
        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = GetDefinitionParams {
            semantic_path: ".git/objects/abc::def".to_owned(), // sandbox should deny
        };
        let result = server.get_definition_impl(params).await;
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

    // ── read_with_deep_context ────────────────────────────────────────

    #[tokio::test]
    async fn test_read_with_deep_context_degrades_when_call_hierarchy_unsupported() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
        let ws_dir = tempdir().expect("temp dir");
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

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let text_content = match &call_res.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert_eq!(text_content, "DEGRADED MODE (no_lsp) — 0 dependencies loaded (results may be incomplete)\n\nfn login() { }");
        assert!(val.degraded);
        assert_eq!(val.degraded_reason.as_deref(), Some("no_lsp"));
        assert!(val.dependencies.is_empty());
    }

    #[tokio::test]
    async fn test_read_with_deep_context_lsp_populates_dependencies() {
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

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let text_content = match &call_res.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert_eq!(text_content, "1 dependencies loaded\n\nfn login() { }");
        assert!(!val.degraded);
        assert_eq!(val.degraded_reason, None);
        assert_eq!(val.dependencies.len(), 1);
        assert_eq!(
            val.dependencies[0].semantic_path,
            "src/token.rs::validate_token"
        );
        assert_eq!(val.dependencies[0].signature, "fn validate_token() -> bool");
        assert_eq!(val.dependencies[0].file, "src/token.rs");
        assert_eq!(val.dependencies[0].line, 15);
    }

    // ── analyze_impact ────────────────────────────────────────────────

    #[tokio::test]
    async fn test_analyze_impact_returns_empty_degraded() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
        let ws_dir = tempdir().expect("temp dir");
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

        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
        };
        let result = server.analyze_impact_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::AnalyzeImpactMetadata =
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
        assert_eq!(val.degraded_reason.as_deref(), Some("no_lsp"));
    }

    #[tokio::test]
    async fn test_analyze_impact_lsp_populates_incoming_and_outgoing() {
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

        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1,
        };
        let result = server.analyze_impact_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::AnalyzeImpactMetadata =
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

    // ── get_definition LSP error path ──────────────────────────────────

    #[tokio::test]
    async fn test_get_definition_lsp_error_returns_lsp_error() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Simulate an LSP protocol error (not NoLspAvailable, not None)
        lawyer.set_goto_definition_result(Err("LSP protocol error".to_string()));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);
        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };

        let result = server.get_definition_impl(params).await;
        let Err(err) = result else {
            panic!("expected error but got Ok");
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "LSP_ERROR");
    }

    #[tokio::test]
    async fn test_get_definition_lsp_none_no_grep_fallback_returns_symbol_not_found() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // Default MockLawyer returns Ok(None) for goto_definition.
        // MockScout returns empty results → no grep fallback.
        let lawyer = Arc::new(MockLawyer::default());
        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.get_definition_impl(params).await;
        let Err(err) = result else {
            panic!("expected error but got Ok");
        };
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "SYMBOL_NOT_FOUND");
    }

    #[tokio::test]
    async fn test_get_definition_grep_fallback_with_mock_scout() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        // MockLawyer returns Ok(None) — triggers grep fallback
        let lawyer = Arc::new(MockLawyer::default());

        // Use NoOpLawyer (NoLspAvailable path) + MockScout with results
        let ws_dir = tempdir().expect("temp dir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        // Write a file so search can find it
        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/other.rs"),
            "fn login() -> bool { true }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/other.rs".to_string(),
                line: 1,
                column: 1,
                content: "fn login() -> bool { true }".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                version_hash: "sha256:abc".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
        }));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = GetDefinitionParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.get_definition_impl(params).await;
        let Ok(res) = result else {
            panic!("expected Ok with grep fallback, got Err");
        };
        // Should return degraded result from grep
        assert!(res.0.degraded);
        assert_eq!(res.0.file, "src/other.rs");
        assert!(res
            .0
            .degraded_reason
            .as_ref()
            .unwrap()
            .contains("grep_fallback"));
    }

    // ── analyze_impact with empty hierarchy (confirmed zero callers) ───────

    #[tokio::test]
    async fn test_analyze_impact_empty_hierarchy_confirmed_zero() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Return empty items — LSP confirmed zero callers
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
        };
        let result = server.analyze_impact_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::AnalyzeImpactMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // NOT degraded — LSP was available and confirmed zero
        assert!(!val.degraded);
        assert_eq!(val.degraded_reason, None);
        let incoming = val.incoming.as_ref().expect("must be Some");
        let outgoing = val.outgoing.as_ref().expect("must be Some");
        assert!(incoming.is_empty(), "confirmed zero callers");
        assert!(outgoing.is_empty(), "confirmed zero callees");
    }

    // ── analyze_impact with LSP error on call_hierarchy_prepare ────────────

    #[tokio::test]
    async fn test_analyze_impact_lsp_error_degrades() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Simulate LSP protocol error
        lawyer.push_prepare_call_hierarchy_result(Err("LSP crashed".to_string()));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 2,
        };
        let result = server.analyze_impact_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::AnalyzeImpactMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Degraded due to LSP error
        assert!(val.degraded);
        assert_eq!(val.degraded_reason.as_deref(), Some("no_lsp"));
    }

    // ── read_with_deep_context with outgoing call error ───────────────────

    #[tokio::test]
    async fn test_read_with_deep_context_outgoing_error_degrades() {
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
        // Prepare succeeds
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
        // But outgoing call fails
        lawyer.push_outgoing_call_result(Err("outgoing failed".to_string()));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Degraded because outgoing call failed
        assert!(val.degraded);
        assert_eq!(val.degraded_reason.as_deref(), Some("no_lsp"));
        assert!(val.dependencies.is_empty());
    }

    // ── read_with_deep_context with empty hierarchy (confirmed zero deps) ──

    #[tokio::test]
    async fn test_read_with_deep_context_empty_hierarchy_zero_deps() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .push(Ok(make_scope()));

        let lawyer = Arc::new(MockLawyer::default());
        // Empty items = LSP confirmed zero deps
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // NOT degraded — LSP confirmed zero
        assert!(!val.degraded);
        assert_eq!(val.degraded_reason, None);
        assert!(val.dependencies.is_empty());
    }

    // ── analyze_impact BFS depth limiting ────────────────────────────────

    #[tokio::test]
    async fn test_analyze_impact_bfs_respects_max_depth() {
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

        let params = AnalyzeImpactParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_depth: 1, // Should stop after first level
        };
        let result = server.analyze_impact_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::AnalyzeImpactMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        let _incoming = val.incoming.as_ref().expect("must be Some");
        // With max_depth=1, BFS processes the initial item at depth 0, finds caller at depth 1,
        // but the second-level caller (depth 2) should NOT be included
        // However depth_reached should be 1
        assert_eq!(val.depth_reached, 1);
    }
}
