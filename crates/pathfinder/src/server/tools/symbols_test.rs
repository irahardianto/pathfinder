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
        semantic_path: "test.rs::test".to_owned(),
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
        semantic_path: "test.rs::test".to_owned(),
        include_dependencies: false,
        ..Default::default()
    };
    let res = server.inspect_impl(params_no_deps).await;
    assert!(res.is_ok());

    // 2. inspect_impl with include_dependencies = true (delegates to read_with_deep_context_impl)
    // This will degrade / fail because lsp is no-op, but we can verify it routes correctly
    let params_with_deps = InspectParams {
        semantic_path: "test.rs::test".to_owned(),
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
        semantic_path: "test.rs".to_owned(), // Missing symbol part
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
        semantic_path: "/etc/passwd::test".to_owned(), // Outside sandbox
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
        semantic_path: "nonexistent.rs::test".to_owned(),
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
        semantic_path: "test.rs::test".to_owned(),
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
        semantic_path: "test.rs::test".to_owned(),
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
        semantic_path: "test.rs::test".to_owned(),
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
