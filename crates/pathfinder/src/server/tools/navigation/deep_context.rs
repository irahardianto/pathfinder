//! `inspect` tool handler (deep context mode).
//!
//! Returns the symbol's source code enriched with LSP call-hierarchy
//! dependencies. Degrades gracefully to symbol scope only when no LSP
//! is configured.

use crate::server::helpers::{
    format_degraded_notice, millis_to_u64, parse_semantic_path, pathfinder_to_error_data,
    require_symbol_target, serialize_metadata,
};
use crate::server::types::InspectParams;
use crate::server::PathfinderServer;
use pathfinder_common::types::DegradedReason;
use pathfinder_lsp::LspError;
use rmcp::model::{CallToolResult, ErrorData};

use super::{
    candidate_definition_pattern, extract_call_candidates, is_source_file, is_workspace_file,
    language_to_file_glob, LspResolution,
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
                         probe returned no result — LSP likely warming up, waiting 1s and retrying"
                    );

                    // 1s is sufficient — grep fallback handles the case where LSP is still not ready.
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
        let pattern = candidate_definition_pattern(language, candidate);

        self.scout
            .search(&pathfinder_search::SearchParams {
                workspace_root: self.workspace_root.path().to_path_buf(),
                query: pattern,
                is_regex: true,
                max_results: max_results_per_candidate,
                path_glob: language_to_file_glob(language).to_string(),
                exclude_glob: Vec::new(),
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

                // Enrich the grep-resolved candidate name to a qualified treesitter path.
                // Falls back to `file::candidate` when Surgeon returns None or errors.
                let dep_path = self.enrich_semantic_path(&file, line, &candidate).await;
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
                    // Enrich the flat LSP name to a qualified treesitter path.
                    // Falls back to `file::flat_name` when Surgeon returns None or errors.
                    let sp = self
                        .enrich_semantic_path(&callee.file, callee.line, &callee.name)
                        .await;
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
        params: InspectParams,
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

        let project_only = true;
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
        let resolution_strategy = if !degraded && engines.contains(&"lsp") {
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

        // Extract file-level import statements when requested.
        // This is a cheap operation (line-scan, no tree-sitter) but opt-in to avoid
        // unnecessary output for languages without verbose import blocks.
        let imports = if params.include_imports {
            let abs_file = self.workspace_root.path().join(&semantic_path.file_path);
            self.extract_file_imports(&abs_file).await
        } else {
            Vec::new()
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
            imports,
            content: Some(scope.content.clone()),
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

        // Build the imports block for the text channel when present.
        let import_block = if metadata.imports.is_empty() {
            String::new()
        } else {
            format!("\nImports:\n{}\n", metadata.imports.join("\n"))
        };

        // Prepend degradation notice when in degraded mode
        let text = if degraded {
            let notice = degraded_reason
                .as_ref()
                .map_or_else(|| "DEGRADED (unknown)".to_owned(), format_degraded_notice);
            format!(
                "{notice}\n\n{dep_count} dependencies loaded{dep_block}{import_block}\n\n{}\n[completed in {duration_ms}ms]",
                scope.content
            )
        } else {
            format!(
                "{dep_count} dependencies loaded{dep_block}{import_block}\n\n{}\n[completed in {duration_ms}ms]",
                scope.content
            )
        };
        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
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

            let trimmed = line.trim();

            match ext {
                "java" | "kt" | "scala" => {
                    if trimmed.starts_with("import ") || trimmed.starts_with("package ") {
                        imports.push(line.to_owned());
                    }
                }
                "cs" => {
                    if trimmed.starts_with("using ") || trimmed.starts_with("namespace ") {
                        imports.push(line.to_owned());
                    }
                }
                "py" => {
                    if trimmed.starts_with("import ") || trimmed.starts_with("from ") {
                        imports.push(line.to_owned());
                    }
                }
                "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => {
                    // Match import statements and require() calls
                    if trimmed.starts_with("import ")
                        || trimmed.starts_with("import{")
                        || trimmed.contains("require(")
                    {
                        imports.push(line.to_owned());
                    }
                }
                "rs" => {
                    // `use foo::bar;` — leading `use ` distinguishes from `use` keyword
                    // in other contexts (e.g., `use std::io::{Read, Write};`).
                    // Also capture `extern crate` for older Rust code.
                    if trimmed.starts_with("use ") || trimmed.starts_with("extern crate ") {
                        imports.push(line.to_owned());
                    }
                }
                "go" => {
                    // Go has single-line `import "pkg"` and multi-line `import (\n...)` blocks.
                    if trimmed == "import (" {
                        in_go_import_block = true;
                        imports.push(line.to_owned());
                    } else if in_go_import_block {
                        imports.push(line.to_owned());
                        if trimmed == ")" {
                            in_go_import_block = false;
                        }
                    } else if trimmed.starts_with("import \"") {
                        imports.push(line.to_owned());
                    }
                }
                "swift" => {
                    if trimmed.starts_with("import ") {
                        imports.push(line.to_owned());
                    }
                }
                "rb" => {
                    if trimmed.starts_with("require ") || trimmed.starts_with("require_relative ") {
                        imports.push(line.to_owned());
                    }
                }
                _ => {
                    // Unknown extension — try the most common pattern
                    if trimmed.starts_with("import ") {
                        imports.push(line.to_owned());
                    }
                }
            }
        }

        imports
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used)]
#[path = "deep_context_test.rs"]
mod tests;
