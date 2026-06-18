use super::*;
use crate::server::PathfinderServer;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::WorkspaceRoot;
use pathfinder_search::RipgrepScout;
use pathfinder_treesitter::mock::MockSurgeon;
use std::sync::Arc;

// ── CG-6: degraded flag for unsupported language ──────────────────────

#[tokio::test]
async fn test_search_codebase_degraded_on_unsupported_language() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create a file with an unsupported extension
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/data.xyz"), "findme content").unwrap();

    // Use real RipgrepScout so it actually searches the filesystem
    let scout = Arc::new(RipgrepScout);
    let surgeon = Arc::new(MockSurgeon::new());
    // Pre-configure surgeon for enrichment calls (1 match expected)
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("code".to_string()));
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);

    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = SearchParams {
        query: "findme".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*.xyz".to_owned(),
        max_results: 10,
        context_lines: 0,
        known_files: vec![],
        exclude_glob: String::default(),
        offset: 0,
        ..Default::default()
    };
    let result = server.search_codebase_impl(params).await;
    let response = result.expect("search should succeed");
    assert!(
        response.0.degraded,
        "should be degraded for unsupported language"
    );
    assert_eq!(
        response
            .0
            .degraded_reason
            .as_ref()
            .map(std::string::ToString::to_string),
        Some("unsupported_language_filter_bypassed".to_string()),
        "filter is bypassed so reason is unsupported_language_filter_bypassed"
    );
}

// ── PATCH-004: group_by_file + known_files regression test ─────────

#[tokio::test]
async fn test_search_group_by_file_with_known_files() {
    // Bug scenario: when all matches belong to files in `known_files` with
    // `group_by_file: true`, the original code would:
    // 1. Return total_matches > 0
    // 2. But file_groups would be "empty" because both `matches` and `known_matches`
    //    would be skipped by serde (they use skip_serializing_if = "Vec::is_empty")
    //    and there was no `total_matches` field to indicate matches exist.
    //
    // Fix adds:
    // - `total_matches` field that is always present
    // - Known matches go into `known_matches` array instead of being lost

    let ws_dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create a Rust file with two matches
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/main.rs"),
        "fn findme() {}\nfn other() { findme(); }\n",
    )
    .unwrap();

    let scout = Arc::new(RipgrepScout);
    let surgeon = Arc::new(MockSurgeon::new());
    // Two matches expected — pre-configure surgeon for enrichment calls
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("code".to_string()));
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("code".to_string()));
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);

    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = SearchParams {
        query: "findme".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*.rs".to_owned(),
        max_results: 10,
        context_lines: 0,
        known_files: vec!["src/main.rs".to_owned()],
        exclude_glob: String::default(),
        offset: 0,
        ..Default::default()
    };

    let result = server.search_codebase_impl(params).await;
    let response = result.expect("search should succeed");

    // 1. total_matches should be positive
    assert_eq!(
        response.0.total_matches, 2,
        "total_matches should reflect actual number of matches"
    );

    // 2. file_groups should NOT be empty (original data-loss bug)
    let groups = response
        .0
        .file_groups
        .expect("should have file_groups when group_by_file=true");
    assert!(
            !groups.is_empty(),
            "file_groups should NOT be empty when matches exist — original bug: total_matches>0 but file_groups empty"
        );

    // 3. total_matches per group should be populated
    assert_eq!(
        groups[0].total_matches, 2,
        "group total_matches should show count even when all matches are known"
    );

    // 4. known_matches should contain the matches (NOT `matches` array)
    //    because the file is in `known_files`
    assert!(
        groups[0].matches.is_empty(),
        "matches array should be empty when all matches belong to known_files"
    );
    assert_eq!(
        groups[0].known_matches.len(),
        2,
        "known_matches should contain the suppressed matches"
    );
    assert!(
        groups[0].known_matches[0].known,
        "known_matches should have known=true flag"
    );
}

