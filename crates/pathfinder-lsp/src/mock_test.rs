use super::*;
use std::path::PathBuf;

fn workspace() -> PathBuf {
    PathBuf::from("/workspace")
}

fn file() -> PathBuf {
    PathBuf::from("src/main.rs")
}

#[tokio::test]
async fn test_mock_defaults_to_none() {
    let mock = MockLawyer::default();
    let result = mock.goto_definition(&workspace(), &file(), 1, 1).await;
    assert!(matches!(result, Ok(None)));
}

#[tokio::test]
async fn test_mock_returns_configured_definition() {
    let mock = MockLawyer::default();
    let expected = DefinitionLocation {
        file: "src/auth.rs".into(),
        line: 42,
        column: 5,
        preview: "pub fn login() {".into(),
    };
    mock.set_goto_definition_result(Ok(Some(expected.clone())));

    let result = mock
        .goto_definition(&workspace(), &file(), 10, 15)
        .await
        .expect("should succeed");
    assert_eq!(result, Some(expected));
}

#[tokio::test]
async fn test_mock_records_calls() {
    let mock = MockLawyer::default();
    let _ = mock.goto_definition(&workspace(), &file(), 5, 10).await;
    let _ = mock.goto_definition(&workspace(), &file(), 20, 3).await;

    assert_eq!(mock.goto_definition_call_count(), 2);
    let calls = mock.goto_definition_calls();
    assert_eq!(calls[0], ("src/main.rs".into(), 5, 10));
    assert_eq!(calls[1], ("src/main.rs".into(), 20, 3));
}

#[tokio::test]
async fn test_mock_returns_error_when_configured() {
    let mock = MockLawyer::default();
    mock.set_goto_definition_result(Err(LspError::Protocol("LSP crashed".to_string())));
    let result = mock.goto_definition(&workspace(), &file(), 1, 1).await;
    assert!(result.is_err());
}
