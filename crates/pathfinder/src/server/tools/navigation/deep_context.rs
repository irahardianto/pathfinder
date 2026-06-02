//! `read_with_deep_context` tool handler.
//!
//! Returns the symbol's source code enriched with LSP call-hierarchy
//! dependencies. Degrades gracefully to symbol scope only when no LSP
//! is configured.

use crate::server::helpers::{
    format_degraded_notice, millis_to_u64, parse_semantic_path, pathfinder_to_error_data,
    require_symbol_target, serialize_metadata,
};
use crate::server::types::ReadWithDeepContextParams;
use crate::server::PathfinderServer;
use pathfinder_common::types::DegradedReason;
use pathfinder_lsp::LspError;
use rmcp::model::{CallToolResult, ErrorData};

use super::{
    extract_call_candidates, is_source_file, is_workspace_file, language_to_file_glob,
    LspResolution,
};

impl PathfinderServer {
    /// Resolve LSP call-hierarchy dependencies for a symbol.
    ///
    /// PATCH-005: When LSP is degraded, falls back to grep-based dependency discovery
    /// by parsing the symbol body for function calls and resolving each via search.
    ///
    /// Extracted from `read_with_deep_context` to reduce nesting depth.
    /// Prepares the call hierarchy, then fetches outgoing calls and
    /// maps them to `DeepContextDependency` entries. Includes LSP warmup
    /// retry logic (3-second wait + re-probe) mirroring `get_definition_impl`.
    #[expect(
        clippy::too_many_lines,
        reason = "Call-hierarchy resolution with LSP warmup probe + retry + grep fallback. Linear structure for readability."
    )]
    async fn resolve_lsp_dependencies(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        start_line: usize,
        name_column: usize,
        project_only: bool,
        max_dependencies: u32,
    ) -> LspResolution {
        let mut dependencies = Vec::new();
        let mut degraded = true;
        let mut degraded_reason = Some(DegradedReason::NoLsp);
        let mut engines = vec!["tree-sitter"];
        let mut dependencies_truncated = false;

        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(start_line + 1).unwrap_or(1),
                u32::try_from(name_column + 1).unwrap_or(1),
            )
            .await;

        match lsp_result {
            Ok(items) if !items.is_empty() => {
                dependencies_truncated = self
                    .append_outgoing_deps(
                        &items[0],
                        &mut dependencies,
                        &mut engines,
                        &mut degraded,
                        &mut degraded_reason,
                        project_only,
                        max_dependencies,
                    )
                    .await;
            }
            Ok(_) => {
                let probe = self
                    .lawyer
                    .goto_definition(
                        self.workspace_root.path(),
                        &semantic_path.file_path,
                        u32::try_from(start_line + 1).unwrap_or(1),
                        u32::try_from(name_column + 1).unwrap_or(1),
                    )
                    .await;

                if matches!(probe, Ok(Some(_))) {
                    engines.push("lsp");
                    degraded = false;
                    degraded_reason = None;
                } else {
                    engines.push("lsp");

                    tracing::info!(
                        tool = "read_with_deep_context",
                        semantic_path = %semantic_path,
                        "read_with_deep_context: call_hierarchy_prepare returned [] and goto_definition \
                         probe returned no result — LSP likely warming up, waiting 3s and retrying"
                    );

                    tokio::time::sleep(std::time::Duration::from_secs(3)).await;

                    let retry_result = self
                        .lawyer
                        .call_hierarchy_prepare(
                            self.workspace_root.path(),
                            &semantic_path.file_path,
                            u32::try_from(start_line + 1).unwrap_or(1),
                            u32::try_from(name_column + 1).unwrap_or(1),
                        )
                        .await;

                    match retry_result {
                        Ok(retry_items) if !retry_items.is_empty() => {
                            tracing::info!(
                                tool = "read_with_deep_context",
                                semantic_path = %semantic_path,
                                "read_with_deep_context: call_hierarchy_prepare succeeded on retry after warmup wait"
                            );
                            dependencies_truncated = self
                                .append_outgoing_deps(
                                    &retry_items[0],
                                    &mut dependencies,
                                    &mut engines,
                                    &mut degraded,
                                    &mut degraded_reason,
                                    project_only,
                                    max_dependencies,
                                )
                                .await;
                        }
                        _ => {
                            tracing::info!(
                                tool = "read_with_deep_context",
                                semantic_path = %semantic_path,
                                "read_with_deep_context: retry also returned empty — attempting grep fallback (PATCH-005)"
                            );
                            (degraded, degraded_reason, dependencies_truncated) = self
                                .attempt_grep_fallback(
                                    semantic_path,
                                    &mut dependencies,
                                    &mut engines,
                                    project_only,
                                    max_dependencies,
                                )
                                .await;
                        }
                    }
                }
            }
            Err(LspError::NoLspAvailable | LspError::UnsupportedCapability { .. }) => {
                tracing::info!(
                    tool = "read_with_deep_context",
                    semantic_path = %semantic_path,
                    "read_with_deep_context: NoLspAvailable — attempting grep fallback (PATCH-005)"
                );
                (degraded, degraded_reason, dependencies_truncated) = self
                    .attempt_grep_fallback(
                        semantic_path,
                        &mut dependencies,
                        &mut engines,
                        project_only,
                        max_dependencies,
                    )
                    .await;
            }
            Err(e) => {
                tracing::warn!(
                    tool = "read_with_deep_context",
                    error = %e,
                    "call_hierarchy_prepare failed — attempting grep fallback (PATCH-005)"
                );
                (degraded, degraded_reason, dependencies_truncated) = self
                    .attempt_grep_fallback(
                        semantic_path,
                        &mut dependencies,
                        &mut engines,
                        project_only,
                        max_dependencies,
                    )
                    .await;
            }
        }

        LspResolution {
            dependencies,
            degraded,
            degraded_reason,
            engines,
            dependencies_truncated,
        }
    }

    /// PATCH-005: Resolve a candidate function name to its definition using grep search.
    async fn resolve_candidate_via_grep(
        &self,
        candidate: &str,
        language: &str,
        max_results_per_candidate: usize,
    ) -> Option<(String, u32, String)> {
        let pattern = match language {
            "rust" => format!(r"(?:(?:pub\s*(?:\([^)]*\)\s*)?(?:async\s*)?)?fn\s+{candidate}\b"),
            "go" => format!(r"func\s+{candidate}\b"),
            "typescript" | "javascript" => {
                format!(
                    r"(?:(?:export\s+(?:default\s*)?)?function\s+{candidate}\b|(?:{candidate}\s*:\s*)[^{{]*\([^)]*\)\s*=>)"
                )
            }
            "python" => format!(r"(?:async\s+)?def\s+{candidate}\b"),
            "java" => {
                format!(
                    r"(?:(?:public|private|protected|static|final|synchronized|native|abstract|transient)\s+)*[A-Z][a-zA-Z0-9_]*\s+{candidate}\b"
                )
            }
            _ => format!(r"\b(?:fn|def|function|class|struct|type|interface)\s+{candidate}\b"),
        };

        self.scout
            .search(&pathfinder_search::SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: pattern,
                is_regex: true,
                max_results: max_results_per_candidate,
                path_glob: language_to_file_glob(language).to_string(),
                exclude_glob: String::default(),
                context_lines: 0,
                offset: 0,
            })
            .await
            .ok()
            .and_then(|result| {
                result.matches.first().map(|m| {
                    (
                        m.file.clone(),
                        u32::try_from(m.line).unwrap_or(u32::MAX),
                        m.content.clone(),
                    )
                })
            })
    }

    /// PATCH-005: Attempt grep-based dependency discovery when LSP is unavailable.
    async fn attempt_grep_fallback(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        dependencies: &mut Vec<crate::server::types::DeepContextDependency>,
        engines: &mut Vec<&'static str>,
        project_only: bool,
        max_dependencies: u32,
    ) -> (bool, Option<DegradedReason>, bool) {
        let scope_result = {
            let Ok(s) = self
                .surgeon
                .read_symbol_scope(self.workspace_root.path(), semantic_path)
                .await
            else {
                tracing::warn!(
                    tool = "read_with_deep_context",
                    semantic_path = %semantic_path,
                    "PATCH-005: failed to read symbol scope for grep fallback"
                );
                return (true, Some(DegradedReason::GrepFallbackDependencies), false);
            };
            s
        };

        let language = &scope_result.language;
        let candidates = extract_call_candidates(&scope_result.content, language);

        if candidates.is_empty() {
            tracing::info!(
                tool = "read_with_deep_context",
                semantic_path = %semantic_path,
                "PATCH-005: grep fallback found no call candidates"
            );
            return (true, Some(DegradedReason::GrepFallbackDependencies), false);
        }

        tracing::info!(
            tool = "read_with_deep_context",
            semantic_path = %semantic_path,
            candidate_count = candidates.len(),
            "PATCH-005: grep fallback resolving {} candidates",
            candidates.len()
        );

        let max_deps = max_dependencies as usize;
        let mut truncated = false;

        for candidate in candidates {
            if dependencies.len() >= max_deps {
                truncated = true;
                break;
            }

            if let Some((file, line, signature)) = self
                .resolve_candidate_via_grep(&candidate, language, 2)
                .await
            {
                if project_only && (!is_source_file(&file) || !is_workspace_file(&file)) {
                    continue;
                }

                let dep_path = format!("{file}::{candidate}");
                // Item 1: Dedup by semantic_path to avoid duplicates when
                // multiple candidates resolve to the same definition.
                if dependencies.iter().any(|d| d.semantic_path == dep_path) {
                    continue;
                }
                dependencies.push(crate::server::types::DeepContextDependency {
                    semantic_path: dep_path,
                    signature,
                    file,
                    line: line as usize,
                });
            }
        }

        engines.push("ripgrep");
        tracing::info!(
            tool = "read_with_deep_context",
            semantic_path = %semantic_path,
            resolved_count = dependencies.len(),
            "PATCH-005: grep fallback resolved {} dependencies",
            dependencies.len()
        );

        (
            true,
            Some(DegradedReason::GrepFallbackDependencies),
            truncated,
        )
    }

    /// Fetch outgoing call-hierarchy items and append them as dependencies.
    /// Returns `true` if results were truncated due to `max_dependencies` limit.
    #[expect(
        clippy::too_many_arguments,
        reason = "All parameters are logically distinct mutable references required by the BFS caller; grouping into a struct would obscure ownership."
    )]
    async fn append_outgoing_deps(
        &self,
        item: &pathfinder_lsp::types::CallHierarchyItem,
        dependencies: &mut Vec<crate::server::types::DeepContextDependency>,
        engines: &mut Vec<&'static str>,
        degraded: &mut bool,
        degraded_reason: &mut Option<DegradedReason>,
        project_only: bool,
        max_dependencies: u32,
    ) -> bool {
        let mut truncated = false;
        match self
            .lawyer
            .call_hierarchy_outgoing(self.workspace_root.path(), item)
            .await
        {
            Ok(outgoing) => {
                engines.push("lsp");
                for call in outgoing {
                    if dependencies.len() >= max_dependencies as usize {
                        truncated = true;
                        break;
                    }

                    let callee = &call.item;

                    // Filter out non-workspace files (stdlib, dependencies) when project_only
                    if project_only
                        && (!is_source_file(&callee.file) || !is_workspace_file(&callee.file))
                    {
                        continue;
                    }

                    let signature = callee.detail.clone().unwrap_or_else(|| callee.name.clone());
                    let sp = format!("{}::{}", callee.file, callee.name);
                    // Dedup by semantic_path to avoid duplicates from LSP returning
                    // the same callee multiple times.
                    if dependencies.iter().any(|d| d.semantic_path == sp) {
                        continue;
                    }
                    dependencies.push(crate::server::types::DeepContextDependency {
                        semantic_path: sp,
                        signature,
                        file: callee.file.clone(),
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
                // Item 3: Set specific reason instead of keeping stale default.
                // The prepare call succeeded but outgoing deps failed —
                // this is a partial LSP failure, not a complete absence.
                *degraded = true;
                *degraded_reason = Some(DegradedReason::LspErrorGrepFallback);
            }
        }
        truncated
    }

    /// Core logic for the `read_with_deep_context` tool.
    ///
    /// Returns the symbol's source code. When LSP is available, appends the
    /// signatures of all called symbols. Degrades gracefully to symbol scope
    /// only when no LSP is configured.
    #[expect(
        clippy::too_many_lines,
        reason = "Sequential pipeline: parse → sandbox → TS → LSP → dep-block rendering. Linear structure is intentional for readability."
    )]
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

        // Early file existence check — avoid tree-sitter parse on nonexistent files
        let abs_file = self.workspace_root.path().join(&semantic_path.file_path);
        if !abs_file.exists() {
            let err = pathfinder_common::error::PathfinderError::FileNotFound {
                path: abs_file.clone(),
            };
            tracing::warn!(
                tool = "read_with_deep_context",
                path = %abs_file.display(),
                "file not found"
            );
            return Err(pathfinder_to_error_data(&err));
        }

        // Fetch the symbol scope (Tree-sitter)
        let ts_start = std::time::Instant::now();
        let scope = self
            .read_symbol_scope_enriched(&semantic_path, &params.semantic_path)
            .await?;
        let tree_sitter_ms = ts_start.elapsed().as_millis();

        // IW-3 (DS-1 gap fix): RAII document lifecycle — did_close fires on all exits.
        let file_path = self.workspace_root.path().join(&semantic_path.file_path);
        let file_content = match tokio::fs::read_to_string(&file_path).await {
            Ok(content) => content,
            Err(e) => {
                tracing::warn!(
                    tool = "read_with_deep_context",
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
                    tool = "read_with_deep_context",
                    semantic_path = %semantic_path,
                    error = %e,
                    "open_document failed — LSP queries may return degraded results"
                );
                None
            }
        };

        let project_only = params.project_only.unwrap_or(true);
        let max_dependencies = params.max_dependencies;

        let lsp_start = std::time::Instant::now();

        let LspResolution {
            dependencies,
            degraded,
            degraded_reason,
            engines,
            dependencies_truncated,
        } = self
            .resolve_lsp_dependencies(
                &semantic_path,
                scope.start_line,
                scope.name_column,
                project_only,
                max_dependencies,
            )
            .await;

        // Note: `_doc_guard` still alive here; drops at function return.
        let lsp_ms = lsp_start.elapsed().as_millis();
        let duration_ms = start.elapsed().as_millis();

        let degraded_reason_str = degraded_reason.as_ref().map(ToString::to_string);
        tracing::info!(
            tool = "read_with_deep_context",
            semantic_path = %params.semantic_path,
            tree_sitter_ms,
            lsp_ms,
            duration_ms,
            degraded,
            degraded_reason = ?degraded_reason_str,
            engines_used = ?engines,
            "read_with_deep_context: complete"
        );

        let dep_count = dependencies.len();
        let lsp_readiness = if degraded {
            match degraded_reason {
                Some(
                    DegradedReason::LspWarmupEmptyUnverified
                    | DegradedReason::LspWarmupGrepFallback,
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
        let resolution_strategy = if engines.contains(&"lsp") {
            Some("lsp_call_hierarchy".to_owned())
        } else if degraded {
            // Distinguish: LSP was never available vs LSP failed vs grep fallback.
            match degraded_reason {
                Some(DegradedReason::NoLsp) => Some("treesitter_direct".to_owned()),
                Some(DegradedReason::GrepFallbackDependencies) => Some("grep_fallback".to_owned()),
                _ => Some("treesitter_fallback".to_owned()),
            }
        } else {
            Some("treesitter_direct".to_owned())
        };
        let metadata = crate::server::types::ReadWithDeepContextMetadata {
            start_line: scope.start_line,
            end_line: scope.end_line,
            language: scope.language,
            dependencies,
            degraded,
            degraded_reason,
            actionable_guidance: degraded_reason.as_ref().map(DegradedReason::guidance),
            lsp_readiness,
            warm_start_in_progress,
            dependencies_truncated,
            resolution_strategy,
            duration_ms: Some(millis_to_u64(duration_ms)),
        };

        // Build the dependency block: list each callee signature, file, and line.
        // This surfaces the same data as structured_content.dependencies in plain text
        // so agents reading the text channel don't need to parse JSON.
        let dep_block: String = if metadata.dependencies.is_empty() {
            String::new()
        } else {
            let mut lines = Vec::with_capacity(metadata.dependencies.len());
            for dep in &metadata.dependencies {
                lines.push(format!("  {} ({}:L{})", dep.signature, dep.file, dep.line));
            }
            format!("\n{}", lines.join("\n"))
        };

        // Prepend degradation notice when in degraded mode
        let text = if degraded {
            let notice = degraded_reason
                .as_ref()
                .map_or_else(|| "DEGRADED (unknown)".to_owned(), format_degraded_notice);
            format!(
                "{notice}\n\n{dep_count} dependencies loaded{dep_block}\n\n{}\n[completed in {duration_ms}ms]",
                scope.content
            )
        } else {
            format!(
                "{dep_count} dependencies loaded{dep_block}\n\n{}\n[completed in {duration_ms}ms]",
                scope.content
            )
        };
        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
mod tests {
    use super::super::test_helpers::{make_scope, make_server_with_lawyer, make_temp_workspace};
    use crate::server::types::ReadWithDeepContextParams;
    use crate::server::PathfinderServer;
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::{DegradedReason, WorkspaceRoot};
    use pathfinder_lsp::types::{CallHierarchyCall, CallHierarchyItem};
    use pathfinder_lsp::{DefinitionLocation, MockLawyer};
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use std::sync::Arc;

    // ── read_with_deep_context ────────────────────────────────────────

    #[tokio::test]
    async fn test_read_with_deep_context_degrades_when_call_hierarchy_unsupported() {
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon.read_symbol_scope_results.lock().unwrap().extend([
            Ok(make_scope()),
            Ok(make_scope()),
            Ok(make_scope()),
        ]);

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

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            ..Default::default()
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let text_content = match &call_res.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(
            text_content.starts_with("DEGRADED (grep_fallback_dependencies) — results are heuristic (grep-based), verify manually — fallback: use search_codebase for authoritative results\n\n0 dependencies loaded\n\nfn login() { }"),
            "text_content: {text_content}"
        );
        assert!(val.degraded);
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::GrepFallbackDependencies)
        );
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
            ..Default::default()
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let text_content = match &call_res.content[0].raw {
            rmcp::model::RawContent::Text(t) => t.text.clone(),
            _ => panic!("expected text content"),
        };
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(
            text_content.starts_with("1 dependencies loaded\n  fn validate_token() -> bool (src/token.rs:L15)\n\nfn login() { }"),
            "text_content: {text_content}"
        );
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
        lawyer.push_outgoing_call_result(Err(pathfinder_lsp::LspError::Protocol(
            "outgoing failed".to_string(),
        )));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            ..Default::default()
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // Degraded because outgoing call failed
        assert!(val.degraded);
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::LspErrorGrepFallback)
        );
        assert!(val.dependencies.is_empty());
    }

    // ── read_with_deep_context with empty hierarchy (confirmed zero deps) ──

    #[tokio::test]
    async fn test_read_with_deep_context_empty_hierarchy_zero_deps() {
        // call_hierarchy_prepare returns Ok([]) AND goto_definition probe returns Ok(Some(...))
        // → LSP is warm, confirmed zero deps. Must NOT be degraded.
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

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            ..Default::default()
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // NOT degraded — LSP warm, genuinely zero deps confirmed
        assert!(
            !val.degraded,
            "must not be degraded when probe confirms LSP is warm"
        );
        assert_eq!(val.degraded_reason, None);
        assert!(val.dependencies.is_empty(), "confirmed zero dependencies");
    }

    #[tokio::test]
    async fn test_read_with_deep_context_empty_hierarchy_warmup_degrades() {
        // call_hierarchy_prepare returns Ok([]) AND goto_definition probe returns Ok(None)
        // → LSP is warming up. Falls through to grep fallback (PATCH-005).
        let surgeon = Arc::new(MockSurgeon::new());
        surgeon.read_symbol_scope_results.lock().unwrap().extend([
            Ok(make_scope()),
            Ok(make_scope()),
            Ok(make_scope()),
        ]);

        let lawyer = Arc::new(MockLawyer::default());
        // Empty call hierarchy
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));
        // Probe: goto_definition returns Ok(None) → LSP is still warming up
        // MockLawyer::default() already returns Ok(None) for goto_definition, so no extra setup needed.

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            ..Default::default()
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        // DEGRADED — grep fallback used because LSP warmup returns empty hierarchy
        assert!(
            val.degraded,
            "must be degraded when goto_definition probe also returns None"
        );
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::GrepFallbackDependencies),
            "degraded_reason must indicate grep fallback was used"
        );
        assert!(val.dependencies.is_empty());
    }

    #[tokio::test]
    async fn test_read_with_deep_context_closes_document_on_success() {
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
        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            ..Default::default()
        };

        let _ = server.read_with_deep_context_impl(params).await;

        tokio::task::yield_now().await;

        assert_eq!(
            lawyer.did_open_call_count(),
            lawyer.did_close_call_count(),
            "DS-1: did_open and did_close must be symmetric in read_with_deep_context"
        );
    }

    // ── TASK-7: max_dependencies truncation ───────────────────────────────────

    /// When outgoing dependencies exceed `max_dependencies`, the result must be
    /// truncated and `dependencies_truncated = true`.
    #[tokio::test]
    async fn test_read_with_deep_context_max_dependencies_truncates_results() {
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

        // Push 5 outgoing callees (each on a distinct file)
        let outgoing_calls: Vec<CallHierarchyCall> = (1..=5)
            .map(|i| CallHierarchyCall {
                item: CallHierarchyItem {
                    name: format!("dep_{i}"),
                    kind: "function".into(),
                    detail: Some(format!("fn dep_{i}()")),
                    file: format!("src/dep_{i}.rs"),
                    line: i * 5,
                    column: 4,
                    data: None,
                },
                call_sites: vec![i * 5],
            })
            .collect();
        lawyer.push_outgoing_call_result(Ok(outgoing_calls));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            max_dependencies: 2, // cap below the 5 available
            ..Default::default()
        };
        let result = server
            .read_with_deep_context_impl(params)
            .await
            .expect("should succeed");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(result.structured_content.unwrap()).unwrap();

        assert_eq!(
            val.dependencies.len(),
            2,
            "dependencies must be capped at max_dependencies=2"
        );
        assert!(
            val.dependencies_truncated,
            "dependencies_truncated must be true when budget is exhausted"
        );
    }

    /// Verify that the `default_max_dependencies()` constant is 50.
    #[test]
    fn test_read_with_deep_context_default_max_dependencies_is_50() {
        use crate::server::types::default_max_dependencies;
        assert_eq!(
            default_max_dependencies(),
            50,
            "default_max_dependencies must be 50 per the implementation"
        );
    }

    // ── attempt_grep_fallback with resolved candidates ───────────────

    #[tokio::test]
    async fn test_read_with_deep_context_grep_fallback_resolves_candidates() {
        // PATCH-005: When LSP is unavailable, grep fallback extracts call candidates
        // from the symbol body and resolves each via search.
        let surgeon = Arc::new(MockSurgeon::new());
        // First call: read_symbol_scope_enriched (for the main function)
        // Second call: read_symbol_scope (for grep fallback candidate extraction)
        // Use scope content that contains a function call to exercise candidate extraction.
        let mut scope = make_scope();
        scope.content = "fn login() -> bool { validate_token() }".to_string();
        surgeon
            .read_symbol_scope_results
            .lock()
            .unwrap()
            .extend([Ok(scope.clone()), Ok(scope)]);

        let ws_dir = make_temp_workspace();
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);

        std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
        std::fs::write(
            ws_dir.path().join("src/auth.rs"),
            "fn login() -> bool { validate_token() }",
        )
        .unwrap();

        let scout = Arc::new(MockScout::default());
        // First search: resolve "validate_token" candidate
        scout.set_result(Ok(pathfinder_search::SearchResult {
            matches: vec![pathfinder_search::SearchMatch {
                file: "src/token.rs".to_string(),
                line: 5,
                column: 1,
                content: "fn validate_token() -> bool { true }".to_string(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:abc".to_string(),
                known: Some(false),
            }],
            total_matches: 1,
            truncated: false,
            files_searched: 1,
            files_in_scope: 1,
        }));

        let server = PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            scout,
            surgeon,
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            ..Default::default()
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(val.degraded);
        assert_eq!(
            val.degraded_reason,
            Some(DegradedReason::GrepFallbackDependencies)
        );
        // The grep fallback should have resolved "validate_token" from the scope body.
        assert!(
            val.dependencies.len() >= 1,
            "expected at least 1 resolved dependency, got {}",
            val.dependencies.len()
        );
    }

    // ── detail: None fallback ────────────────────────────────────────

    #[tokio::test]
    async fn test_read_with_deep_context_detail_none_falls_back_to_name() {
        // When callee.detail is None, the signature should fall back to callee.name.
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

        // Outgoing call with detail=None
        lawyer.push_outgoing_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "validate_token".into(),
                kind: "function".into(),
                detail: None, // No detail — should fall back to name
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
            ..Default::default()
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        assert_eq!(val.dependencies.len(), 1);
        // Signature should be the name since detail is None
        assert_eq!(val.dependencies[0].signature, "validate_token");
        assert_eq!(
            val.dependencies[0].semantic_path,
            "src/token.rs::validate_token"
        );
    }

    // ── Warmup retry success ────────────────────────────────────────

    #[tokio::test]
    async fn test_read_with_deep_context_warmup_retry_success() {
        // LSP returns Ok([]) first (warmup), then Ok(items) on retry.
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

        // First call: empty (warmup)
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![]));
        // goto_definition probe: Ok(None) — confirms LSP warming up
        // (default MockLawyer returns Ok(None))
        // Retry call: succeeds with items
        lawyer.push_prepare_call_hierarchy_result(Ok(vec![item]));
        // Outgoing deps for retry
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
            ..Default::default()
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed on retry");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded, "should NOT be degraded on retry success");
        assert_eq!(val.dependencies.len(), 1);
        assert_eq!(
            val.dependencies[0].semantic_path,
            "src/token.rs::validate_token"
        );
    }

    // ── project_only filtering in append_outgoing_deps ──────────────

    #[tokio::test]
    async fn test_read_with_deep_context_filters_non_workspace_deps() {
        // When project_only=true, callees from non-workspace files should be filtered.
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

        // Outgoing: one workspace dep + one non-workspace (absolute path = stdlib)
        lawyer.push_outgoing_call_result(Ok(vec![
            CallHierarchyCall {
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
            },
            CallHierarchyCall {
                item: CallHierarchyItem {
                    name: "println".into(),
                    kind: "function".into(),
                    detail: Some("macro println".into()),
                    file: "/rust/library/std/src/io/stdio.rs".into(), // absolute = non-workspace
                    line: 100,
                    column: 1,
                    data: None,
                },
                call_sites: vec![9],
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            project_only: Some(true),
            ..Default::default()
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        // Only the workspace dep should be included
        assert_eq!(val.dependencies.len(), 1);
        assert_eq!(
            val.dependencies[0].semantic_path,
            "src/token.rs::validate_token"
        );
    }

    // ── Dedup in append_outgoing_deps ────────────────────────────────

    #[tokio::test]
    async fn test_read_with_deep_context_deduplicates_deps() {
        // When LSP returns the same callee multiple times, dedup should prevent duplicates.
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

        // Outgoing: same callee returned twice (simulates LSP duplicate)
        lawyer.push_outgoing_call_result(Ok(vec![
            CallHierarchyCall {
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
            },
            CallHierarchyCall {
                item: CallHierarchyItem {
                    name: "validate_token".into(),
                    kind: "function".into(),
                    detail: Some("fn validate_token()".into()),
                    file: "src/token.rs".into(),
                    line: 15,
                    column: 4,
                    data: None,
                },
                call_sites: vec![10],
            },
        ]));

        let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

        let params = ReadWithDeepContextParams {
            semantic_path: "src/auth.rs::login".to_owned(),
            ..Default::default()
        };
        let result = server.read_with_deep_context_impl(params).await;
        let call_res = result.expect("should succeed");
        let val: crate::server::types::ReadWithDeepContextMetadata =
            serde_json::from_value(call_res.structured_content.unwrap()).unwrap();

        assert!(!val.degraded);
        // Dedup should prevent the duplicate
        assert_eq!(
            val.dependencies.len(),
            1,
            "duplicate callees should be deduped, got {}",
            val.dependencies.len()
        );
    }
}