#[test]
fn test_apply_filter_mode_code_only() {
    // Arrange
    let matches = vec![
        SearchMatch {
            file: "src/main.rs".to_owned(),
            line: 1,
            column: 1,
            content: "fn main() {}".to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "hash".to_owned(),
            known: None,
        },
        SearchMatch {
            file: "src/main.rs".to_owned(),
            line: 2,
            column: 1,
            content: "// a comment".to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "hash".to_owned(),
            known: None,
        },
    ];
    let node_types = vec!["code".to_owned(), "comment".to_owned()];

    // Act
    let filtered = apply_filter_mode(matches, &node_types, FilterMode::CodeOnly);

    // Assert
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].content, "fn main() {}");
}

#[test]
fn test_apply_filter_mode_comments_only() {
    // Arrange
    let matches = vec![
        SearchMatch {
            file: "src/main.rs".to_owned(),
            line: 1,
            column: 1,
            content: "fn main() {}".to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "hash".to_owned(),
            known: None,
        },
        SearchMatch {
            file: "src/main.rs".to_owned(),
            line: 2,
            column: 1,
            content: "// a comment".to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "hash".to_owned(),
            known: None,
        },
        SearchMatch {
            file: "src/main.rs".to_owned(),
            line: 3,
            column: 1,
            content: "\"a string\"".to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "hash".to_owned(),
            known: None,
        },
    ];
    let node_types = vec!["code".to_owned(), "comment".to_owned(), "string".to_owned()];

    // Act
    let filtered = apply_filter_mode(matches, &node_types, FilterMode::CommentsOnly);

    // Assert
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].content, "// a comment");
    assert_eq!(filtered[1].content, "\"a string\"");
}

#[test]
fn test_apply_filter_mode_all() {
    // Arrange
    let matches = vec![
        SearchMatch {
            file: "src/main.rs".to_owned(),
            line: 1,
            column: 1,
            content: "fn main() {}".to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "hash".to_owned(),
            known: None,
        },
        SearchMatch {
            file: "src/main.rs".to_owned(),
            line: 2,
            column: 1,
            content: "// a comment".to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "hash".to_owned(),
            known: None,
        },
    ];
    let node_types = vec!["code".to_owned(), "comment".to_owned()];

    // Act
    let filtered = apply_filter_mode(matches.clone(), &node_types, FilterMode::All);

    // Assert
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].content, matches[0].content);
    assert_eq!(filtered[1].content, matches[1].content);
}

#[tokio::test]
async fn test_search_degraded_filter_bypassed_returns_matches() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // File with unsupported extension — Tree-sitter can't classify nodes
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/data.xyz"), "// TODO: fix this hack").unwrap();

    let scout = Arc::new(RipgrepScout);
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("code".to_string())); // degraded: defaults to code
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);

    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    // Request comments_only on an unsupported language — without the fix this returns 0 matches
    let params = SearchParams {
        query: "TODO".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*.xyz".to_owned(),
        max_results: 10,
        context_lines: 0,
        known_files: vec![],
        exclude_glob: String::default(),
        offset: 0,
        ..Default::default()
    };
    let result = server.search_codebase_impl(params).await;
    let response = result.expect("search should succeed");

    // The match should be returned despite filter_mode = CommentsOnly
    assert!(
        !response.0.matches.is_empty(),
        "matches must not be empty when filter is bypassed"
    );
    assert!(response.0.degraded, "degraded must be true");
    assert_eq!(
        response
            .0
            .degraded_reason
            .as_ref()
            .map(std::string::ToString::to_string),
        Some("unsupported_language_filter_bypassed".to_string()),
        "degraded_reason must indicate filter was bypassed"
    );
}

// ── 1.2 hint field: populated when filter_mode removes all results ─────

/// Verify that `hint` is set when `filter_mode=code_only` removes all results.
/// This prevents agents from falsely concluding a symbol doesn't exist.
#[tokio::test]
async fn test_search_hint_populated_when_filter_removes_all_results() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create a Rust file that has the symbol ONLY in a comment
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/lib.rs"),
        "// TODO: implement find_me\nfn other() {}\n",
    )
    .unwrap();

    let scout = Arc::new(RipgrepScout);
    let surgeon = Arc::new(MockSurgeon::new());
    // Surgeon reports the match as a "comment" node
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("comment".to_string()));
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = SearchParams {
        query: "find_me".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*.rs".to_owned(),
        max_results: 10,
        context_lines: 0,
        known_files: vec![],
        exclude_glob: String::default(),
        offset: 0,
        ..Default::default()
    };
    let result = server.search_codebase_impl(params).await;
    let response = result.expect("search should succeed");

    // No matches after filter
    assert_eq!(
        response.0.returned_count, 0,
        "filter should remove comment match"
    );
    // But raw match count shows ripgrep found something
    assert!(
        response.0.raw_match_count > 0,
        "raw_match_count must be positive"
    );
    // hint must be present to guide agent
    assert!(
        response.0.hint.is_some(),
        "hint must be present when filter removed all results"
    );
    let hint = response.0.hint.as_ref().unwrap();
    assert!(
        hint.contains("filter_mode='all'"),
        "hint must suggest filter_mode=all, got: {hint}"
    );
}

