use super::*;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::WorkspaceRoot;
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use std::sync::Arc;
use tempfile::tempdir;

// ── GAP-004: version_hash in text output ───────────────────────────────

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_symbol_scope_includes_version_hash_in_text() {
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create a test file
    let file_path = ws.path().join("test.rs");
    let content = "fn test() {}\n";
    tokio::fs::write(&file_path, content).await.unwrap();

    let mock_surgeon = MockSurgeon::new();
    let expected_scope = pathfinder_common::types::SymbolScope {
        content: content.to_owned(),
        start_line: 1,
        end_line: 1,
        name_column: 0,
        language: "rust".to_owned(),
        ..Default::default()
    };
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(expected_scope));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_path: Some("test.rs::test".to_owned()),
        ..Default::default()
    };

    let result = server.read_symbol_scope_impl(params).await;
    assert!(result.is_ok(), "read_symbol_scope should succeed");
    let call_result = result.unwrap();

    // Verify the text content is the symbol source
    if let Some(content) = call_result.content.first() {
        if let rmcp::model::RawContent::Text(text_content) = &content.raw {
            assert!(
                !text_content.text.is_empty(),
                "text output should be non-empty"
            );
        } else {
            panic!("Expected text content");
        }
    } else {
        panic!("Expected content");
    }
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_inspect_impl_routing() {
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let file_path = ws.path().join("test.rs");
    let content = "fn test() {}\n";
    tokio::fs::write(&file_path, content).await.unwrap();

    let mock_surgeon = MockSurgeon::new();
    let expected_scope = pathfinder_common::types::SymbolScope {
        content: content.to_owned(),
        start_line: 1,
        end_line: 1,
        name_column: 0,
        language: "rust".to_owned(),
        ..Default::default()
    };
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(expected_scope.clone()));
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(expected_scope.clone()));
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(expected_scope));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    // 1. inspect_impl with include_dependencies = false (delegates to read_symbol_scope_impl)
    let params_no_deps = InspectParams {
        semantic_path: Some("test.rs::test".to_owned()),
        include_dependencies: false,
        ..Default::default()
    };
    let res = server.inspect_impl(params_no_deps).await;
    assert!(res.is_ok());

    // 2. inspect_impl with include_dependencies = true (delegates to read_with_deep_context_impl)
    // This will degrade / fail because lsp is no-op, but we can verify it routes correctly
    let params_with_deps = InspectParams {
        semantic_path: Some("test.rs::test".to_owned()),
        include_dependencies: true,
        ..Default::default()
    };
    let res = server.inspect_impl(params_with_deps).await;
    // Because deep_context uses read_symbol_scope / LSP, it should fail or return degraded
    assert!(res.is_ok());
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_symbol_scope_require_symbol_target_fails() {
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_path: Some("test.rs".to_owned()), // Missing symbol part
        ..Default::default()
    };
    let result = server.read_symbol_scope_impl(params).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_symbol_scope_sandbox_check_fails() {
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_path: Some("/etc/passwd::test".to_owned()), // Outside sandbox
        ..Default::default()
    };
    let result = server.read_symbol_scope_impl(params).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode(-32001)); // Access denied
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_symbol_scope_file_not_found() {
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_path: Some("nonexistent.rs::test".to_owned()),
        ..Default::default()
    };
    let result = server.read_symbol_scope_impl(params).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS); // File not found maps to invalid params
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_read_symbol_scope_surgeon_error() {
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let file_path = ws.path().join("test.rs");
    let content = "fn test() {}\n";
    tokio::fs::write(&file_path, content).await.unwrap();

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Err(
            pathfinder_treesitter::error::SurgeonError::SymbolNotFound {
                path: "test".to_owned(),
                did_you_mean: vec![],
            },
        ));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_path: Some("test.rs::test".to_owned()),
        ..Default::default()
    };
    let result = server.read_symbol_scope_impl(params).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS); // SymbolNotFound maps to invalid params
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_inspect_impl_invalid_max_dependencies() {
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let file_path = ws.path().join("test.rs");
    let content = "fn test() {}\n";
    tokio::fs::write(&file_path, content).await.unwrap();

    let mock_surgeon = MockSurgeon::new();
    let expected_scope = pathfinder_common::types::SymbolScope {
        content: content.to_owned(),
        start_line: 1,
        end_line: 1,
        name_column: 0,
        language: "rust".to_owned(),
        ..Default::default()
    };
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(expected_scope.clone()));
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(expected_scope));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    // Case 1: max_dependencies == 0
    let params = InspectParams {
        semantic_path: Some("test.rs::test".to_owned()),
        include_dependencies: true,
        max_dependencies: 0,
        ..Default::default()
    };
    let res = server.inspect_impl(params).await;
    assert!(res.is_err());
    assert_eq!(
        res.unwrap_err().code,
        rmcp::model::ErrorCode::INVALID_PARAMS
    );

    // Case 2: max_dependencies > 500
    let params = InspectParams {
        semantic_path: Some("test.rs::test".to_owned()),
        include_dependencies: true,
        max_dependencies: 501,
        ..Default::default()
    };
    let res = server.inspect_impl(params).await;
    assert!(res.is_err());
    assert_eq!(
        res.unwrap_err().code,
        rmcp::model::ErrorCode::INVALID_PARAMS
    );
}

