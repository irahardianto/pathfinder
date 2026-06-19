use super::*;
use crate::server::types::{Detail, ExploreParams, InspectParams, ReadParams, SearchParams};
use pathfinder_search::{MockScout, SearchMatch, SearchResult};
use pathfinder_treesitter::mock::MockSurgeon;
use pathfinder_treesitter::surgeon::{AccessLevel, ExtractedSymbol, SymbolKind};
use rmcp::model::ErrorCode;
use std::fs;
use tempfile::tempdir;

#[tokio::test]
async fn test_get_repo_map_success() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .generate_skeleton_results
        .lock()
        .unwrap()
        .push(Ok(pathfinder_treesitter::repo_map::RepoMapResult {
            skeleton: "class Mock {}".to_string(),
            tech_stack: vec!["TypeScript".to_string()],
            files_scanned: 1,
            files_truncated: 0,
            truncated_paths: vec![],
            files_in_scope: 1,
            coverage_percent: 100,
            version_hashes: std::collections::HashMap::default(),
        }));

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
    );

    let params = ExploreParams {
        path: ".".to_owned(),
        detail: Detail::Symbols,
        max_tokens: 16_000,
        depth: 3,
        visibility: pathfinder_common::types::Visibility::Public,
        max_tokens_per_file: 2000,
        changed_since: String::default(),
        include_extensions: vec![],
        exclude_extensions: vec![],
    };

    let result = server.get_repo_map_impl(params).await;
    assert!(result.is_ok());
    let call_res = result.unwrap();
    let skeleton = match &call_res.content[0].raw {
        rmcp::model::RawContent::Text(t) => t.text.clone(),
        _ => panic!("expected text content"),
    };
    let response: crate::server::types::GetRepoMapMetadata =
        serde_json::from_value(call_res.structured_content.unwrap()).unwrap();
    assert!(
        skeleton.starts_with("class Mock {}"),
        "skeleton: {skeleton}"
    );
    assert_eq!(response.files_scanned, 1);
    assert_eq!(response.coverage_percent, 100);
    // Visibility filtering is now implemented via name-convention heuristics.
    assert_eq!(response.visibility_degraded, None);
}

#[tokio::test]
async fn test_get_repo_map_visibility_not_degraded() {
    // Both visibility modes should return visibility_degraded: None
    // because visibility filtering is now implemented via name-convention heuristics.
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .generate_skeleton_results
        .lock()
        .unwrap()
        .push(Ok(pathfinder_treesitter::repo_map::RepoMapResult {
            skeleton: String::default(),
            tech_stack: vec![],
            files_scanned: 0,
            files_truncated: 0,
            truncated_paths: vec![],
            files_in_scope: 0,
            coverage_percent: 100,
            version_hashes: std::collections::HashMap::default(),
        }));

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
    );

    let params = ExploreParams {
        visibility: pathfinder_common::types::Visibility::All,
        ..Default::default()
    };
    let result = server
        .get_repo_map_impl(params)
        .await
        .expect("should succeed");
    let meta: crate::server::types::GetRepoMapMetadata =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();
    assert_eq!(
        meta.visibility_degraded, None,
        "visibility filtering is implemented; visibility_degraded must be None"
    );
}

#[tokio::test]
async fn test_get_repo_map_access_denied() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_surgeon = MockSurgeon::new();
    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
    );

    let params = ExploreParams {
        path: ".env".to_string(), // Sandbox should deny this
        ..Default::default()
    };

    let Err(err) = server.get_repo_map_impl(params).await else {
        panic!("Expected ACCESS_DENIED error");
    };
    assert_eq!(err.code, ErrorCode(-32001));
}

