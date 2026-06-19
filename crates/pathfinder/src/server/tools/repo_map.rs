//! `explore` tool — AST-based repository skeleton with token budgeting.

use crate::server::helpers::{
    format_degraded_notice, invalid_params_error, millis_to_u64, pathfinder_to_error_data,
    serialize_metadata,
};
use crate::server::types::{
    default_max_tokens, Detail, ExploreParams, LspCapabilities, RepoCapabilities,
};
use crate::server::PathfinderServer;
use pathfinder_common::types::DegradedReason;
use rmcp::model::{CallToolResult, ErrorData};
use std::path::Path;
use std::sync::Arc;

/// Count source files in the project to determine if auto-scaling is needed.
async fn count_source_files(root: &Path) -> usize {
    let mut count = 0;
    let extensions: [&str; 38] = [
        "rs", "ts", "tsx", "js", "jsx", "py", "go", "java", "kt", "swift", "cpp", "c", "h", "cs",
        "rb", "php", "scala", "clj", "ex", "exs", "erl", "hs", "ml", "m", "nim", "pl", "pm", "r",
        "sh", "lua", "dart", "fs", "fsi", "fsx", "zig", "v", "svelte", "vue",
    ];

    let mut dirs_to_visit = vec![root.to_path_buf()];

    while let Some(dir_path) = dirs_to_visit.pop() {
        let Ok(read_dir) = tokio::fs::read_dir(&dir_path).await else {
            continue;
        };

        let mut entries_stream = read_dir;
        loop {
            match entries_stream.next_entry().await {
                Ok(Some(entry)) => {
                    if let Ok(file_type) = entry.file_type().await {
                        let path = entry.path();

                        if file_type.is_file() {
                            let ext = path.extension().and_then(|e| e.to_str());
                            if ext.is_some_and(|e| extensions.contains(&e)) {
                                count += 1;
                            }
                        } else if file_type.is_dir() {
                            // Skip common non-source directories
                            let dir_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                            if !matches!(
                                dir_name,
                                "node_modules"
                                    | "target"
                                    | "vendor"
                                    | ".git"
                                    | "dist"
                                    | "build"
                                    | "out"
                                    | ".next"
                                    | ".venv"
                                    | "venv"
                                    | "env"
                            ) {
                                dirs_to_visit.push(path);
                            }
                        }
                    }
                }
                Ok(None) => break,
                Err(_) => {}
            }
        }
    }

    count
}

/// Convert the full `LspLanguageStatus` map into a flat `language → status` string map.
///
/// Uses the same two-phase readiness model as `lsp_health_impl`:
/// - `"ready"`: `navigation_ready = Some(true)`
/// - `"warming_up"`: LSP connected but not yet navigation-ready
/// - `"starting"`: uptime present but no capabilities reported
/// - `"unavailable"`: no connection info
///
/// Returns `None` when the map is empty (no LSP processes running).
///
/// Status logic matches `lsp_health_impl` two-phase readiness model:
/// - `navigation_ready == Some(true)` → `"ready"`
/// - `navigation_ready == Some(false) || indexing_complete == Some(false)` → `"warming_up"`
/// - `uptime_seconds.is_some()` (process running, no capability data yet) → `"starting"`
/// - else → `"unavailable"`
fn derive_lsp_status(
    capability_status: &std::collections::HashMap<String, pathfinder_lsp::types::LspLanguageStatus>,
) -> Option<std::collections::HashMap<String, String>> {
    if capability_status.is_empty() {
        return None;
    }
    let map = capability_status
        .iter()
        .map(|(lang, status)| {
            let s = if status.navigation_ready == Some(true) {
                "ready"
            } else if status.navigation_ready == Some(false)
                || status.indexing_complete == Some(false)
            {
                "warming_up"
            } else if status.uptime_seconds.is_some() {
                "starting"
            } else {
                "unavailable"
            };
            (lang.clone(), s.to_owned())
        })
        .collect();
    Some(map)
}

