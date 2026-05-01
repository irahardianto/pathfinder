//! `get_repo_map` tool — AST-based repository skeleton with token budgeting.

use crate::server::helpers::{pathfinder_to_error_data, serialize_metadata};
use crate::server::types::{GetRepoMapParams, LspCapabilities, RepoCapabilities};
use crate::server::PathfinderServer;
use rmcp::model::{CallToolResult, ErrorData};
use std::path::Path;

impl PathfinderServer {
    /// Build an empty-changes response when `changed_since` finds no diffs.
    async fn empty_changes_response(&self) -> Result<CallToolResult, ErrorData> {
        let capability_status = self.lawyer.capability_status().await;
        let metadata = crate::server::types::GetRepoMapMetadata {
            tech_stack: vec![],
            files_scanned: 0,
            files_truncated: 0,
            files_in_scope: 0,
            coverage_percent: 100,
            version_hashes: std::collections::HashMap::new(),
            visibility_degraded: None,
            degraded: false,
            degraded_reason: None,
            capabilities: RepoCapabilities {
                edit: true,
                search: true,
                lsp: LspCapabilities {
                    supported: true,
                    per_language: capability_status,
                },
            },
        };
        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(
            "No files changed since the specified ref. No skeleton generated.",
        )]);
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
    pub(crate) async fn get_repo_map_impl(
        &self,
        params: GetRepoMapParams,
    ) -> Result<CallToolResult, ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(tool = "get_repo_map", path = %params.path, "get_repo_map: start");

        let target_path = Path::new(&params.path);

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
                        return self.empty_changes_response().await;
                    }
                    changed_files = Some(files);
                }
                Err(e) => {
                    tracing::warn!(error = %e, "get_repo_map: fallback to full map (git failed)");
                    degraded = true;
                    degraded_reason = Some(format!("Git error: {e}"));
                }
            }
        }

        let ts_start = std::time::Instant::now();
        let visibility_str = match params.visibility {
            pathfinder_common::types::Visibility::Public => "public",
            pathfinder_common::types::Visibility::All => "all",
        };
        // Clamp to reasonable bounds: minimum 500 (usable output), max 100k (memory safety)
        let max_tokens = params.max_tokens.clamp(500, 100_000);
        let config = pathfinder_treesitter::repo_map::SkeletonConfig::new(
            max_tokens,
            params.depth,
            visibility_str,
            params.max_tokens_per_file,
        )
        .with_changed_files(changed_files)
        .with_include_extensions(params.include_extensions)
        .with_exclude_extensions(params.exclude_extensions);

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

        let capability_status = self.lawyer.capability_status().await;

        let metadata = crate::server::types::GetRepoMapMetadata {
            tech_stack: result.tech_stack,
            files_scanned: result.files_scanned,
            files_truncated: result.files_truncated,
            files_in_scope: result.files_in_scope,
            coverage_percent: result.coverage_percent,
            version_hashes: result.version_hashes,
            visibility_degraded: None,
            degraded,
            degraded_reason,
            capabilities: RepoCapabilities {
                edit: true,
                search: true,
                lsp: LspCapabilities {
                    supported: true,
                    per_language: capability_status,
                },
            },
        };

        let mut res = CallToolResult::success(vec![rmcp::model::Content::text(result.skeleton)]);
        res.structured_content = serialize_metadata(&metadata);
        Ok(res)
    }
}
