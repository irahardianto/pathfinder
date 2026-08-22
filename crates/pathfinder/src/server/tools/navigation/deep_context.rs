//! `inspect` tool handler (deep context mode).
//!
//! Returns the symbol's source code enriched with LSP call-hierarchy
//! dependencies. Degrades gracefully to symbol scope only when no LSP
//! is configured.

use crate::server::helpers::{
    format_degraded_notice, invalid_params_error, millis_to_u64, parse_semantic_path,
    pathfinder_to_error_data, require_symbol_target, serialize_metadata,
};
use crate::server::types::InspectParams;
use crate::server::PathfinderServer;
use pathfinder_common::types::DegradedReason;
use rmcp::model::{CallToolResult, ErrorData};

use super::{
    candidate_definition_pattern, extract_call_candidates, is_source_file, is_workspace_file,
    language_to_file_glob, LspResolution,
};

struct AppendOutgoingArgs<'a> {
    item: &'a pathfinder_lsp::types::CallHierarchyItem,
    dependencies: &'a mut Vec<crate::server::types::DeepContextDependency>,
    engines: &'a mut Vec<&'static str>,
    degraded: &'a mut bool,
    degraded_reason: &'a mut Option<DegradedReason>,
    project_only: bool,
    max_dependencies: u32,
}

struct EmptyPrepareArgs<'a> {
    semantic_path: &'a pathfinder_common::types::SemanticPath,
    start_line: usize,
    name_column: usize,
    project_only: bool,
    max_dependencies: u32,
    dependencies: &'a mut Vec<crate::server::types::DeepContextDependency>,
    engines: &'a mut Vec<&'static str>,
}

impl PathfinderServer {
    /// Resolve LSP call-hierarchy dependencies for a symbol.
    async fn resolve_lsp_dependencies(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        start_line: usize,
        name_column: usize,
        project_only: bool,
        max_dependencies: u32,
    ) -> LspResolution {
        let mut dependencies = Vec::new();
        let mut engines = vec!["tree-sitter"];

        let lsp_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(start_line + 1).unwrap_or(1),
                u32::try_from(name_column + 1).unwrap_or(1),
            )
            .await;

        let (degraded, degraded_reason, dependencies_truncated) = match lsp_result {
            Ok(items) if !items.is_empty() => {
                let mut degraded = true;
                let mut degraded_reason = Some(DegradedReason::NoLsp);
                let trunc = self
                    .append_outgoing_deps(AppendOutgoingArgs {
                        item: &items[0],
                        dependencies: &mut dependencies,
                        engines: &mut engines,
                        degraded: &mut degraded,
                        degraded_reason: &mut degraded_reason,
                        project_only,
                        max_dependencies,
                    })
                    .await;
                (degraded, degraded_reason, trunc)
            }
            Ok(_) => {
                self.handle_empty_call_hierarchy_prepare(EmptyPrepareArgs {
                    semantic_path,
                    start_line,
                    name_column,
                    project_only,
                    max_dependencies,
                    dependencies: &mut dependencies,
                    engines: &mut engines,
                })
                .await
            }
            Err(e) => {
                tracing::info!(
                    tool = "read_with_deep_context",
                    error = %e,
                    semantic_path = %semantic_path,
                    "call_hierarchy_prepare failed — attempting grep fallback"
                );
                self.attempt_grep_fallback(
                    semantic_path,
                    &mut dependencies,
                    &mut engines,
                    project_only,
                    max_dependencies,
                )
                .await
            }
        };