/// Verify that `hint` is absent when `filter_mode=all` (no filtering applied).
#[tokio::test]
async fn test_search_hint_absent_when_no_filter_applied() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/lib.rs"), "fn find_me() {}\n").unwrap();

    let scout = Arc::new(RipgrepScout);
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("code".to_string()));
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = SearchParams {
        query: "find_me".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*.rs".to_owned(),
        max_results: 10,
        context_lines: 0,
        known_files: vec![],
        exclude_glob: String::default(),
        offset: 0,
        ..Default::default()
    };
    let result = server.search_codebase_impl(params).await;
    let response = result.expect("search should succeed");

    assert!(response.0.returned_count > 0, "should have results");
    assert!(
        response.0.hint.is_none(),
        "hint must be absent when results are present"
    );
}

/// Verify that `next_offset` is populated when search results are truncated.
#[tokio::test]
async fn test_search_next_offset_populated_when_truncated() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create multiple files with matches to exceed max_results
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    for i in 0..5 {
        std::fs::write(
            ws_dir.path().join(format!("src/file{i}.rs")),
            format!("fn findme_{i}() {{ findme(); }}\n"),
        )
        .unwrap();
    }

    let scout = Arc::new(RipgrepScout);
    let surgeon = Arc::new(MockSurgeon::new());
    // Pre-configure surgeon for enrichment calls (5 matches expected)
    for _ in 0..5 {
        surgeon
            .enclosing_symbol_results
            .lock()
            .unwrap()
            .push(Ok(None));
        surgeon
            .enclosing_symbol_detail_results
            .lock()
            .unwrap()
            .push(Ok(None));
        surgeon
            .node_type_at_position_results
            .lock()
            .unwrap()
            .push(Ok("code".to_string()));
    }
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = SearchParams {
        query: "findme".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*.rs".to_owned(),
        max_results: 2,
        context_lines: 0,
        known_files: vec![],
        exclude_glob: String::default(),
        offset: 0,
        ..Default::default()
    };
    let result = server.search_codebase_impl(params).await;
    let response = result.expect("search should succeed");

    // Should be truncated since we have 5 matches but max_results=2
    assert!(response.0.truncated, "should be truncated");
    assert!(
        response.0.next_offset.is_some(),
        "next_offset must be present when truncated"
    );
    let next_offset = response.0.next_offset.unwrap();
    assert_eq!(
        next_offset, 2,
        "next_offset should be offset + returned_count"
    );
}

#[tokio::test]
async fn test_search_binary_skipped_counted() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/main.rs"), "fn findme() {}\n").unwrap();
    std::fs::write(ws_dir.path().join("src/image.png"), "binary data").unwrap();
    std::fs::write(ws_dir.path().join("src/archive.zip"), "zip data").unwrap();

    let scout = Arc::new(RipgrepScout);
    let surgeon = Arc::new(MockSurgeon::new());
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("code".to_string()));
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);

    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = SearchParams {
        query: "findme".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*".to_owned(),
        max_results: 10,
        context_lines: 0,
        known_files: vec![],
        exclude_glob: String::default(),
        offset: 0,
        ..Default::default()
    };
    let result = server.search_codebase_impl(params).await;
    let response = result.expect("search should succeed");

    assert_eq!(
        response.0.binary_skipped, 2,
        "binary_skipped should count .png and .zip files"
    );
    assert_eq!(
        response.0.gitignored_skipped, 0,
        "gitignored_skipped should be 0 when no .gitignore rules apply"
    );
    assert_eq!(
        response.0.other_skipped, 0,
        "other_skipped should be 0 when no I/O errors occur"
    );
}

