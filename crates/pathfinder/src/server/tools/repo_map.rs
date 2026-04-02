//! `get_repo_map` tool — AST-based repository skeleton with token budgeting.

use crate::server::helpers::pathfinder_to_error_data;
use crate::server::types::{
    GetRepoMapParams, GetRepoMapResponse, LspCapabilities, RepoCapabilities,
};
use crate::server::PathfinderServer;
use rmcp::handler::server::wrapper::Json;
use rmcp::model::ErrorData;
use std::path::Path;

impl PathfinderServer {
    /// Core logic for the `get_repo_map` tool.
    ///
    /// Generates a structural skeleton of the project via Tree-sitter.
    /// Visibility filtering is not yet implemented; `visibility_degraded`
    /// is always set to `Some(true)` so agents know the param has no effect.
    pub(crate) async fn get_repo_map_impl(
        &self,
        params: GetRepoMapParams,
    ) -> Result<Json<GetRepoMapResponse>, ErrorData> {
        let start = std::time::Instant::now();
        tracing::info!(tool = "get_repo_map", path = %params.path, "get_repo_map: start");

        let target_path = Path::new(&params.path);

        // Sandbox check
        if let Err(e) = self.sandbox.check(target_path) {
            tracing::warn!(tool = "get_repo_map", path = %params.path, error = %e, "get_repo_map: access denied");
            return Err(pathfinder_to_error_data(&e));
        }

        let ts_start = std::time::Instant::now();
        let result = match self
            .surgeon
            .generate_skeleton(
                self.workspace_root.path(),
                target_path,
                params.max_tokens,
                params.depth,
                match params.visibility {
                    pathfinder_common::types::Visibility::Public => "public",
                    pathfinder_common::types::Visibility::All => "all",
                },
                params.max_tokens_per_file,
            )
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

        Ok(Json(GetRepoMapResponse {
            skeleton: result.skeleton,
            tech_stack: result.tech_stack,
            files_scanned: result.files_scanned,
            files_truncated: result.files_truncated,
            files_in_scope: result.files_in_scope,
            coverage_percent: result.coverage_percent,
            version_hashes: result.version_hashes,
            // Visibility filtering is implemented via name-convention heuristics
            // (_-prefix = private, lowercase-first = Go package-private). Not degraded.
            visibility_degraded: None,
            capabilities: RepoCapabilities {
                edit: true,
                search: true,
                lsp: LspCapabilities {
                    supported: true,
                    per_language: capability_status,
                },
            },
        }))
    }
}