        LspResolution {
            dependencies,
            degraded,
            degraded_reason,
            engines,
            dependencies_truncated,
        }
    }

    async fn handle_empty_call_hierarchy_prepare(
        &self,
        args: EmptyPrepareArgs<'_>,
    ) -> (bool, Option<DegradedReason>, bool) {
        let EmptyPrepareArgs {
            semantic_path,
            start_line,
            name_column,
            project_only,
            max_dependencies,
            dependencies,
            engines,
        } = args;
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
            return (false, None, false);
        }

        engines.push("lsp");
        tracing::info!(
            tool = "read_with_deep_context",
            semantic_path = %semantic_path,
            "read_with_deep_context: call_hierarchy_prepare returned [] and goto_definition \
             probe returned no result — LSP likely warming up, waiting 1s and retrying"
        );

        tokio::time::sleep(std::time::Duration::from_secs(1)).await;

        let retry_result = self
            .lawyer
            .call_hierarchy_prepare(
                self.workspace_root.path(),
                &semantic_path.file_path,
                u32::try_from(start_line + 1).unwrap_or(1),
                u32::try_from(name_column + 1).unwrap_or(1),
            )
            .await;

        if let Ok(retry_items) = retry_result {
            if !retry_items.is_empty() {
                let mut degraded = true;
                let mut degraded_reason = Some(DegradedReason::NoLsp);
                let trunc = self
                    .append_outgoing_deps(AppendOutgoingArgs {
                        item: &retry_items[0],
                        dependencies,
                        engines,
                        degraded: &mut degraded,
                        degraded_reason: &mut degraded_reason,
                        project_only,
                        max_dependencies,
                    })
                    .await;
                return (degraded, degraded_reason, trunc);
            }
        }

        self.attempt_grep_fallback(
            semantic_path,
            dependencies,
            engines,
            project_only,
            max_dependencies,
        )
        .await
    }

    /// Attempt grep-based dependency discovery when LSP is unavailable.
    /// Uses batch candidate resolution with a single alternation pattern pass (RSP-A10).
    async fn attempt_grep_fallback(
        &self,
        semantic_path: &pathfinder_common::types::SemanticPath,
        dependencies: &mut Vec<crate::server::types::DeepContextDependency>,
        engines: &mut Vec<&'static str>,
        project_only: bool,
        max_dependencies: u32,
    ) -> (bool, Option<DegradedReason>, bool) {
        let Ok(scope_result) = self
            .surgeon
            .read_symbol_scope(self.workspace_root.path(), semantic_path)
            .await
        else {
            return (true, Some(DegradedReason::GrepFallbackDependencies), false);
        };

        let language = &scope_result.language;
        let candidates = extract_call_candidates(&scope_result.content, language);

        if candidates.is_empty() {
            return (true, Some(DegradedReason::GrepFallbackDependencies), false);
        }

        let budget = super::GrepFallbackBudget::new();
        let Some(deadline) = budget.acquire_search() else {
            return (true, Some(DegradedReason::GrepFallbackDependencies), false);
        };

        let max_deps = max_dependencies as usize;
        let alt = candidates
            .iter()
            .map(|c| regex::escape(c))
            .collect::<Vec<_>>()
            .join("|");
        let pattern = candidate_definition_pattern(language, &alt);
        let path_glob = language_to_file_glob(language);

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

        let truncated = if let Ok(result) = search_res {
            self.match_grep_candidates(
                &candidates,
                language,
                &result,
                project_only,
                max_deps,
                dependencies,
            )
            .await
        } else {
            false
        };

        engines.push("ripgrep");
        (
            true,
            Some(DegradedReason::GrepFallbackDependencies),
            truncated,
        )
    }

    async fn match_grep_candidates(
        &self,
        candidates: &[String],
        language: &str,
        result: &pathfinder_search::SearchResult,
        project_only: bool,
        max_deps: usize,
        dependencies: &mut Vec<crate::server::types::DeepContextDependency>,
    ) -> bool {
        let mut truncated = false;
        for candidate in candidates {
            if dependencies.len() >= max_deps {
                truncated = true;
                break;
            }

            let cand_pat = candidate_definition_pattern(language, candidate);
            let cand_re = regex::Regex::new(&cand_pat).ok();
            let found_match = result
                .matches
                .iter()
                .find(|m| cand_re.as_ref().is_some_and(|re| re.is_match(&m.content)));

            if let Some(m) = found_match {
                let file = m.file.clone();
                let line = u32::try_from(m.line).unwrap_or(u32::MAX);
                let signature = m.content.clone();

                if project_only && (!is_source_file(&file) || !is_workspace_file(&file)) {
                    continue;
                }

                let dep_path = self.enrich_semantic_path(&file, line, candidate).await;
                if dependencies.iter().any(|d| d.semantic_path == dep_path) {
                    continue;
                }
                dependencies.push(crate::server::types::DeepContextDependency {
                    semantic_path: dep_path,
                    signature,
                    file,
                    line: line as usize,
                    confidence: pathfinder_common::types::Confidence::Heuristic,
                });
            }
        }
        truncated
    }

    /// Fetch outgoing call-hierarchy items and append them as dependencies.
    /// Returns `true` if results were truncated due to `max_dependencies` limit.
    async fn append_outgoing_deps(&self, args: AppendOutgoingArgs<'_>) -> bool {
        let AppendOutgoingArgs {
            item,
            dependencies,
            engines,
            degraded,
            degraded_reason,
            project_only,
            max_dependencies,
        } = args;
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
                    if project_only
                        && (!is_source_file(&callee.file) || !is_workspace_file(&callee.file))
                    {
                        continue;
                    }

                    let signature = callee.detail.clone().unwrap_or_else(|| callee.name.clone());
                    let sp = self
                        .enrich_semantic_path(&callee.file, callee.line, &callee.name)
                        .await;
                    if dependencies.iter().any(|d| d.semantic_path == sp) {
                        continue;
                    }
                    dependencies.push(crate::server::types::DeepContextDependency {
                        semantic_path: sp,
                        signature,
                        file: callee.file.clone(),
                        line: callee.line as usize,
                        confidence: pathfinder_common::types::Confidence::Verified,
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
                *degraded = true;
                *degraded_reason = Some(DegradedReason::LspErrorGrepFallback);
            }
        }
        truncated
    }

    /// Core logic for the `read_with_deep_context` tool.
    pub(crate) async fn read_with_deep_context_impl(
        &self,
        params: InspectParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();
        let semantic_path_str = params
            .semantic_path
            .as_deref()
            .ok_or_else(|| invalid_params_error("`semantic_path` must be provided"))?;

        let semantic_path = parse_semantic_path(semantic_path_str)?;
        require_symbol_target(&semantic_path, semantic_path_str)?;

        if let Err(e) = self.sandbox.check(&semantic_path.file_path) {
            return Err(pathfinder_to_error_data(&e));
        }

        let abs_file = self.workspace_root.path().join(&semantic_path.file_path);
        if !abs_file.exists() {
            let err = pathfinder_common::error::PathfinderError::FileNotFound { path: abs_file };
            return Err(pathfinder_to_error_data(&err));
        }

        let scope = self
            .read_symbol_scope_enriched(&semantic_path, semantic_path_str)
            .await?;

        let file_path = self.workspace_root.path().join(&semantic_path.file_path);
        let file_content = tokio::fs::read_to_string(&file_path)
            .await
            .unwrap_or_default();
        let _doc_guard = self
            .lawyer
            .open_document(
                self.workspace_root.path(),
                &semantic_path.file_path,
                &file_content,
            )
            .await
            .ok();

        let resolution = self
            .resolve_lsp_dependencies(
                &semantic_path,
                scope.start_line,
                scope.name_column,
                true,
                params.max_dependencies,
            )
            .await;

        let duration_ms = start.elapsed().as_millis();
        let imports = if params.include_imports {
            let abs_file = self.workspace_root.path().join(&semantic_path.file_path);
            self.extract_file_imports(&abs_file).await
        } else {
            Vec::new()
        };

        Ok(Self::build_deep_context_result(
            &scope,
            &resolution,
            imports,
            duration_ms,
            params.retry_attempt,
        ))
    }

    fn build_deep_context_result(
        scope: &pathfinder_common::types::SymbolScope,
        resolution: &LspResolution,
        imports: Vec<String>,
        duration_ms: u128,
        retry_attempt: u32,
    ) -> CallToolResult {
        let LspResolution {
            dependencies,
            degraded,
            degraded_reason,
            engines,
            dependencies_truncated,
        } = resolution;

        let lsp_readiness = if *degraded {
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

        let resolution_strategy = if !*degraded && engines.contains(&"lsp") {
            Some("lsp_call_hierarchy".to_owned())
        } else if *degraded {
            match degraded_reason {
                Some(DegradedReason::NoLsp) => Some("treesitter_direct".to_owned()),
                Some(DegradedReason::GrepFallbackDependencies) => Some("grep_fallback".to_owned()),
                _ => Some("treesitter_fallback".to_owned()),
            }
        } else {
            Some("treesitter_direct".to_owned())
        };

        let status = if *degraded {
            if warm_start_in_progress == Some(true) {
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
                    reason: degraded_reason
                        .as_ref()
                        .map_or_else(|| "degraded".into(), ToString::to_string),
                    retry_after_secs: None,
                }
            }
        } else {
            pathfinder_common::types::ToolStatus::Ok
        };

        let deps_evidence = if *degraded {
            if dependencies.is_empty() {
                None
            } else {
                Some(pathfinder_common::types::Evidence::heuristic(
                    dependencies.clone(),
                ))
            }
        } else {
            Some(pathfinder_common::types::Evidence::verified(
                dependencies.clone(),
            ))
        };

        crate::server::helpers::validate_evidence(&status, deps_evidence.as_ref());

        let metadata = crate::server::types::ReadWithDeepContextMetadata {
            status: status.clone(),
            start_line: scope.start_line,
            end_line: scope.end_line,
            language: scope.language.clone(),
            dependencies: deps_evidence.clone(),
            degraded: *degraded,
            degraded_reason: *degraded_reason,
            actionable_guidance: degraded_reason.as_ref().map(DegradedReason::guidance),
            lsp_readiness,
            warm_start_in_progress,
            dependencies_truncated: *dependencies_truncated,
            next_offset: None,
            resolution_strategy,
            duration_ms: Some(millis_to_u64(duration_ms)),
            imports,
            content: Some(scope.content.clone()),
        };

        let text = Self::render_deep_context_text(
            &status,
            *degraded,
            *degraded_reason,
            deps_evidence.as_ref(),
            &metadata.imports,
            &scope.content,
            duration_ms,
        );

        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = serialize_metadata(&metadata);
        res
    }

    fn render_deep_context_text(
        status: &pathfinder_common::types::ToolStatus,
        degraded: bool,
        degraded_reason: Option<DegradedReason>,
        deps_evidence: Option<
            &pathfinder_common::types::Evidence<crate::server::types::DeepContextDependency>,
        >,
        imports: &[String],
        content: &str,
        duration_ms: u128,
    ) -> String {
        let dep_block: String = match deps_evidence {
            None => "\nDependencies: UNKNOWN (not verified — do NOT treat as zero)".to_string(),
            Some(ev)
                if ev.confidence == pathfinder_common::types::Confidence::Verified
                    && ev.items.is_empty() =>
            {
                "\n0 dependencies loaded (verified zero)".to_string()
            }
            Some(ev) => {
                let dep_count = ev.items.len();
                let conf_str = if ev.confidence == pathfinder_common::types::Confidence::Verified {
                    "(verified)"
                } else {
                    "(heuristic candidates)"
                };
                let mut lines = Vec::with_capacity(ev.items.len() + 1);
                lines.push(format!("\n{dep_count} dependencies loaded {conf_str}"));
                for dep in &ev.items {
                    if dep.confidence == pathfinder_common::types::Confidence::Heuristic {
                        lines.push(format!(
                            "  {} ({}:L{}) [HEURISTIC]",
                            dep.signature, dep.file, dep.line
                        ));
                    } else {
                        lines.push(format!("  {} ({}:L{})", dep.signature, dep.file, dep.line));
                    }
                }
                lines.join("\n")
            }
        };

        let import_block = if imports.is_empty() {
            String::new()
        } else {
            format!("\nImports:\n{}\n", imports.join("\n"))
        };

        let mut text_parts = Vec::new();
        if let Some(header) = crate::server::helpers::format_tool_status_header(status) {
            text_parts.push(header);
        } else if degraded {
            let notice = degraded_reason
                .as_ref()
                .map_or_else(|| "DEGRADED (unknown)".to_owned(), format_degraded_notice);
            text_parts.push(notice);
        }

        text_parts.push(format!(
            "{dep_block}{import_block}\n\n{content}\n[completed in {duration_ms}ms]"
        ));

        text_parts.join("\n")
    }

    /// Extract file-level import/using/require statements from a source file.
    ///
    /// Uses a simple line-scan approach — no tree-sitter parsing required.
    /// Returns at most 200 import lines to prevent context overflow on files
    /// with auto-generated import blocks (e.g., generated Java files).
    ///
    /// Patterns per language:
    /// - Java/Kotlin/C#: lines starting with `import` or `using`
    /// - Python: lines starting with `import` or `from ... import`
    /// - TypeScript/JavaScript: lines starting with `import` or `require`
    /// - Rust: lines starting with `use ` (leading space to exclude `use` in other contexts)
    /// - Go: lines starting with `import` (including multi-line import blocks)
    async fn extract_file_imports(&self, file_path: &std::path::Path) -> Vec<String> {
        const MAX_IMPORTS: usize = 200;
        let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");

        let content = match tokio::fs::read_to_string(file_path).await {
            Ok(c) => c,
            Err(e) => {
                tracing::debug!(
                    tool = "read_with_deep_context",
                    path = %file_path.display(),
                    error = %e,
                    "extract_file_imports: could not read file"
                );
                return Vec::new();
            }
        };

        let mut imports = Vec::new();
        let mut in_go_import_block = false;

        for line in content.lines() {
            if imports.len() >= MAX_IMPORTS {
                break;
            }

            if is_import_line(ext, line, &mut in_go_import_block) {
                imports.push(line.to_owned());
            }
        }

        imports
    }
}

fn is_import_line(ext: &str, line: &str, in_go_import_block: &mut bool) -> bool {
    let trimmed = line.trim();
    match ext {
        "java" | "kt" | "scala" => {
            trimmed.starts_with("import ") || trimmed.starts_with("package ")
        }
        "cs" => trimmed.starts_with("using ") || trimmed.starts_with("namespace "),
        "py" => trimmed.starts_with("import ") || trimmed.starts_with("from "),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
            trimmed.starts_with("import ")
                || trimmed.starts_with("import{")
                || trimmed.contains("require(")
        }
        "rs" => trimmed.starts_with("use ") || trimmed.starts_with("extern crate "),
        "go" => {
            if trimmed == "import (" {
                *in_go_import_block = true;
                true
            } else if *in_go_import_block {
                if trimmed == ")" {
                    *in_go_import_block = false;
                }
                true
            } else {
                trimmed.starts_with("import \"")
            }
        }
        "rb" => trimmed.starts_with("require ") || trimmed.starts_with("require_relative "),
        _ => trimmed.starts_with("import "),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "deep_context_test.rs"]
mod tests;