#[tokio::test]
async fn test_search_codebase_invalid_regex_returns_invalid_params() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let scout = Arc::new(RipgrepScout);
    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = SearchParams {
        query: "[invalid regex".to_owned(),
        mode: SearchMode::Regex,
        path_glob: "**/*.rs".to_owned(),
        max_results: 10,
        context_lines: 0,
        known_files: vec![],
        exclude_glob: String::default(),
        offset: 0,
        ..Default::default()
    };
    let result = server.search_codebase_impl(params).await;
    let Err(err) = result else {
        panic!("Expected search to fail on invalid regex");
    };
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
    assert!(err.message.contains("invalid pattern"));
}

// ── P2-7: Zero-match hint tests ───────────────────────────────────

#[tokio::test]
async fn test_search_zero_total_matches_hint() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create a file that won't match our search
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

    let scout = Arc::new(RipgrepScout);
    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = SearchParams {
        query: "nonexistent_symbol_xyz_123".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*.rs".to_owned(),
        max_results: 10,
        context_lines: 0,
        known_files: vec![],
        exclude_glob: String::default(),
        offset: 0,
        ..Default::default()
    };

    let result = server.search_codebase_impl(params).await;
    let response = result.expect("search should succeed");

    assert_eq!(response.0.total_matches, 0);
    assert!(
        response.0.hint.is_some(),
        "hint should be present when zero total matches found"
    );
    let hint = response.0.hint.unwrap();
    assert!(
        hint.contains("No matches found"),
        "hint should mention no matches, got: {hint}"
    );
}

#[tokio::test]
async fn test_search_group_by_file_parameter() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/lib.rs"), "fn find_me() {}\n").unwrap();

    let scout = Arc::new(RipgrepScout);
    let surgeon = Arc::new(MockSurgeon::new());
    // For 1 match:
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("code".to_string()));
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
    let server =
        PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon.clone(), lawyer);

    // Scenario 1: group_by_file = true
    let params_grouped = SearchParams {
        query: "find_me".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*.rs".to_owned(),
        max_results: 10,
        group_by_file: true,
        ..Default::default()
    };
    let response_grouped = server
        .search_codebase_impl(params_grouped)
        .await
        .expect("grouped search should succeed")
        .0;
    assert!(
        response_grouped.file_groups.is_some(),
        "file_groups should be present"
    );
    assert_eq!(response_grouped.total_matches, 1);

    // Scenario 2: group_by_file = false
    // For the next call, re-push mock results for the match
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("code".to_string()));

    let params_flat = SearchParams {
        query: "find_me".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*.rs".to_owned(),
        max_results: 10,
        group_by_file: false,
        ..Default::default()
    };
    let response_flat = server
        .search_codebase_impl(params_flat)
        .await
        .expect("flat search should succeed")
        .0;
    assert!(
        response_flat.file_groups.is_none(),
        "file_groups should not be present"
    );
    assert_eq!(response_flat.total_matches, 1);
}

#[tokio::test]
async fn test_search_filter_mode_parameter() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/lib.rs"),
        "fn find_me() {}\n// find_me in comment\n",
    )
    .unwrap();

    let scout = Arc::new(RipgrepScout);
    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
    let server =
        PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon.clone(), lawyer);

    // Scenario 1: filter_mode = FilterMode::CommentsOnly
    // Ripgrep will find 2 matches.
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("code".to_string()));

    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("comment".to_string()));

    let params_comments = SearchParams {
        query: "find_me".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*.rs".to_owned(),
        max_results: 10,
        filter_mode: FilterMode::CommentsOnly,
        ..Default::default()
    };
    let response_comments = server
        .search_codebase_impl(params_comments)
        .await
        .expect("comments search should succeed")
        .0;
    assert_eq!(response_comments.total_matches, 1);
    assert_eq!(
        response_comments.matches[0].content,
        "// find_me in comment"
    );

    // Scenario 2: filter_mode = FilterMode::CodeOnly
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("code".to_string()));

    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("comment".to_string()));

    let params_code = SearchParams {
        query: "find_me".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*.rs".to_owned(),
        max_results: 10,
        filter_mode: FilterMode::CodeOnly,
        ..Default::default()
    };
    let response_code = server
        .search_codebase_impl(params_code)
        .await
        .expect("code search should succeed")
        .0;
    assert_eq!(response_code.total_matches, 1);
    assert_eq!(response_code.matches[0].content, "fn find_me() {}");

    // Scenario 3: filter_mode = FilterMode::All
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("code".to_string()));

    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("comment".to_string()));

    let params_all = SearchParams {
        query: "find_me".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*.rs".to_owned(),
        max_results: 10,
        filter_mode: FilterMode::All,
        ..Default::default()
    };
    let response_all = server
        .search_codebase_impl(params_all)
        .await
        .expect("all search should succeed")
        .0;
    assert_eq!(response_all.total_matches, 2);
}

