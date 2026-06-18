use super::*;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::WorkspaceRoot;
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use std::sync::Arc;

fn make_server(ws: WorkspaceRoot, mock_surgeon: MockSurgeon) -> PathfinderServer {
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(mock_surgeon),
    )
}

#[tokio::test]
async fn test_get_semantic_path_found() {
    let ws_dir = tempfile::tempdir().expect("temp dir");
    // Create a real file so the existence check passes.
    let file_rel = "src/auth.rs";
    let file_abs = ws_dir.path().join(file_rel);
    std::fs::create_dir_all(file_abs.parent().unwrap()).expect("create dir");
    std::fs::write(&file_abs, "fn login() {}").expect("write file");

    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(Some("login".to_owned())));

    let server = make_server(ws, mock_surgeon);
    let params = LocateParams {
        file: Some(file_rel.to_owned()),
        line: Some(1),
        ..Default::default()
    };

    let result = server.get_semantic_path_impl(params).await;
    assert!(result.is_ok(), "expected Ok, got: {:?}", result.err());
    let call = result.unwrap();

    // Verify structured content has semantic_path
    let meta: GetSemanticPathResult =
        serde_json::from_value(call.structured_content.unwrap()).unwrap();
    assert_eq!(meta.semantic_path, Some("src/auth.rs::login".to_owned()));
    assert_eq!(meta.symbol, Some("login".to_owned()));
    assert_eq!(meta.line, 1);
}

#[tokio::test]
async fn test_get_semantic_path_not_in_symbol() {
    let ws_dir = tempfile::tempdir().expect("temp dir");
    let file_rel = "src/lib.rs";
    let file_abs = ws_dir.path().join(file_rel);
    std::fs::create_dir_all(file_abs.parent().unwrap()).expect("create dir");
    std::fs::write(&file_abs, "use std::io;").expect("write file");

    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let mock_surgeon = MockSurgeon::new();
    // None = line is not inside any named symbol
    mock_surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));

    let server = make_server(ws, mock_surgeon);
    let params = LocateParams {
        file: Some(file_rel.to_owned()),
        line: Some(1),
        ..Default::default()
    };

    let result = server.get_semantic_path_impl(params).await;
    assert!(result.is_ok());
    let call = result.unwrap();

    let meta: GetSemanticPathResult =
        serde_json::from_value(call.structured_content.unwrap()).unwrap();
    assert!(meta.semantic_path.is_none());
    assert!(meta.symbol.is_none());
}

#[tokio::test]
async fn test_get_semantic_path_file_not_found() {
    let ws_dir = tempfile::tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let mock_surgeon = MockSurgeon::new();

    let server = make_server(ws, mock_surgeon);
    let params = LocateParams {
        file: Some("nonexistent.rs".to_owned()),
        line: Some(5),
        ..Default::default()
    };

    let result = server.get_semantic_path_impl(params).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    // Should be INVALID_PARAMS (-32602) for a missing file
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[tokio::test]
async fn test_get_semantic_path_sandbox_denied() {
    let ws_dir = tempfile::tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let mock_surgeon = MockSurgeon::new();

    let server = make_server(ws, mock_surgeon);
    // .env is a hardcoded sandbox deny
    let params = LocateParams {
        file: Some(".env".to_owned()),
        line: Some(1),
        ..Default::default()
    };

    let result = server.get_semantic_path_impl(params).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode(-32001));
}

#[tokio::test]
async fn test_get_semantic_path_missing_file_param() {
    let ws_dir = tempfile::tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let mock_surgeon = MockSurgeon::new();

    let server = make_server(ws, mock_surgeon);
    let params = LocateParams {
        file: None,
        line: Some(1),
        ..Default::default()
    };

    let result = server.get_semantic_path_impl(params).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[tokio::test]
async fn test_get_semantic_path_missing_line_param() {
    let ws_dir = tempfile::tempdir().expect("temp dir");
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let mock_surgeon = MockSurgeon::new();

    let server = make_server(ws, mock_surgeon);
    let params = LocateParams {
        file: Some("src/auth.rs".to_owned()),
        line: None,
        ..Default::default()
    };

    let result = server.get_semantic_path_impl(params).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[tokio::test]
async fn test_get_semantic_path_enclosing_symbol_error() {
    let ws_dir = tempfile::tempdir().expect("temp dir");
    let file_rel = "src/auth.rs";
    let file_abs = ws_dir.path().join(file_rel);
    std::fs::create_dir_all(file_abs.parent().unwrap()).expect("create dir");
    std::fs::write(&file_abs, "fn login() {}").expect("write file");

    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Err(pathfinder_treesitter::error::SurgeonError::Io(
            std::sync::Arc::new(std::io::Error::other("disk error")),
        )));

    let server = make_server(ws, mock_surgeon);
    let params = LocateParams {
        file: Some(file_rel.to_owned()),
        line: Some(1),
        ..Default::default()
    };

    let result = server.get_semantic_path_impl(params).await;
    assert!(result.is_err(), "surgeon error should propagate");
}

#[tokio::test]
async fn test_get_semantic_path_none_symbol_text_format() {
    let ws_dir = tempfile::tempdir().expect("temp dir");
    let file_rel = "src/lib.rs";
    let file_abs = ws_dir.path().join(file_rel);
    std::fs::create_dir_all(file_abs.parent().unwrap()).expect("create dir");
    std::fs::write(&file_abs, "use std::io;").expect("write file");

    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let mock_surgeon = MockSurgeon::new();
    mock_surgeon
        .enclosing_symbol_results
        .lock()
        .unwrap()
        .push(Ok(None));

    let server = make_server(ws, mock_surgeon);
    let params = LocateParams {
        file: Some(file_rel.to_owned()),
        line: Some(1),
        ..Default::default()
    };

    let result = server.get_semantic_path_impl(params).await;
    assert!(result.is_ok());
    let call = result.unwrap();

    // Verify text output contains the expected guidance message
    let text = call
        .content
        .first()
        .and_then(|c| match &c.raw {
            rmcp::model::RawContent::Text(t) => Some(t.text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    assert!(
        text.contains("is not inside a named symbol"),
        "text should contain guidance for line not in symbol, got: {text}"
    );
    assert!(
        text.contains("read(filepath="),
        "text should suggest using read tool, got: {text}"
    );
}