#[tokio::test]
async fn test_search_codebase_routes_to_scout_and_handles_success() {
    let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_scout = MockScout::default();
    mock_scout.set_result(Ok(SearchResult {
        matches: vec![SearchMatch {
            file: "src/main.rs".to_owned(),
            line: 10,
            column: 5,
            content: "test_query()".to_owned(),
            context_before: vec![],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "sha256:123".to_owned(),
            known: None,
        }],
        total_matches: 1,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let mock_surgeon = Arc::new(MockSurgeon::new());
    mock_surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(Some("test_query_func".to_owned())));
    mock_surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(Some(ExtractedSymbol {
            name: "test_query_func".to_owned(),
            semantic_path: "test_query_func".to_owned(),
            kind: SymbolKind::Function,
            byte_range: 0..1,
            start_line: 0,
            end_line: 0,
            name_column: 0,
            access_level: AccessLevel::Public,
            children: vec![],
        })));

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(mock_scout.clone()),
        mock_surgeon.clone(),
    );
    let params = SearchParams {
        query: "test_query".to_owned(),
        mode: crate::server::types::SearchMode::Regex,
        ..Default::default()
    };

    let result = server.search_codebase_impl(params).await;
    // Json(val) gives us val.0
    let val = result.expect("search_codebase should succeed").0;

    assert_eq!(val.total_matches, 1);
    assert!(!val.truncated);
    let matches = val.matches;
    assert_eq!(matches[0].file, "src/main.rs");
    assert_eq!(matches[0].content, "test_query()");
    assert_eq!(
        matches[0].enclosing_semantic_path.as_deref(),
        Some("src/main.rs::test_query_func")
    );

    let calls = mock_scout.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].query, "test_query");
    assert!(calls[0].is_regex);

    let surgeon_calls = mock_surgeon.enclosing_symbol_detail_calls.lock().unwrap();
    assert_eq!(surgeon_calls.len(), 1);
    assert_eq!(surgeon_calls[0].1, std::path::PathBuf::from("src/main.rs"));
    assert_eq!(surgeon_calls[0].2, 10);
}

#[tokio::test]
async fn test_search_codebase_handles_scout_error() {
    let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_scout = MockScout::default();
    mock_scout.set_result(Err("simulated engine error".to_owned()));

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(mock_scout),
        Arc::new(MockSurgeon::new()),
    );
    let params = SearchParams {
        query: "test".to_owned(),
        ..Default::default()
    };

    let result = server.search_codebase_impl(params).await;

    let err = result
        .err()
        .expect("search_codebase should return error on scout failure");
    assert_eq!(err.code, ErrorCode::INTERNAL_ERROR);
    assert_eq!(err.message, "search engine error: simulated engine error");
}

// ── filter_mode unit tests ────────────────────────────────────────

fn make_search_match(file: &str, line: u64, content: &str) -> SearchMatch {
    SearchMatch {
        file: file.to_owned(),
        line,
        column: 0,
        content: content.to_owned(),
        context_before: vec![],
        context_after: vec![],
        enclosing_semantic_path: None,
        is_definition: None,
        version_hash: "sha256:abc".to_owned(),
        known: None,
    }
}

#[tokio::test]
async fn test_search_codebase_filter_mode_code_only_drops_comments() {
    let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_scout = MockScout::default();
    mock_scout.set_result(Ok(SearchResult {
        matches: vec![
            make_search_match("src/a.go", 1, "code line"),
            make_search_match("src/a.go", 2, "// comment line"),
            make_search_match("src/a.go", 3, "another code line"),
        ],
        total_matches: 3,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let mock_surgeon = Arc::new(MockSurgeon::new());
    // 3 matches → 3 calls: code, comment, code
    // enclosing_symbol called 3 times → return None each (default "code" below)
    // enclosing_symbol_detail called 3 times → return None each
    // node_type_at_position called 3 times → pre-configure results
    mock_surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .extend([Ok(None), Ok(None), Ok(None)]);
    mock_surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .extend([Ok(None), Ok(None), Ok(None)]);
    mock_surgeon
        .node_type_at_position_results
        .lock()
        .unwrap()
        .extend([
            Ok("code".to_owned()),
            Ok("comment".to_owned()),
            Ok("code".to_owned()),
        ]);

    let server =
        PathfinderServer::with_engines(ws, config, sandbox, Arc::new(mock_scout), mock_surgeon);

    let params = SearchParams {
        query: "line".to_owned(),
        ..Default::default()
    };

    let result = server
        .search_codebase_impl(params)
        .await
        .expect("should succeed")
        .0;

    // Only the 2 code matches should survive
    assert_eq!(result.matches.len(), 2, "code_only should drop comments");
    assert_eq!(result.matches[0].content, "code line");
    assert_eq!(result.matches[1].content, "another code line");
    // raw_match_count reflects the ORIGINAL ripgrep count (before filtering)
    assert_eq!(result.raw_match_count, 3);
    // total_matches reflects the FILTERED count (after filtering)
    assert_eq!(result.total_matches, 2);
    // No degraded flag — filtering was real
    assert!(!result.degraded);
}

// ── read_file tests ──────────────────────────────────────

#[tokio::test]
async fn test_read_file_pagination() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
    );

    // Write a 10-line file
    let filepath = "config.yaml";
    let lines: Vec<String> = (1..=10).map(|i| format!("line{i}: value")).collect();
    let content = lines.join("\n");
    fs::write(ws_dir.path().join(filepath), &content).expect("write");

    // Full read
    let result = server
        .read_file_impl(ReadParams {
            filepath: Some(filepath.to_owned()),
            start_line: 1,
            max_lines_per_file: 500,
            ..Default::default()
        })
        .await
        .expect("should succeed");
    let val: crate::server::types::ReadFileMetadata =
        serde_json::from_value(result.structured_content.unwrap()).unwrap();
    assert_eq!(val.total_lines, 10);
    assert_eq!(val.lines_returned, 10);
    assert!(!val.truncated);
    assert_eq!(val.language, "yaml");

    // Paginated read — lines 3-5
    let result2 = server
        .read_file_impl(ReadParams {
            filepath: Some(filepath.to_owned()),
            start_line: 3,
            end_line: Some(5),
            ..Default::default()
        })
        .await
        .expect("should succeed");
    let val2: crate::server::types::ReadFileMetadata =
        serde_json::from_value(result2.structured_content.unwrap()).unwrap();
    assert_eq!(val2.start_line, 3);
    assert_eq!(val2.lines_returned, 3);
    assert!(val2.truncated);
    let text_content = match &result2.content[0].raw {
        rmcp::model::RawContent::Text(t) => t.text.clone(),
        _ => panic!("expected text content"),
    };
    assert!(text_content.contains("line3"));
    assert!(text_content.contains("line5"));
    assert!(!text_content.contains("line6"));

    // FILE_NOT_FOUND
    let result3 = server
        .read_file_impl(ReadParams {
            filepath: Some("nonexistent.yaml".to_owned()),
            start_line: 1,
            max_lines_per_file: 500,
            ..Default::default()
        })
        .await;
    assert!(result3.is_err());
    let Err(err) = result3 else {
        panic!("expected error")
    };
    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "FILE_NOT_FOUND", "got: {err:?}");
}