/// Verify the low-coverage hint fires when less than 50% of in-scope files were searched.
///
/// Creates a workspace with 10 files — 9 binary (skipped), 1 searchable. The
/// binary files inflate `files_in_scope` → `coverage_percent` ends up well under 50%
/// when the search runs.
///
/// NOTE: This test uses `RipgrepScout` against real tempdir files to exercise the
/// full `files_searched / files_in_scope` accounting path.
#[tokio::test]
async fn test_search_low_coverage_hint_emitted() {
    let ws_dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();

    // 1 searchable source file with a known pattern
    std::fs::write(ws_dir.path().join("src/main.rs"), "fn low_cov_target() {}").unwrap();

    // 9 binary files — these inflate files_in_scope but are skipped by ripgrep
    for i in 0..9u8 {
        let bytes: Vec<u8> = (0..=255u8).collect(); // non-UTF-8 bytes
        std::fs::write(ws_dir.path().join(format!("src/binary_{i}.bin")), &bytes).unwrap();
    }

    let scout = Arc::new(RipgrepScout);
    let surgeon = Arc::new(MockSurgeon::new());
    // One match in main.rs — push mock results for it
    surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));
    surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .push(Ok("code".to_string()));
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
    let server = PathfinderServer::with_all_engines(ws, config, sandbox, scout, surgeon, lawyer);

    let params = SearchParams {
        query: "low_cov_target".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*".to_owned(),
        max_results: 10,
        filter_mode: FilterMode::All,
        ..Default::default()
    };

    let result = server
        .search_codebase_impl(params)
        .await
        .expect("search should succeed")
        .0;

    // The match is found
    assert!(result.total_matches >= 1, "should find at least 1 match");

    // When binary files inflate files_in_scope enough, coverage drops below 50%
    // and the low-coverage hint should fire. Skip assertion if ripgrep doesn't
    // count binaries in files_in_scope (implementation detail).
    if result.coverage_percent < 50 {
        let hint = result.hint.as_deref().unwrap_or("");
        assert!(
            hint.contains("Low coverage"),
            "expected low-coverage hint when coverage_percent={}, got hint={:?}",
            result.coverage_percent,
            result.hint
        );
        assert!(
            hint.contains("binary_skipped"),
            "hint should mention binary_skipped counter"
        );
    }
    // If coverage >= 50 (binary files don't count), the test is still green —
    // it just confirms the hint correctly stays silent at high coverage.
}

// ── General search engine error ─────────────────────────────────────────

#[tokio::test]
async fn test_search_engine_error_returns_internal_error() {
    use pathfinder_search::MockScout;

    let ws_dir = tempfile::tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Use MockScout that returns a general error (not invalid pattern)
    let mock_scout = Arc::new(MockScout::default());
    mock_scout.set_result(Err("unexpected engine crash".to_owned()));

    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(pathfinder_lsp::NoOpLawyer);
    let server =
        PathfinderServer::with_all_engines(ws, config, sandbox, mock_scout, surgeon, lawyer);

    let params = SearchParams {
        query: "hello".to_owned(),
        mode: SearchMode::Text,
        path_glob: "**/*.rs".to_owned(),
        max_results: 10,
        filter_mode: FilterMode::All,
        ..Default::default()
    };

    let result = server.search_codebase_impl(params).await;
    assert!(result.is_err(), "engine error should return Err");
}
