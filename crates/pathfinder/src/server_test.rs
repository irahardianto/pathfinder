use super::*;
use crate::server::types::{InspectParams, LocateParams, TraceParams};
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use rmcp::model::ErrorCode;
use tempfile::tempdir;

// ── read_symbol_scope tests ─────────────────────────────────────

#[tokio::test]
async fn test_read_symbol_scope_routes_to_surgeon_and_handles_success() {
    let ws_dir = tempdir().expect("temp dir");
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
        ..Default::default()
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
        semantic_path: Some("src/auth.go::Login".to_owned()),
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

    assert_eq!(metadata.start_line, expected_scope.start_line + 1);
    assert_eq!(metadata.end_line, expected_scope.end_line + 1);
    assert_eq!(metadata.language, expected_scope.language);

    let calls = mock_surgeon.read_symbol_scope_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
}

#[tokio::test]
async fn test_read_symbol_scope_handles_surgeon_error() {
    let ws_dir = tempdir().expect("temp dir");
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
        semantic_path: Some("src/auth.go::Login".to_owned()),
        ..Default::default()
    };

    let Err(err) = server.read_symbol_scope_impl(params).await else {
        panic!("Expected failed response");
    };

    assert_eq!(err.code, ErrorCode::INVALID_PARAMS);
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

// ── Server constructor tests ────────────────────────────────────────

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

    let info = server.get_info();
    assert_eq!(info.server_info.name, "pathfinder");
}

#[tokio::test]
async fn test_with_engines_uses_no_op_lawyer() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

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
            ..Default::default()
        }));

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        mock_surgeon,
    );

    let params = LocateParams {
        semantic_path: Some("src/lib.rs::hello".to_owned()),
        ..Default::default()
    };
    let result = server.get_definition_impl(params).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_server_new_constructor() {
    let ws_dir = tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let server = PathfinderServer::new(ws, config).await;

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

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(mock_scout),
        Arc::new(mock_surgeon),
    );

    // 1. inspect
    let inspect_params = InspectParams {
        semantic_path: Some("src/lib.rs::hello".to_string()),
        ..Default::default()
    };
    let res = server.inspect(Parameters(inspect_params)).await;
    assert!(res.is_err()); // nonexistent file in path

    // 2. locate
    let locate_params = LocateParams {
        semantic_path: Some("src/lib.rs::hello".to_string()),
        ..Default::default()
    };
    let res = server.locate(Parameters(locate_params)).await;
    assert!(res.is_err());

    // 3. trace
    let trace_params = TraceParams {
        semantic_path: "src/lib.rs::hello".to_string(),
        ..Default::default()
    };
    let res = server.trace(Parameters(trace_params)).await;
    assert!(res.is_err());
}