// ── read_symbol_scope tests ─────────────────────────────────────

#[tokio::test]
async fn test_read_symbol_scope_routes_to_surgeon_and_handles_success() {
    let ws_dir = tempdir().expect("temp dir");
    // Create test file so file existence check passes
    let src_dir = ws_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).expect("create src dir");
    std::fs::write(src_dir.join("auth.go"), "func Login() {}").expect("create auth.go");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let mock_surgeon = Arc::new(MockSurgeon::new());

    let content = "func Login() {}";
    let expected_scope = pathfinder_common::types::SymbolScope {
        content: content.to_owned(),
        start_line: 5,
        end_line: 7,
        name_column: 0,
        language: "go".to_owned(),
    };
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(expected_scope.clone()));

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        mock_surgeon.clone(),
    );

    let params = InspectParams {
        semantic_path: "src/auth.go::Login".to_owned(),
        ..Default::default()
    };

    let result = server.read_symbol_scope_impl(params).await;
    let val = result.expect("should succeed");

    let rmcp::model::RawContent::Text(t) = &val.content[0].raw else {
        panic!("Expected text content");
    };
    assert!(
        t.text.starts_with(&expected_scope.content),
        "text: {}",
        t.text
    );

    let metadata: crate::server::types::ReadSymbolScopeMetadata =
        serde_json::from_value(val.structured_content.expect("missing structured_content"))
            .expect("valid metadata");

    assert_eq!(metadata.start_line, expected_scope.start_line);
    assert_eq!(metadata.end_line, expected_scope.end_line);
    assert_eq!(metadata.language, expected_scope.language);

    let calls = mock_surgeon.read_symbol_scope_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
}

#[tokio::test]
async fn test_read_symbol_scope_handles_surgeon_error() {
    let ws_dir = tempdir().expect("temp dir");
    // Create test file so file existence check passes
    let src_dir = ws_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).expect("create src dir");
    std::fs::write(src_dir.join("auth.go"), "func Login() {}").expect("create auth.go");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let mock_surgeon = Arc::new(MockSurgeon::new());

    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Err(pathfinder_treesitter::SurgeonError::SymbolNotFound {
            path: "src/auth.go::Login".to_owned(),
            did_you_mean: vec!["Logout".to_owned()],
        }));

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        mock_surgeon,
    );

    let params = InspectParams {
        semantic_path: "src/auth.go::Login".to_owned(),
        ..Default::default()
    };

    let Err(err) = server.read_symbol_scope_impl(params).await else {
        panic!("Expected failed response");
    };

    assert_eq!(err.code, ErrorCode::INVALID_PARAMS); // SymbolNotFound maps to INVALID_PARAMS
    let code = err
        .data
        .as_ref()
        .unwrap()
        .get("error")
        .unwrap()
        .as_str()
        .unwrap();
    assert_eq!(code, "SYMBOL_NOT_FOUND");
}

