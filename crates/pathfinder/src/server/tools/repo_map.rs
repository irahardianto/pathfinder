//! `get_repo_map` tool — AST-based repository skeleton with token budgeting.

use crate::server::helpers::{pathfinder_to_error_data, serialize_metadata};
use crate::server::types::{
    default_max_tokens, GetRepoMapParams, LspCapabilities, RepoCapabilities,
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
            } else if status.uptime_seconds.is_some() {
                "warming_up"
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
    async fn empty_changes_response(&self) -> Result<CallToolResult, ErrorData> {
        let capability_status = self.lawyer.capability_status().await;
        let lsp_status = derive_lsp_status(&capability_status);
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
                search: true,
                lsp: LspCapabilities {
                    supported: true,
                    per_language: capability_status,
                },
            },
            max_tokens_used: 0,
            lsp_status,
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
    #[expect(
        clippy::too_many_lines,
        reason = "Linear orchestration pipeline: sandbox → git filter → auto-scale → tree-sitter → LSP pre-warm → metadata assembly. Extraction would obscure the sequential flow."
    )]
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
                    degraded_reason = Some(DegradedReason::GitError);
                }
            }
        }

        let ts_start = std::time::Instant::now();
        let visibility_str = match params.visibility {
            pathfinder_common::types::Visibility::Public => "public",
            pathfinder_common::types::Visibility::All => "all",
        };

        // Auto-scale token budget for large projects
        let effective_max_tokens = if params.max_tokens == default_max_tokens() {
            // Only auto-scale when the user didn't explicitly set a value
            let source_file_count = count_source_files(self.workspace_root.path()).await;
            if source_file_count > 20 {
                let scaled = (u32::try_from(source_file_count).unwrap_or(u32::MAX) * 800)
                    .clamp(16_000, 48_000);
                tracing::info!(
                    tool = "get_repo_map",
                    source_file_count,
                    auto_scaled_tokens = scaled,
                    requested_tokens = params.max_tokens,
                    "auto-scaling max_tokens for large project"
                );
                scaled
            } else {
                params.max_tokens
            }
        } else {
            // Respect explicit user setting
            params.max_tokens
        };

        // Clamp to reasonable bounds: minimum 500 (usable output), max 100k (memory safety)
        let max_tokens = effective_max_tokens.clamp(500, 100_000);
        let config = pathfinder_treesitter::repo_map::SkeletonConfig::new(
            max_tokens,
            params.depth,
            visibility_str,
            params.max_tokens_per_file,
        )
        .with_changed_files(changed_files)
        .with_include_extensions(params.include_extensions)
        .with_exclude_extensions(params.exclude_extensions)
        .with_include_tests(params.include_tests);

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
                search: true,
                lsp: LspCapabilities {
                    supported: true,
                    per_language: capability_status,
                },
            },
            max_tokens_used: max_tokens,
            lsp_status,
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
            include_tests: true,
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

    // ── 1.3 lsp_status flat map ──────────────────────────────────────────────

    /// Verify that `derive_lsp_status` returns `None` when the capability map is empty.
    /// (no LSP processes running → field absent from JSON)
    #[test]
    fn test_derive_lsp_status_empty_map_returns_none() {
        let empty: std::collections::HashMap<String, pathfinder_lsp::types::LspLanguageStatus> =
            std::collections::HashMap::new();
        assert!(
            super::derive_lsp_status(&empty).is_none(),
            "empty capability map must produce None lsp_status"
        );
    }

    /// Verify `derive_lsp_status` produces the correct status strings.
    /// - `navigation_ready=Some(true)` → `"ready"`
    /// - `uptime_seconds=Some(_)` but no `navigation_ready` → `"warming_up"`
    /// - neither → `"unavailable"`
    #[test]
    fn test_derive_lsp_status_correct_status_strings() {
        use pathfinder_lsp::types::LspLanguageStatus;

        let mut map = std::collections::HashMap::new();

        // ready: navigation_ready = Some(true)
        map.insert(
            "rust".to_owned(),
            LspLanguageStatus {
                validation: false,
                reason: String::new(),
                navigation_ready: Some(true),
                indexing_complete: None,
                uptime_seconds: Some(30),
                diagnostics_strategy: None,
                supports_definition: None,
                supports_call_hierarchy: None,
                supports_diagnostics: None,
                supports_formatting: None,
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
            },
        );

        // warming_up: uptime present, but navigation_ready not true
        map.insert(
            "typescript".to_owned(),
            LspLanguageStatus {
                validation: false,
                reason: String::new(),
                navigation_ready: None,
                indexing_complete: None,
                uptime_seconds: Some(5),
                diagnostics_strategy: None,
                supports_definition: None,
                supports_call_hierarchy: None,
                supports_diagnostics: None,
                supports_formatting: None,
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
            },
        );

        // unavailable: no uptime, no navigation_ready
        map.insert(
            "python".to_owned(),
            LspLanguageStatus {
                validation: false,
                reason: String::new(),
                navigation_ready: None,
                indexing_complete: None,
                uptime_seconds: None,
                diagnostics_strategy: None,
                supports_definition: None,
                supports_call_hierarchy: None,
                supports_diagnostics: None,
                supports_formatting: None,
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
            },
        );

        let result = super::derive_lsp_status(&map).expect("non-empty map must return Some");

        assert_eq!(result.get("rust").map(String::as_str), Some("ready"));
        assert_eq!(
            result.get("typescript").map(String::as_str),
            Some("warming_up")
        );
        assert_eq!(
            result.get("python").map(String::as_str),
            Some("unavailable")
        );
    }
}
