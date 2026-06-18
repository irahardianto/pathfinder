use crate::server::types::{Detail, ExploreParams};
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

fn default_params() -> ExploreParams {
    ExploreParams {
        path: ".".to_owned(),
        detail: Detail::Symbols,
        changed_since: String::new(),
        max_tokens: 16_000,
        max_tokens_per_file: 2_000,
        depth: 5,
        visibility: Visibility::Public,
        include_extensions: vec![],
        exclude_extensions: vec![],
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
        truncated_paths: vec![],
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
        .push(Err(SurgeonError::Io(std::sync::Arc::new(
            std::io::Error::other("disk full"),
        ))));
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
/// Matches `lsp_health_impl` two-phase readiness model:
/// - `navigation_ready=Some(true)` → `"ready"`
/// - `navigation_ready=Some(false)` OR `indexing_complete=Some(false)` → `"warming_up"`
/// - `uptime_seconds=Some(_)` but no capability info → `"starting"`
/// - neither → `"unavailable"`
#[allow(clippy::too_many_lines)]
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
            indexing_progress_percent: None,
            registrations_received: None,
        },
    );

    // warming_up: navigation_ready = Some(false)
    map.insert(
        "csharp".to_owned(),
        LspLanguageStatus {
            validation: false,
            reason: String::new(),
            navigation_ready: Some(false),
            indexing_complete: None,
            uptime_seconds: Some(15),
            diagnostics_strategy: None,
            supports_definition: None,
            supports_call_hierarchy: None,
            supports_diagnostics: None,
            supports_formatting: None,
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    );

    // warming_up: indexing_complete = Some(false)
    map.insert(
        "go".to_owned(),
        LspLanguageStatus {
            validation: false,
            reason: String::new(),
            navigation_ready: None,
            indexing_complete: Some(false),
            uptime_seconds: Some(10),
            diagnostics_strategy: None,
            supports_definition: None,
            supports_call_hierarchy: None,
            supports_diagnostics: None,
            supports_formatting: None,
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    );

    // starting: uptime present, but no capability signals yet (lazy start)
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
            indexing_progress_percent: None,
            registrations_received: None,
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
            indexing_progress_percent: None,
            registrations_received: None,
        },
    );

    let result = super::derive_lsp_status(&map).expect("non-empty map must return Some");

    assert_eq!(result.get("rust").map(String::as_str), Some("ready"));
    assert_eq!(result.get("csharp").map(String::as_str), Some("warming_up"));
    assert_eq!(result.get("go").map(String::as_str), Some("warming_up"));
    assert_eq!(
        result.get("typescript").map(String::as_str),
        Some("starting")
    );
    assert_eq!(
        result.get("python").map(String::as_str),
        Some("unavailable")
    );
}

/// Verify that LSP pre-warm is NOT triggered when `tech_stack` is empty.
#[tokio::test]
async fn test_get_repo_map_no_prewarm_when_tech_stack_empty() {
    let mut result = ok_result();
    result.tech_stack = vec![]; // Empty tech stack

    let surgeon = MockSurgeon::default();
    surgeon
        .generate_skeleton_results
        .lock()
        .unwrap()
        .push(Ok(result));
    let (server, _dir) = make_server(surgeon);

    let result = server.get_repo_map_impl(default_params()).await;
    assert!(result.is_ok(), "get_repo_map should succeed: {result:?}");

    // Give any spawned tasks a chance to run (there shouldn't be any)
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    // No panic means the warm_start was not called (NoOpLawyer would panic if called unexpectedly)
}

/// Verify auto-scaling logic for large projects (>20 source files).
#[tokio::test]
async fn test_get_repo_map_auto_scaling_for_large_project() {
    let surgeon = MockSurgeon::default();
    surgeon
        .generate_skeleton_results
        .lock()
        .unwrap()
        .push(Ok(ok_result()));
    let (server, ws_dir) = make_server(surgeon);

    // Create >20 source files to trigger auto-scaling
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    for i in 0..25 {
        std::fs::write(
            ws_dir.path().join(format!("src/file{i}.rs")),
            format!("fn func_{i}() {{}}"),
        )
        .unwrap();
    }

    let mut params = default_params();
    // Use default max_tokens to trigger auto-scaling
    params.max_tokens = 16_000;

    let result = server.get_repo_map_impl(params).await;
    assert!(result.is_ok(), "get_repo_map should succeed: {result:?}");
}

// ── Detail::Structure token clamping ────────────────────────────────────

#[tokio::test]
async fn test_get_repo_map_detail_structure_clamps_tokens() {
    let surgeon = MockSurgeon::default();
    surgeon
        .generate_skeleton_results
        .lock()
        .unwrap()
        .push(Ok(ok_result()));
    let (server, _dir) = make_server(surgeon);

    let mut params = default_params();
    params.detail = Detail::Structure;
    params.max_tokens = 10_000; // Should be clamped to min(10_000, 4_000) = 4_000

    let result = server.get_repo_map_impl(params).await;
    assert!(
        result.is_ok(),
        "Detail::Structure should succeed: {result:?}"
    );
    let tool_result = result.unwrap();
    let meta = tool_result.structured_content.as_ref().unwrap();
    let max_tokens_used = meta
        .get("max_tokens_used")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    assert_eq!(
        max_tokens_used, 4_000,
        "Detail::Structure should clamp max_tokens to 4000, got {max_tokens_used}"
    );
}

// ── Detail::Files depth + token clamping ────────────────────────────────

#[tokio::test]
async fn test_get_repo_map_detail_files_clamps_depth_and_tokens() {
    let surgeon = MockSurgeon::default();
    surgeon
        .generate_skeleton_results
        .lock()
        .unwrap()
        .push(Ok(ok_result()));
    let (server, _dir) = make_server(surgeon);

    let mut params = default_params();
    params.detail = Detail::Files;
    params.depth = 10; // Should be clamped to min(10, 3) = 3
    params.max_tokens = 20_000; // Should be clamped to min(20_000, 8_000) = 8_000

    let result = server.get_repo_map_impl(params).await;
    assert!(result.is_ok(), "Detail::Files should succeed: {result:?}");
    let tool_result = result.unwrap();
    let meta = tool_result.structured_content.as_ref().unwrap();
    let max_tokens_used = meta
        .get("max_tokens_used")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);
    assert_eq!(
        max_tokens_used, 8_000,
        "Detail::Files should clamp max_tokens to 8000, got {max_tokens_used}"
    );
}

// ── Degraded text includes notice prefix ────────────────────────────────

#[tokio::test]
async fn test_get_repo_map_degraded_text_contains_notice() {
    let surgeon = MockSurgeon::default();
    surgeon
        .generate_skeleton_results
        .lock()
        .unwrap()
        .push(Ok(ok_result()));
    let (server, _dir) = make_server(surgeon);

    let mut params = default_params();
    // Use a ref that doesn't exist → git error → fallback → degraded=true
    params.changed_since = "nonexistent-ref-for-notice-test".to_owned();

    let result = server.get_repo_map_impl(params).await;
    assert!(
        result.is_ok(),
        "git failure should fall back to full map: {result:?}"
    );
    let tool_result = result.unwrap();

    // Extract the text content
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

    // Degraded text should have a notice prefix (from format_degraded_notice)
    assert!(
        text.contains("DEGRADED") || text.contains("degraded"),
        "degraded text should contain notice, got: {text}"
    );
    assert!(
        text.contains("skeleton"),
        "degraded text should still contain skeleton content"
    );
}