// \u2500\u2500 E4 tests \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

// ── E4 tests ─────────────────────────────────────────────────────

/// E4.1: Matches in `known_files` must have content + context stripped,
/// while matches in other files must retain full content.
#[tokio::test]
async fn test_search_codebase_known_files_suppresses_context() {
    let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_scout = MockScout::default();
    mock_scout.set_result(Ok(SearchResult {
        matches: vec![
            SearchMatch {
                file: "src/auth.ts".to_owned(),
                line: 10,
                column: 1,
                content: "secret content".to_owned(),
                context_before: vec!["before".to_owned()],
                context_after: vec!["after".to_owned()],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:abc".to_owned(),
                known: None,
            },
            SearchMatch {
                file: "src/main.ts".to_owned(),
                line: 5,
                column: 1,
                content: "visible content".to_owned(),
                context_before: vec!["ctx_before".to_owned()],
                context_after: vec!["ctx_after".to_owned()],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:xyz".to_owned(),
                known: None,
            },
        ],
        total_matches: 2,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let mock_surgeon = Arc::new(MockSurgeon::new());
    // Two matches → two enrichment calls
    mock_surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .extend([Ok(None), Ok(None)]);
    mock_surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .extend([Ok(None), Ok(None)]);

    let server =
        PathfinderServer::with_engines(ws, config, sandbox, Arc::new(mock_scout), mock_surgeon);

    let params = SearchParams {
        query: "content".to_owned(),
        known_files: vec!["src/auth.ts".to_owned()],
        ..Default::default()
    };

    let result = server
        .search_codebase_impl(params)
        .await
        .expect("should succeed")
        .0;

    assert_eq!(result.matches.len(), 2);

    // Known file match — content + context stripped, known=true
    let known_match = result
        .matches
        .iter()
        .find(|m| m.file == "src/auth.ts")
        .unwrap();
    assert!(
        known_match.content.is_empty(),
        "content should be suppressed for known file"
    );
    assert!(
        known_match.context_before.is_empty(),
        "context_before should be empty"
    );
    assert!(
        known_match.context_after.is_empty(),
        "context_after should be empty"
    );
    assert_eq!(
        known_match.known,
        Some(true),
        "known flag must be set for known-file matches"
    );

    // Unknown file match — content retained, no known flag
    let normal_match = result
        .matches
        .iter()
        .find(|m| m.file == "src/main.ts")
        .unwrap();
    assert_eq!(normal_match.content, "visible content");
    assert_eq!(normal_match.context_before, vec!["ctx_before"]);
    assert_eq!(normal_match.context_after, vec!["ctx_after"]);
    assert_eq!(
        normal_match.known, None,
        "unknown-file matches must not have known flag"
    );
}

/// E4.1: `known_files` path normalisation — `./src/auth.ts` must match `src/auth.ts`.
#[tokio::test]
async fn test_search_codebase_known_files_path_normalisation() {
    let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_scout = MockScout::default();
    mock_scout.set_result(Ok(SearchResult {
        matches: vec![SearchMatch {
            file: "src/auth.ts".to_owned(),
            line: 1,
            column: 1,
            content: "should be stripped".to_owned(),
            context_before: vec!["before".to_owned()],
            context_after: vec![],
            enclosing_semantic_path: None,
            is_definition: None,
            version_hash: "sha256:abc".to_owned(),
            known: None,
        }],
        total_matches: 1,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let mock_surgeon = Arc::new(MockSurgeon::new());
    mock_surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));
    mock_surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .push(Ok(None));

    let server =
        PathfinderServer::with_engines(ws, config, sandbox, Arc::new(mock_scout), mock_surgeon);

    // Pass with leading "./" — should still match "src/auth.ts"
    let params = SearchParams {
        query: "stripped".to_owned(),
        known_files: vec!["./src/auth.ts".to_owned()],
        ..Default::default()
    };

    let result = server
        .search_codebase_impl(params)
        .await
        .expect("should succeed")
        .0;

    let m = &result.matches[0];
    assert!(
        m.content.is_empty(),
        "content should be suppressed despite ./ prefix"
    );
    assert!(m.context_before.is_empty());
    assert_eq!(m.known, Some(true), "known flag must be set");
}

/// E4.2: `group_by_file=true` groups matches by file with shared `version_hash`;
/// known files go into `known_matches` with minimal info.
#[tokio::test]
async fn test_search_codebase_group_by_file() {
    let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_scout = MockScout::default();
    mock_scout.set_result(Ok(SearchResult {
        matches: vec![
            // Two matches in the same known file
            SearchMatch {
                file: "src/auth.ts".to_owned(),
                line: 1,
                column: 1,
                content: "known line 1".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:auth".to_owned(),
                known: None,
            },
            SearchMatch {
                file: "src/auth.ts".to_owned(),
                line: 2,
                column: 1,
                content: "known line 2".to_owned(),
                context_before: vec![],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:auth".to_owned(),
                known: None,
            },
            // One match in a normal file
            SearchMatch {
                file: "src/main.ts".to_owned(),
                line: 5,
                column: 1,
                content: "main content".to_owned(),
                context_before: vec!["prev".to_owned()],
                context_after: vec![],
                enclosing_semantic_path: None,
                is_definition: None,
                version_hash: "sha256:main".to_owned(),
                known: None,
            },
        ],
        total_matches: 3,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let mock_surgeon = Arc::new(MockSurgeon::new());
    // 3 enrichments
    mock_surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .extend([Ok(None), Ok(None), Ok(None)]);
    mock_surgeon
        .enclosing_symbol_detail_results
        .lock()
        .unwrap()
        .extend([Ok(None), Ok(None), Ok(None)]);

    let server =
        PathfinderServer::with_engines(ws, config, sandbox, Arc::new(mock_scout), mock_surgeon);

    let params = SearchParams {
        query: "line".to_owned(),
        known_files: vec!["src/auth.ts".to_owned()],
        ..Default::default()
    };

    let result = server
        .search_codebase_impl(params)
        .await
        .expect("should succeed")
        .0;

    let groups = result
        .file_groups
        .expect("file_groups should be Some when group_by_file=true");
    assert_eq!(groups.len(), 2);

    let auth_group = groups.iter().find(|g| g.file == "src/auth.ts").unwrap();
    assert_eq!(auth_group.version_hash, "sha256:auth");
    assert!(
        auth_group.matches.is_empty(),
        "known file should have no full matches"
    );
    assert_eq!(
        auth_group.known_matches.len(),
        2,
        "known file should have 2 known_matches"
    );
    assert!(auth_group.known_matches[0].known);

    let main_group = groups.iter().find(|g| g.file == "src/main.ts").unwrap();
    assert_eq!(main_group.version_hash, "sha256:main");
    assert_eq!(main_group.matches.len(), 1);
    // GroupedMatch has no file/version_hash — those are at group level only
    assert_eq!(main_group.matches[0].content, "main content");
    assert_eq!(main_group.matches[0].line, 5);
    assert!(main_group.known_matches.is_empty());
}

/// E4.3: `exclude_glob` is forwarded to the scout as part of `SearchParams`.
#[tokio::test]
async fn test_search_codebase_exclude_glob_forwarded_to_scout() {
    let ws = WorkspaceRoot::new(std::env::temp_dir()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_scout = MockScout::default();
    mock_scout.set_result(Ok(SearchResult {
        matches: vec![],
        total_matches: 0,
        truncated: false,
        files_searched: 0,
        files_in_scope: 0,
        binary_skipped: 0,
        gitignored_skipped: 0,
        other_skipped: 0,
    }));

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(mock_scout.clone()),
        Arc::new(MockSurgeon::new()),
    );

    let params = SearchParams {
        query: "anything".to_owned(),
        exclude_glob: "**/*.test.*".to_owned(),
        ..Default::default()
    };

    server
        .search_codebase_impl(params)
        .await
        .expect("should succeed");

    let calls = mock_scout.calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].exclude_glob, "**/*.test.*",
        "exclude_glob must be forwarded to the scout"
    );
}

// ── Server constructor tests (WP-5) ─────────────────────────────────

#[tokio::test]
async fn test_with_all_engines_constructs_functional_server() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
        Arc::new(pathfinder_lsp::MockLawyer::default()),
    );

    // Verify server functions — get_info should work
    let info = server.get_info();
    assert_eq!(info.server_info.name, "pathfinder");
}

#[tokio::test]
async fn test_with_engines_uses_no_op_lawyer() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create a Rust file for surgeon to read
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/lib.rs"), "fn hello() -> i32 { 1 }").unwrap();

    let mock_surgeon = Arc::new(MockSurgeon::new());
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(pathfinder_common::types::SymbolScope {
            content: "fn hello() -> i32 { 1 }".to_owned(),
            start_line: 0,
            end_line: 0,
            name_column: 0,
            language: "rust".to_owned(),
        }));

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        mock_surgeon,
    );

    // Navigation with NoOpLawyer should degrade gracefully
    let params = crate::server::types::LocateParams {
        semantic_path: Some("src/lib.rs::hello".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    // Should fail because NoOpLawyer returns NoLspAvailable and no grep fallback match
    assert!(result.is_err());
}

#[tokio::test]
async fn test_read_file_not_found() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = crate::server::types::ReadParams {
        filepath: Some("missing.txt".to_owned()),
        start_line: 1,
        max_lines_per_file: 100,
        ..Default::default()
    };
    let result = server.read_file_impl(params).await;
    let Err(err) = result else {
        panic!("expected error");
    };
    let code = err
        .data
        .as_ref()
        .and_then(|d| d.get("error"))
        .and_then(|v| v.as_str())
        .unwrap_or("");
    assert_eq!(code, "FILE_NOT_FOUND");
}

#[tokio::test]
async fn test_deserialization_error_wrapping() {
    // Deserialization errors are now handled by rmcp's `FromContextPart`
    // impl (tested in integration tests). This test verifies that the impl
    // layer correctly rejects semantically invalid but structurally valid
    // params (empty filepath → FILE_NOT_FOUND).
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
    );

    // Structurally valid params but semantically invalid (empty filepath).
    let params = ReadParams {
        filepath: Some(String::new()),
        start_line: 0,
        max_lines_per_file: 500,
        ..Default::default()
    };

    let result = server.read_file_impl(params).await;
    assert!(result.is_err(), "empty filepath should error");
}

