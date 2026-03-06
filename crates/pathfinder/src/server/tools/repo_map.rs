//! `get_repo_map` tool — AST-based repository skeleton with token budgeting.

use crate::server::helpers::{io_error_data, pathfinder_to_error_data};
use crate::server::types::{GetRepoMapParams, GetRepoMapResponse, Visibility};
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

        let result = match self
            .surgeon
            .generate_skeleton(
                self.workspace_root.path(),
                target_path,
                params.max_tokens,
                params.depth,
                match params.visibility {
                    Visibility::Public => "public",
                    Visibility::All => "all",
                },
            )
            .await
        {
            Ok(r) => r,
            Err(e) => {
                let pfe = match e {
                    pathfinder_treesitter::error::SurgeonError::ParseError(reason) => {
                        pathfinder_common::error::PathfinderError::ParseError {
                            path: target_path.to_path_buf(),
                            reason,
                        }
                    }
                    pathfinder_treesitter::error::SurgeonError::UnsupportedLanguage(_) => {
                        pathfinder_common::error::PathfinderError::UnsupportedLanguage {
                            path: target_path.to_path_buf(),
                        }
                    }
                    pathfinder_treesitter::error::SurgeonError::SymbolNotFound { .. } => {
                        pathfinder_common::error::PathfinderError::SymbolNotFound {
                            semantic_path: params.path.clone(),
                            did_you_mean: vec![],
                        }
                    }
                    pathfinder_treesitter::error::SurgeonError::Io(err) => {
                        return Err(io_error_data(err.to_string()));
                    }
                };
                return Err(pathfinder_to_error_data(&pfe));
            }
        };

        tracing::info!(
            tool = "get_repo_map",
            path = %params.path,
            duration_ms = start.elapsed().as_millis(),
            files_scanned = result.files_scanned,
            files_truncated = result.files_truncated,
            engines_used = "treesitter",
            "get_repo_map: complete"
        );

        Ok(Json(GetRepoMapResponse {
            skeleton: result.skeleton,
            tech_stack: result.tech_stack,
            files_scanned: result.files_scanned,
            files_truncated: result.files_truncated,
            files_in_scope: result.files_in_scope,
            coverage_percent: result.coverage_percent,
            version_hashes: result.version_hashes,
            // Visibility filtering is not yet implemented; all symbols are returned.
            // Always signal degraded so agents know the param has no effect.
            visibility_degraded: Some(true),
        }))
    }
}
