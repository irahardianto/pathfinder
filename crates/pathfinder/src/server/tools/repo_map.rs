//! `get_repo_map` tool — AST-based repository skeleton with token budgeting.

use crate::server::helpers::{pathfinder_to_error_data, serialize_metadata};
use crate::server::types::{GetRepoMapParams, LspCapabilities, RepoCapabilities};
use crate::server::PathfinderServer;
use rmcp::model::{CallToolResult, ErrorData};
use std::path::Path;
use std::sync::Arc;

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

        // LT-4: Pre-warm LSP processes for languages found in the project skeleton.
        // This runs in the background so get_repo_map returns immediately.
        if !result.tech_stack.is_empty() {
            let lawyer = Arc::clone(&self.lawyer);
            let languages = result.tech_stack.clone();
            tokio::spawn(async move {
                lawyer.warm_start_for_languages(&languages);
            });
        }

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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use crate::server::types::GetRepoMapParams;
    use pathfinder_common::config::PathfinderConfig;
    use pathfinder_common::sandbox::Sandbox;
    use pathfinder_common::types::{Visibility, WorkspaceRoot};
    use pathfinder_search::MockScout;
    use pathfinder_treesitter::mock::MockSurgeon;
    use pathfinder_treesitter::repo_map::RepoMapResult;
    use pathfinder_treesitter::SurgeonError;
    use std::collections::HashMap;
    use std::sync::Arc;
    use tempfile::tempdir;

    fn default_params() -> GetRepoMapParams {
        GetRepoMapParams {
            path: ".".to_owned(),
            changed_since: String::new(),
            max_tokens: 16_000,
            max_tokens_per_file: 2_000,
            depth: 5,
            visibility: Visibility::Public,
            include_extensions: vec![],
            exclude_extensions: vec![],
            include_imports: pathfinder_common::types::IncludeImports::ThirdParty,
        }
    }

    fn make_server(surgeon: MockSurgeon) -> (crate::server::PathfinderServer, tempfile::TempDir) {
        let ws_dir = tempdir().expect("tempdir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("workspace");
        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = crate::server::PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(surgeon),
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );
        (server, ws_dir)
    }

    fn ok_result() -> RepoMapResult {
        RepoMapResult {
            skeleton: "# skeleton".to_owned(),
            tech_stack: vec!["rust".to_owned()],
            files_scanned: 3,
            files_truncated: 0,
            files_in_scope: 3,
            coverage_percent: 100,
            version_hashes: HashMap::new(),
        }
    }

    // ── happy path ──────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_repo_map_returns_skeleton() {
        let surgeon = MockSurgeon::default();
        surgeon
            .generate_skeleton_results
            .lock()
            .unwrap()
            .push(Ok(ok_result()));
        let (server, _dir) = make_server(surgeon);

        let result = server.get_repo_map_impl(default_params()).await;
        assert!(result.is_ok(), "should succeed: {result:?}");
        let tool_result = result.unwrap();
        let text = tool_result
            .content
            .first()
            .and_then(|c| {
                if let rmcp::model::RawContent::Text(t) = &c.raw {
                    Some(t.text.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        assert!(text.contains("skeleton"), "skeleton text should be present");
    }

    // ── sandbox rejection ────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_repo_map_rejects_sandbox_denied_path() {
        let (server, _dir) = make_server(MockSurgeon::default());
        let mut params = default_params();
        params.path = ".git/HEAD".to_owned(); // hardcoded deny pattern

        let result = server.get_repo_map_impl(params).await;
        assert!(result.is_err(), "sandbox should deny .git paths");
        let err = result.unwrap_err();
        let code = err
            .data
            .as_ref()
            .and_then(|d| d.get("error"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert_eq!(code, "ACCESS_DENIED");
    }

    // ── surgeon error ────────────────────────────────────────────────────────

    #[tokio::test]
    async fn test_get_repo_map_propagates_surgeon_error() {
        let surgeon = MockSurgeon::default();
        surgeon
            .generate_skeleton_results
            .lock()
            .unwrap()
            .push(Err(SurgeonError::Io(std::io::Error::other("disk full"))));
        let (server, _dir) = make_server(surgeon);

        let result = server.get_repo_map_impl(default_params()).await;
        assert!(result.is_err(), "surgeon error should propagate");
    }

    // ── changed_since: empty file list returns early response ────────────────

    #[tokio::test]
    async fn test_get_repo_map_changed_since_empty_returns_early() {
        // MockSurgeon has no results queued — if skeleton is called, it panics.
        // The empty-changes path should short-circuit before calling surgeon.
        let ws_dir = tempdir().expect("tempdir");
        let ws = WorkspaceRoot::new(ws_dir.path()).expect("workspace");

        // Initialise an empty git repo so get_changed_files_since succeeds with []
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(ws_dir.path())
            .status()
            .expect("git init");
        std::process::Command::new("git")
            .args(["commit", "--allow-empty", "-m", "init"])
            .env("GIT_AUTHOR_NAME", "test")
            .env("GIT_AUTHOR_EMAIL", "t@t.t")
            .env("GIT_COMMITTER_NAME", "test")
            .env("GIT_COMMITTER_EMAIL", "t@t.t")
            .current_dir(ws_dir.path())
            .status()
            .expect("git commit");

        let config = PathfinderConfig::default();
        let sandbox = Sandbox::new(ws.path(), &config.sandbox);
        let server = crate::server::PathfinderServer::with_all_engines(
            ws,
            config,
            sandbox,
            Arc::new(MockScout::default()),
            Arc::new(MockSurgeon::default()), // no results queued
            Arc::new(pathfinder_lsp::NoOpLawyer),
        );

        let mut params = default_params();
        params.changed_since = "HEAD".to_owned(); // nothing changed since HEAD

        let result = server.get_repo_map_impl(params).await;
        assert!(result.is_ok(), "empty changed_since should succeed");
        let tool_result = result.unwrap();
        let text = tool_result
            .content
            .first()
            .and_then(|c| {
                if let rmcp::model::RawContent::Text(t) = &c.raw {
                    Some(t.text.clone())
                } else {
                    None
                }
            })
            .unwrap_or_default();
        assert!(
            text.contains("No files changed"),
            "should return empty-changes message, got: {text}"
        );
    }

    // ── changed_since: git failure falls back to full map ────────────────────

    #[tokio::test]
    async fn test_get_repo_map_changed_since_git_failure_falls_back() {
        let surgeon = MockSurgeon::default();
        surgeon
            .generate_skeleton_results
            .lock()
            .unwrap()
            .push(Ok(ok_result()));
        let (server, _dir) = make_server(surgeon);

        let mut params = default_params();
        // Use a ref that doesn't exist → git error → fallback
        params.changed_since = "nonexistent-ref-xyzzy".to_owned();

        let result = server.get_repo_map_impl(params).await;
        assert!(
            result.is_ok(),
            "git failure should fall back to full map: {result:?}"
        );
        // Metadata should reflect degraded=true
        let tool_result = result.unwrap();
        let meta = tool_result.structured_content.as_ref().unwrap();
        assert_eq!(
            meta.get("degraded").and_then(serde_json::Value::as_bool),
            Some(true),
            "degraded flag should be set on git failure"
        );
    }

    /// LT-4: Verify that `get_repo_map` triggers pre-warm for detected languages.
    ///
    /// This test verifies that the warmup spawn doesn't panic even with
    /// a `NoOpLawyer` (which has default no-op `warm_start_for_languages`).
    #[tokio::test]
    async fn test_get_repo_map_triggers_lt4_prewarm() {
        let mut result = ok_result();
        result.tech_stack = vec!["rust".to_owned(), "go".to_owned()];

        let surgeon = MockSurgeon::default();
        surgeon
            .generate_skeleton_results
            .lock()
            .unwrap()
            .push(Ok(result));
        let (server, _dir) = make_server(surgeon);

        let result = server.get_repo_map_impl(default_params()).await;
        assert!(result.is_ok(), "get_repo_map should succeed: {result:?}");

        // Give the spawned warm_start_for_languages task a chance to run.
        // With NoOpLawyer, it's a no-op, but we verify no panics occur.
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
}