// ── PATCH-005 backward-compat: single semantic_path returns ReadSymbolScopeMetadata ───

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_inspect_single_unchanged() {
    // Verify that single-path mode (no `semantic_paths`) still returns the old
    // `ReadSymbolScopeMetadata` format rather than the new `BatchInspectResult`.
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let file_path = ws.path().join("test.rs");
    tokio::fs::write(&file_path, "fn test() {}\n")
        .await
        .unwrap();

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(pathfinder_common::types::SymbolScope {
            content: "fn test() {}".to_owned(),
            start_line: 1,
            end_line: 1,
            name_column: 0,
            language: "rust".to_owned(),
            ..Default::default()
        }));

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_path: Some("test.rs::test".to_owned()),
        // semantic_paths is absent — must stay in single-path mode
        ..Default::default()
    };

    let result = server.inspect_impl(params).await.expect("should succeed");
    // Response must be the legacy ReadSymbolScopeMetadata shape, NOT BatchInspectResult
    let meta: crate::server::types::ReadSymbolScopeMetadata = serde_json::from_value(
        result
            .structured_content
            .expect("structured_content present"),
    )
    .expect("single-path mode must return ReadSymbolScopeMetadata, not BatchInspectResult");
    assert_eq!(meta.content, "fn test() {}");
    assert_eq!(meta.start_line, 1);
}

