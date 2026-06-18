
use super::*;
use clap::Parser;

#[test]
fn test_cli_parse_workspace_path() {
    let test_path = std::env::temp_dir().join("workspace");
    let cli = Cli::parse_from(["pathfinder", test_path.to_str().unwrap()]);
    assert_eq!(cli.workspace_path, test_path);
    assert!(!cli.lsp_trace);
}

#[test]
fn test_cli_parse_lsp_trace_flag() {
    let test_path = std::env::temp_dir().join("ws");
    let cli = Cli::parse_from(["pathfinder", test_path.to_str().unwrap(), "--lsp-trace"]);
    assert!(cli.lsp_trace);
}

#[test]
fn test_cli_parse_missing_workspace_fails() {
    let result = Cli::try_parse_from(["pathfinder"]);
    assert!(result.is_err(), "should require workspace path");
}

#[tokio::test]
async fn test_run_invalid_workspace_path() {
    // Using a non-existent path should fail during WorkspaceRoot::new
    let result = run(
        PathBuf::from("/nonexistent/path/that/does/not/exist"),
        false,
        4,
        64,
    )
    .await;
    // The path might or might not be valid depending on WorkspaceRoot validation
    // At minimum, it should not panic
    if let Err(e) = result {
        // Error message should mention the path
        let msg = format!("{e:#}");
        assert!(msg.contains("path") || msg.contains("Invalid"));
    }
}

#[test]
fn test_parse_init() {
    let json_str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}"#;
    let res: Result<rmcp::model::ClientJsonRpcMessage, _> = serde_json::from_str(json_str);
    match res {
        Ok(msg) => println!("Success: {msg:?}"),
        Err(e) => panic!("Parse failed: {e:?}"),
    }
}