impl PathfinderServer {
    /// Build an empty-changes response when `changed_since` finds no diffs.
    ///
    /// Passes `changed_since_ref` so the text output can name the exact ref and
    /// tell the agent what to do if they want the full skeleton.
    async fn empty_changes_response(
        &self,
        changed_since_ref: &str,
    ) -> Result<CallToolResult, ErrorData> {
        let capability_status = self.lawyer.capability_status().await;
        let lsp_status = derive_lsp_status(&capability_status);
        let metadata = crate::server::types::GetRepoMapMetadata {
            tech_stack: vec![],
            files_scanned: 0,
            files_truncated: 0,
            truncated_paths: vec![],
            files_in_scope: 0,
            coverage_percent: 100,
            version_hashes: std::collections::HashMap::new(),
            visibility_degraded: None,
            degraded: false,
            degraded_reason: None,
            actionable_guidance: None,
            capabilities: RepoCapabilities {
                search: true,
                lsp: LspCapabilities {
                    supported: true,
                    per_language: capability_status,
                },
            },
            max_tokens_used: 0,
            lsp_status,
            duration_ms: None,
            hint: None,
        };
        // Produce an actionable message so the agent knows WHY the result is empty
        // and what to do next, rather than seeing a silent empty response.
        let message = format!(
            "No files changed since '{changed_since_ref}'. \
             The repository is unchanged relative to that ref.\n\
             To see the full repository skeleton, call get_repo_map without the \
             changed_since parameter."
        );
        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(message)]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
    }
    /// Core logic for the `get_repo_map` tool.
    ///
    /// Generates a structural skeleton of the project via Tree-sitter.
    /// Visibility filtering is not yet implemented; `visibility_degraded`
    /// is always set to `Some(true)` so agents know the param has no effect.
    // Orchestrates git (changed_since filter) and Tree-sitter (skeleton generation),
    // with degraded-mode fallback when git fails, plus LSP capability collection for
    // the response metadata. The linear structure makes the orchestration explicit.
    #[expect(
        clippy::too_many_lines,
        reason = "Linear orchestration pipeline: sandbox → git filter → auto-scale → tree-sitter → LSP pre-warm → metadata assembly. Extraction would obscure the sequential flow."
    )]
    pub(crate) async fn get_repo_map_impl(
        &self,
        params: ExploreParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(tool = "get_repo_map", path = %params.path, "get_repo_map: start");

        let target_path = Path::new(&params.path);

        // Validation check for mutually exclusive parameters: include_extensions and exclude_extensions
        if !params.include_extensions.is_empty() && !params.exclude_extensions.is_empty() {
            return Err(invalid_params_error(
                "`include_extensions` and `exclude_extensions` are mutually exclusive; you cannot provide both",
            ));
        }

        // Sandbox check
        if let Err(e) = self.sandbox.check(target_path) {
            tracing::warn!(tool = "get_repo_map", path = %params.path, error = %e, "get_repo_map: access denied");
            return Err(pathfinder_to_error_data(&e));
        }

        let mut degraded = false;
        let mut degraded_reason = None;
        let mut changed_files = None;
        if !params.changed_since.is_empty() {
            match pathfinder_common::git::get_changed_files_since(
                &pathfinder_common::git::SystemGit,
                self.workspace_root.path(),
                &params.changed_since,
            )
            .await
            {
                Ok(files) => {
                    if files.is_empty() {
                        return self.empty_changes_response(&params.changed_since).await;
                    }
                    changed_files = Some(files);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "get_repo_map: fallback to full map (git failed)");
                    degraded = true;
                    degraded_reason = Some(DegradedReason::GitError);
                }
            }
        }

        // Apply detail overrides to depth and max_tokens
        let (depth, max_tokens) = match params.detail {
            Detail::Structure => (params.depth, params.max_tokens.min(4_000)),
            Detail::Files => (params.depth.min(3), params.max_tokens.min(8_000)),
            Detail::Symbols => (params.depth, params.max_tokens),
        };
        let include_tests = !matches!(params.detail, Detail::Structure | Detail::Files);

        let ts_start = std::time::Instant::now();
        let visibility_str = match params.visibility {
            pathfinder_common::types::Visibility::Public => "public",
            pathfinder_common::types::Visibility::All => "all",
        };

        // Auto-scale token budget for large projects
        let effective_max_tokens = if max_tokens == default_max_tokens() {
            // Only auto-scale when the user didn't explicitly set a value
            let source_file_count = count_source_files(self.workspace_root.path()).await;
            if source_file_count > 20 {
                let scaled = u32::try_from(source_file_count)
                    .unwrap_or(u32::MAX)
                    .saturating_mul(800)
                    .clamp(16_000, 48_000);
                tracing::info!(
                    tool = "get_repo_map",
                    source_file_count,
                    auto_scaled_tokens = scaled,
                    requested_tokens = max_tokens,
                    "auto-scaling max_tokens for large project"
                );
                scaled
            } else {
                max_tokens
            }
        } else {
            // Respect explicit user setting
            max_tokens
        };

        // Clamp to reasonable bounds: minimum 500 (usable output), max 100k (memory safety)
        let max_tokens = effective_max_tokens.clamp(500, 100_000);
        let skeleton_detail = match params.detail {
            Detail::Structure => pathfinder_treesitter::repo_map::SkeletonDetail::Structure,
            Detail::Files => pathfinder_treesitter::repo_map::SkeletonDetail::Files,
            Detail::Symbols => pathfinder_treesitter::repo_map::SkeletonDetail::Symbols,
        };
        let config = pathfinder_treesitter::repo_map::SkeletonConfig::new(
            max_tokens,
            depth,
            visibility_str,
            params.max_tokens_per_file,
        )
        .with_detail(skeleton_detail)
        .with_changed_files(changed_files)
        .with_include_extensions(params.include_extensions)
        .with_exclude_extensions(params.exclude_extensions)
        .with_include_tests(include_tests);

        let result = match self
            .surgeon
            .generate_skeleton(self.workspace_root.path(), target_path, &config)
            .await
        {
            Ok(r) => r,
            Err(e) => {
                return Err(crate::server::helpers::treesitter_error_to_error_data(e));
            }
        };
        let tree_sitter_ms = ts_start.elapsed().as_millis();

        tracing::info!(
            tool = "get_repo_map",
            path = %params.path,
            tree_sitter_ms,
            duration_ms = start.elapsed().as_millis(),
            files_scanned = result.files_scanned,
            files_truncated = result.files_truncated,
            engines_used = "treesitter",
            "get_repo_map: complete"
        );

        // LT-4: Pre-warm LSP processes for languages found in the project skeleton.
        // PATCH-004: Use warm_start_for_languages_and_track which sets warm_start_complete flag.
        if !result.tech_stack.is_empty() {
            let lawyer = Arc::clone(&self.lawyer);
            let languages = result.tech_stack.clone();

            tokio::spawn(async move {
                lawyer.warm_start_for_languages_and_track(&languages);
                tracing::info!(
                    "PATCH-004: get_repo_map triggered warm_start_and_track for {} languages",
                    languages.len()
                );
            });
        }

        let capability_status = self.lawyer.capability_status().await;
        let lsp_status = derive_lsp_status(&capability_status);
        let duration_ms = start.elapsed().as_millis();

        let hint = if result.coverage_percent < 100 {
            Some(format!(
                "Repository map is incomplete (coverage: {}%). To scan more files, increase max_tokens (currently {}).",
                result.coverage_percent, max_tokens
            ))
        } else {
            None
        };

        let metadata = crate::server::types::GetRepoMapMetadata {
            tech_stack: result.tech_stack,
            files_scanned: result.files_scanned,
            files_truncated: result.files_truncated,
            truncated_paths: result.truncated_paths,
            files_in_scope: result.files_in_scope,
            coverage_percent: result.coverage_percent,
            version_hashes: result.version_hashes,
            visibility_degraded: None,
            degraded,
            degraded_reason,
            actionable_guidance: degraded_reason.as_ref().map(DegradedReason::guidance),
            capabilities: RepoCapabilities {
                search: true,
                lsp: LspCapabilities {
                    supported: true,
                    per_language: capability_status,
                },
            },
            max_tokens_used: max_tokens,
            lsp_status,
            duration_ms: Some(millis_to_u64(duration_ms)),
            hint: hint.clone(),
        };

        let mut text = if degraded {
            let notice = degraded_reason
                .as_ref()
                .map_or_else(|| "DEGRADED (unknown)".to_owned(), format_degraded_notice);
            format!(
                "{notice}\n{}\n[completed in {duration_ms}ms]",
                result.skeleton
            )
        } else {
            format!("{}\n[completed in {duration_ms}ms]", result.skeleton)
        };

        if let Some(ref h) = hint {
            text = format!("{text}\n\nHint: {h}");
        }

        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(text)]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
    }
}

#[cfg(test)]
#[path = "repo_map_test.rs"]
mod tests;