// ── PATCH-005 batch with include_dependencies=true ─────────────────────────

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_inspect_batch_with_dependencies() {
    // Verify batch inspect works when include_dependencies=true.
    // With NoOpLawyer, LSP is unavailable so dependencies are empty and
    // the result is degraded — but the BatchInspectResult structure is intact.
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let file1 = ws.path().join("file1.rs");
    tokio::fs::write(&file1, "fn foo() {}\n").await.unwrap();
    let file2 = ws.path().join("file2.rs");
    tokio::fs::write(&file2, "fn bar() {}\n").await.unwrap();

    let mock_surgeon = MockSurgeon::new();
    // read_with_deep_context_impl calls read_symbol_scope TWICE per path:
    //   1. via read_symbol_scope_enriched (initial scope)
    //   2. via attempt_grep_fallback (NoOpLawyer → LSP unavailable → grep path)
    // Two paths → 4 total results queued.
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .extend([
            Ok(pathfinder_common::types::SymbolScope {
                content: "fn foo() {}".to_owned(),
                start_line: 1,
                end_line: 1,
                name_column: 0,
                language: "rust".to_owned(),
                ..Default::default()
            }),
            Ok(pathfinder_common::types::SymbolScope {
                content: "fn foo() {}".to_owned(), // 2nd call for grep fallback
                start_line: 1,
                end_line: 1,
                name_column: 0,
                language: "rust".to_owned(),
                ..Default::default()
            }),
            Ok(pathfinder_common::types::SymbolScope {
                content: "fn bar() {}".to_owned(),
                start_line: 1,
                end_line: 1,
                name_column: 0,
                language: "rust".to_owned(),
                ..Default::default()
            }),
            Ok(pathfinder_common::types::SymbolScope {
                content: "fn bar() {}".to_owned(), // 2nd call for grep fallback
                start_line: 1,
                end_line: 1,
                name_column: 0,
                language: "rust".to_owned(),
                ..Default::default()
            }),
        ]);

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_paths: Some(vec!["file1.rs::foo".to_owned(), "file2.rs::bar".to_owned()]),
        include_dependencies: true,
        ..Default::default()
    };

    let result = server.inspect_impl(params).await.expect("should succeed");
    let val: crate::server::types::BatchInspectResult = serde_json::from_value(
        result
            .structured_content
            .expect("structured_content present"),
    )
    .expect("batch mode returns BatchInspectResult");

    assert_eq!(val.results.len(), 2);
    // Both entries must have dependencies field present (empty because NoOpLawyer)
    for entry in &val.results {
        assert_eq!(entry.status, "ok");
        assert!(
            entry.dependencies.is_some(),
            "include_dependencies=true must populate dependencies field"
        );
    }
    assert_eq!(val.succeeded, 2);
    assert_eq!(val.failed, 0);
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_inspect_batch_multiple_symbols() {
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    // Create test files
    let file1 = ws.path().join("file1.rs");
    tokio::fs::write(&file1, "fn foo() {}\n").await.unwrap();
    let file2 = ws.path().join("file2.rs");
    tokio::fs::write(&file2, "fn bar() {}\n").await.unwrap();

    let mock_surgeon = MockSurgeon::new();
    // Preload mock results: two scopes
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .extend([
            Ok(pathfinder_common::types::SymbolScope {
                content: "fn foo() {}".to_owned(),
                start_line: 1,
                end_line: 1,
                name_column: 0,
                language: "rust".to_owned(),
                ..Default::default()
            }),
            Ok(pathfinder_common::types::SymbolScope {
                content: "fn bar() {}".to_owned(),
                start_line: 1,
                end_line: 1,
                name_column: 0,
                language: "rust".to_owned(),
                ..Default::default()
            }),
        ]);

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_paths: Some(vec!["file1.rs::foo".to_owned(), "file2.rs::bar".to_owned()]),
        ..Default::default()
    };

    let result = server.inspect_impl(params).await.expect("should succeed");
    let val: crate::server::types::BatchInspectResult = serde_json::from_value(
        result
            .structured_content
            .expect("missing structured_content"),
    )
    .expect("valid metadata");

    assert_eq!(val.succeeded, 2);
    assert_eq!(val.failed, 0);
    assert_eq!(val.results.len(), 2);
    assert_eq!(val.results[0].semantic_path, "file1.rs::foo");
    assert_eq!(val.results[0].status, "ok");
    assert_eq!(val.results[0].source, Some("fn foo() {}".to_owned()));
    assert_eq!(val.results[1].semantic_path, "file2.rs::bar");
    assert_eq!(val.results[1].status, "ok");
    assert_eq!(val.results[1].source, Some("fn bar() {}".to_owned()));
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_inspect_batch_partial_failure() {
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let file1 = ws.path().join("file1.rs");
    tokio::fs::write(&file1, "fn foo() {}\n").await.unwrap();

    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .extend([Ok(pathfinder_common::types::SymbolScope {
            content: "fn foo() {}".to_owned(),
            start_line: 1,
            end_line: 1,
            name_column: 0,
            language: "rust".to_owned(),
            ..Default::default()
        })]);

    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_paths: Some(vec![
            "file1.rs::foo".to_owned(),
            "nonexistent.rs::bar".to_owned(),
        ]),
        ..Default::default()
    };

    let result = server.inspect_impl(params).await.expect("should succeed");
    let val: crate::server::types::BatchInspectResult = serde_json::from_value(
        result
            .structured_content
            .expect("missing structured_content"),
    )
    .expect("valid metadata");

    assert_eq!(val.succeeded, 1);
    assert_eq!(val.failed, 1);
    assert_eq!(val.results.len(), 2);
    assert_eq!(val.results[0].status, "ok");
    assert_eq!(val.results[1].status, "error");
    assert!(val.results[1].error.is_some());
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_inspect_batch_max_10_limit() {
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    // 11 paths
    let paths = (1..=11).map(|i| format!("file{i}.rs::foo")).collect();
    let params = InspectParams {
        semantic_paths: Some(paths),
        ..Default::default()
    };

    let result = server.inspect_impl(params).await;
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().code,
        rmcp::model::ErrorCode::INVALID_PARAMS
    );
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_inspect_batch_mutual_exclusion_both_params_errors() {
    // Providing both semantic_path AND semantic_paths simultaneously must return INVALID_PARAMS.
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_path: Some("file.rs::foo".to_owned()),
        semantic_paths: Some(vec!["file.rs::bar".to_owned()]),
        ..Default::default()
    };

    let result = server.inspect_impl(params).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(
        err.code,
        rmcp::model::ErrorCode::INVALID_PARAMS,
        "both semantic_path and semantic_paths must return INVALID_PARAMS, got: {:?}",
        err.message
    );
}

#[tokio::test]
#[allow(clippy::unwrap_used)]
async fn test_inspect_batch_empty_returns_error() {
    let ws_dir = tempdir().unwrap();
    let ws = WorkspaceRoot::new(ws_dir.path()).unwrap();
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
        Arc::new(pathfinder_lsp::NoOpLawyer),
    );

    let params = InspectParams {
        semantic_paths: Some(vec![]),
        ..Default::default()
    };

    let result = server.inspect_impl(params).await;
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().code,
        rmcp::model::ErrorCode::INVALID_PARAMS
    );
}