#[tokio::test]
async fn test_server_new_constructor() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let server = PathfinderServer::new(ws, config).await;

    // Verify server basic capability
    let info = server.get_info();
    assert_eq!(info.server_info.name, "pathfinder");
}

#[tokio::test]
async fn test_thin_outer_handlers() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let mock_scout = MockScout::default();
    let mock_surgeon = MockSurgeon::new();

    // Mock surgeon explore response
    mock_surgeon
        .generate_skeleton_results
        .lock()
        .unwrap()
        .push(Ok(pathfinder_treesitter::repo_map::RepoMapResult {
            skeleton: "class Test {}".to_string(),
            tech_stack: vec!["TypeScript".to_string()],
            files_scanned: 1,
            files_truncated: 0,
            truncated_paths: vec![],
            files_in_scope: 1,
            coverage_percent: 100,
            version_hashes: std::collections::HashMap::default(),
        }));

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(mock_scout),
        Arc::new(mock_surgeon),
    );

    // 1. explore
    let explore_params = ExploreParams {
        path: ".".to_owned(),
        ..Default::default()
    };
    let res = server.explore(Parameters(explore_params)).await;
    assert!(res.is_ok());

    // 2. search
    let search_params = SearchParams {
        query: "test".to_owned(),
        ..Default::default()
    };
    let res = server.search(Parameters(search_params)).await;
    assert!(res.is_ok());

    // 3. read
    let read_params = ReadParams {
        filepath: Some("nonexistent.txt".to_string()),
        ..Default::default()
    };
    let res = server.read(Parameters(read_params)).await;
    assert!(res.is_err()); // nonexistent file should error

    // 4. inspect
    let inspect_params = InspectParams {
        semantic_path: "src/lib.rs::hello".to_string(),
        ..Default::default()
    };
    let res = server.inspect(Parameters(inspect_params)).await;
    assert!(res.is_err()); // nonexistent file in path

    // 5. locate
    let locate_params = LocateParams {
        semantic_path: Some("src/lib.rs::hello".to_string()),
        ..Default::default()
    };
    let res = server.locate(Parameters(locate_params)).await;
    assert!(res.is_err());

    // 6. trace
    let trace_params = TraceParams {
        semantic_path: "src/lib.rs::hello".to_string(),
        ..Default::default()
    };
    let res = server.trace(Parameters(trace_params)).await;
    assert!(res.is_err());

    // 7. health
    let health_params = HealthParams::default();
    let res = server.health(Parameters(health_params)).await;
    assert!(res.is_ok());
}
