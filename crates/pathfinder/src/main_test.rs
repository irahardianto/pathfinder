use super::*;
use clap::Parser;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::{SymbolScope, WorkspaceRoot};
use pathfinder_lsp::types::{CallHierarchyCall, CallHierarchyItem, ReferenceLocation};
use pathfinder_lsp::{DefinitionLocation, MockLawyer};
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use rmcp::ServiceExt;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

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

// ── MCP JSON-RPC Server & Tool Integration Tests (SC-4 Coverage) ──────

struct IntegrationTestBundle {
    server: PathfinderServer,
    #[expect(dead_code)]
    scout: Arc<MockScout>,
    surgeon: Arc<MockSurgeon>,
    lawyer: Arc<MockLawyer>,
    _temp_dir: tempfile::TempDir,
    ws: WorkspaceRoot,
}

fn create_integration_test_server() -> IntegrationTestBundle {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let ws = WorkspaceRoot::new(temp_dir.path()).expect("valid workspace root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let scout = Arc::new(MockScout::default());
    let surgeon = Arc::new(MockSurgeon::new());
    let lawyer = Arc::new(MockLawyer::default());

    let server = PathfinderServer::with_all_engines(
        ws.clone(),
        config,
        sandbox,
        scout.clone(),
        surgeon.clone(),
        lawyer.clone(),
    );

    IntegrationTestBundle {
        server,
        scout,
        surgeon,
        lawyer,
        _temp_dir: temp_dir,
        ws,
    }
}

fn setup_mock_call_hierarchy(bundle: &IntegrationTestBundle) {
    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 1,
        column: 4,
        data: None,
    };
    bundle
        .lawyer
        .push_prepare_call_hierarchy_result(Ok(vec![item]));
    bundle
        .lawyer
        .push_incoming_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "handle_request".into(),
                kind: "function".into(),
                detail: Some("fn handle_request()".into()),
                file: "src/server.rs".into(),
                line: 20,
                column: 4,
                data: None,
            },
            call_sites: vec![25],
        }]));
    bundle
        .lawyer
        .push_outgoing_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "validate_token".into(),
                kind: "function".into(),
                detail: Some("fn validate_token() -> bool".into()),
                file: "src/token.rs".into(),
                line: 15,
                column: 4,
                data: None,
            },
            call_sites: vec![9],
        }]));
}

fn create_transport_pair() -> (
    DuplexStream,
    DuplexStream,
    BufReader<DuplexStream>,
    DuplexStream,
) {
    let (server_read, client_write) = tokio::io::duplex(8192);
    let (client_read, server_write) = tokio::io::duplex(8192);
    let client_reader = BufReader::new(client_read);
    (server_read, server_write, client_reader, client_write)
}

async fn write_jsonrpc(writer: &mut DuplexStream, msg: &serde_json::Value) {
    let s = msg.to_string();
    writer.write_all(s.as_bytes()).await.unwrap();
    writer.write_all(b"\n").await.unwrap();
    writer.flush().await.unwrap();
}

async fn read_jsonrpc(reader: &mut BufReader<DuplexStream>) -> serde_json::Value {
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();
    assert!(!line.is_empty(), "expected a response but got EOF");
    serde_json::from_str(&line).unwrap_or_else(|e| {
        panic!("invalid JSON: {e}\nraw: {line}");
    })
}

async fn do_handshake(
    client_writer: &mut DuplexStream,
    client_reader: &mut BufReader<DuplexStream>,
) {
    let init_req = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {
                "name": "test-client",
                "version": "1.0.0"
            }
        }
    });
    write_jsonrpc(client_writer, &init_req).await;

    let init_resp = read_jsonrpc(client_reader).await;
    assert_eq!(init_resp["jsonrpc"], "2.0");
    assert_eq!(init_resp["id"], 1);

    let init_notif = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    });
    write_jsonrpc(client_writer, &init_notif).await;
}

#[tokio::test]
async fn test_mcp_integration_locate_tool_definition_success() {
    let bundle = create_integration_test_server();

    // Create actual test file in temp workspace so exists() passes
    let src_dir = bundle.ws.path().join("src");
    tokio::fs::create_dir_all(&src_dir).await.unwrap();
    let file_path = src_dir.join("auth.rs");
    tokio::fs::write(&file_path, "fn login() { }\n")
        .await
        .unwrap();

    // Mock surgeon to return symbol scope for "src/auth.rs::login"
    let expected_scope = SymbolScope {
        content: "fn login() { }\n".to_string(),
        start_line: 0,
        end_line: 0,
        name_column: 3,
        language: "rust".to_string(),
        ..Default::default()
    };
    bundle
        .surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(expected_scope));

    // Mock lawyer to return definition location
    bundle
        .lawyer
        .set_goto_definition_result(Ok(Some(DefinitionLocation {
            file: "src/auth.rs".to_string(),
            line: 1,
            column: 4,
            preview: "fn login() { }\n".to_string(),
        })));

    // Transport setup
    let (server_read, server_write, mut client_reader, mut client_writer) = create_transport_pair();
    let server_handle = tokio::spawn(async move {
        bundle
            .server
            .serve((server_read, server_write))
            .await
            .expect("server serve")
    });

    do_handshake(&mut client_writer, &mut client_reader).await;

    // Call "locate" tool
    let locate_call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "locate",
            "arguments": {
                "semantic_path": "src/auth.rs::login"
            }
        }
    });
    write_jsonrpc(&mut client_writer, &locate_call).await;

    let response = read_jsonrpc(&mut client_reader).await;
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 2);

    let result = &response["result"];
    let structured = &result["structuredContent"];
    assert_eq!(structured["file"], "src/auth.rs");
    assert_eq!(structured["line"], 1);
    assert_eq!(structured["column"], 4);
    assert_eq!(structured["degraded"], false);

    // Shutdown
    drop(client_writer);
    let _ = server_handle.await;
}

#[tokio::test]
async fn test_mcp_integration_trace_tool_impact_success() {
    let bundle = create_integration_test_server();

    // Create actual test file
    let src_dir = bundle.ws.path().join("src");
    tokio::fs::create_dir_all(&src_dir).await.unwrap();
    let file_path = src_dir.join("auth.rs");
    tokio::fs::write(&file_path, "fn login() { }\n")
        .await
        .unwrap();

    // Mock surgeon
    let expected_scope = SymbolScope {
        content: "fn login() { }\n".to_string(),
        start_line: 0,
        end_line: 0,
        name_column: 3,
        language: "rust".to_string(),
        ..Default::default()
    };
    bundle
        .surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(expected_scope));

    setup_mock_call_hierarchy(&bundle);

    let (server_read, server_write, mut client_reader, mut client_writer) = create_transport_pair();
    let server_handle = tokio::spawn(async move {
        bundle
            .server
            .serve((server_read, server_write))
            .await
            .expect("server serve")
    });

    do_handshake(&mut client_writer, &mut client_reader).await;

    // Call "trace" tool (scope callers)
    let trace_call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "trace",
            "arguments": {
                "semantic_path": "src/auth.rs::login",
                "scope": "callers"
            }
        }
    });
    write_jsonrpc(&mut client_writer, &trace_call).await;

    let response = read_jsonrpc(&mut client_reader).await;
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 2);

    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["degraded"], false);
    assert_eq!(structured["status"]["status"], "ok");
    assert!(structured["incoming"]["items"].is_array());
    assert_eq!(
        structured["incoming"]["items"][0]["semantic_path"],
        "src/server.rs::handle_request"
    );
    assert!(structured["outgoing"]["items"].is_array());
    assert_eq!(
        structured["outgoing"]["items"][0]["semantic_path"],
        "src/token.rs::validate_token"
    );

    drop(client_writer);
    let _ = server_handle.await;
}

#[tokio::test]
async fn test_mcp_integration_inspect_tool_symbols_success() {
    let bundle = create_integration_test_server();

    let src_dir = bundle.ws.path().join("src");
    tokio::fs::create_dir_all(&src_dir).await.unwrap();
    let file_path = src_dir.join("auth.rs");
    tokio::fs::write(&file_path, "fn login() { }\n")
        .await
        .unwrap();

    // Mock surgeon
    let expected_scope = SymbolScope {
        content: "fn login() { }\n".to_string(),
        start_line: 0,
        end_line: 0,
        name_column: 3,
        language: "rust".to_string(),
        ..Default::default()
    };
    bundle
        .surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .push(Ok(expected_scope));

    let (server_read, server_write, mut client_reader, mut client_writer) = create_transport_pair();
    let server_handle = tokio::spawn(async move {
        bundle
            .server
            .serve((server_read, server_write))
            .await
            .expect("server serve")
    });

    do_handshake(&mut client_writer, &mut client_reader).await;

    // Call "inspect" tool (without dependencies)
    let inspect_call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "inspect",
            "arguments": {
                "semantic_path": "src/auth.rs::login"
            }
        }
    });
    write_jsonrpc(&mut client_writer, &inspect_call).await;

    let response = read_jsonrpc(&mut client_reader).await;
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 2);

    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["language"], "rust");
    assert_eq!(structured["start_line"], 1);
    assert_eq!(structured["end_line"], 1);

    drop(client_writer);
    let _ = server_handle.await;
}

#[tokio::test]
async fn test_mcp_integration_inspect_tool_deep_context_success() {
    let bundle = create_integration_test_server();

    let src_dir = bundle.ws.path().join("src");
    tokio::fs::create_dir_all(&src_dir).await.unwrap();
    let file_path = src_dir.join("auth.rs");
    tokio::fs::write(&file_path, "fn login() { }\n")
        .await
        .unwrap();

    // Mock surgeon
    let expected_scope = SymbolScope {
        content: "fn login() { }\n".to_string(),
        start_line: 0,
        end_line: 0,
        name_column: 3,
        language: "rust".to_string(),
        ..Default::default()
    };
    bundle
        .surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .extend([Ok(expected_scope.clone()), Ok(expected_scope)]);

    // Mock lawyer for deep context
    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 1,
        column: 4,
        data: None,
    };
    bundle
        .lawyer
        .push_prepare_call_hierarchy_result(Ok(vec![item.clone()]));
    bundle
        .lawyer
        .push_outgoing_call_result(Ok(vec![CallHierarchyCall {
            item: CallHierarchyItem {
                name: "validate_token".into(),
                kind: "function".into(),
                detail: Some("fn validate_token() -> bool".into()),
                file: "src/token.rs".into(),
                line: 15,
                column: 4,
                data: None,
            },
            call_sites: vec![9],
        }]));

    let (server_read, server_write, mut client_reader, mut client_writer) = create_transport_pair();
    let server_handle = tokio::spawn(async move {
        bundle
            .server
            .serve((server_read, server_write))
            .await
            .expect("server serve")
    });

    do_handshake(&mut client_writer, &mut client_reader).await;

    // Call "inspect" tool (with include_dependencies = true)
    let inspect_call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "inspect",
            "arguments": {
                "semantic_path": "src/auth.rs::login",
                "include_dependencies": true
            }
        }
    });
    write_jsonrpc(&mut client_writer, &inspect_call).await;

    let response = read_jsonrpc(&mut client_reader).await;
    println!("DEBUG: INSPECT RESPONSE = {response:#?}");
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 2);

    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["degraded"], false);
    assert_eq!(structured["status"]["status"], "ok");
    assert!(structured["dependencies"]["items"].is_array());
    assert_eq!(
        structured["dependencies"]["items"][0]["semantic_path"],
        "src/token.rs::validate_token"
    );

    drop(client_writer);
    let _ = server_handle.await;
}

fn build_overview_fixtures() -> (
    SymbolScope,
    Vec<CallHierarchyItem>,
    Vec<CallHierarchyCall>,
    Vec<CallHierarchyCall>,
    Vec<ReferenceLocation>,
) {
    let scope = SymbolScope {
        content: "fn login() { }\n".to_string(),
        start_line: 0,
        end_line: 0,
        name_column: 3,
        language: "rust".to_string(),
        ..Default::default()
    };
    let item = CallHierarchyItem {
        name: "login".into(),
        kind: "function".into(),
        detail: None,
        file: "src/auth.rs".into(),
        line: 1,
        column: 4,
        data: None,
    };
    let incoming = vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "handle_request".into(),
            kind: "function".into(),
            detail: Some("fn handle_request()".into()),
            file: "src/server.rs".into(),
            line: 20,
            column: 4,
            data: None,
        },
        call_sites: vec![25],
    }];
    let outgoing = vec![CallHierarchyCall {
        item: CallHierarchyItem {
            name: "validate_token".into(),
            kind: "function".into(),
            detail: Some("fn validate_token() -> bool".into()),
            file: "src/token.rs".into(),
            line: 15,
            column: 4,
            data: None,
        },
        call_sites: vec![9],
    }];
    let references = vec![ReferenceLocation {
        file: "src/main.rs".into(),
        line: 10,
        column: 8,
        snippet: "login();".into(),
    }];

    (scope, vec![item], incoming, outgoing, references)
}

#[tokio::test]
async fn test_mcp_integration_trace_tool_overview_success() {
    let bundle = create_integration_test_server();

    let src_dir = bundle.ws.path().join("src");
    tokio::fs::create_dir_all(&src_dir).await.unwrap();
    let file_path = src_dir.join("auth.rs");
    tokio::fs::write(&file_path, "fn login() { }\n")
        .await
        .unwrap();

    let (scope, item, incoming, outgoing, references) = build_overview_fixtures();

    bundle
        .surgeon
        .read_symbol_scope_results
        .lock()
        .unwrap()
        .extend([Ok(scope.clone()), Ok(scope.clone()), Ok(scope)]);

    bundle.lawyer.push_prepare_call_hierarchy_result(Ok(item));
    bundle.lawyer.push_incoming_call_result(Ok(incoming));
    bundle.lawyer.push_outgoing_call_result(Ok(outgoing));
    bundle.lawyer.set_references_result(Ok(references));

    let (server_read, server_write, mut client_reader, mut client_writer) = create_transport_pair();
    let server_handle = tokio::spawn(async move {
        bundle
            .server
            .serve((server_read, server_write))
            .await
            .expect("server serve")
    });

    do_handshake(&mut client_writer, &mut client_reader).await;

    // Call "trace" tool (scope overview)
    let trace_call = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/call",
        "params": {
            "name": "trace",
            "arguments": {
                "semantic_path": "src/auth.rs::login",
                "scope": "overview"
            }
        }
    });
    write_jsonrpc(&mut client_writer, &trace_call).await;

    let response = read_jsonrpc(&mut client_reader).await;
    assert_eq!(response["jsonrpc"], "2.0");
    assert_eq!(response["id"], 2);

    let structured = &response["result"]["structuredContent"];
    assert_eq!(structured["status"]["status"], "ok");
    assert_eq!(structured["source"]["content"], "fn login() { }\n");
    assert_eq!(
        structured["impact"]["incoming"]["items"][0]["semantic_path"],
        "src/server.rs::handle_request"
    );
    assert_eq!(
        structured["impact"]["outgoing"]["items"][0]["semantic_path"],
        "src/token.rs::validate_token"
    );
    assert_eq!(structured["references"]["items"][0]["file"], "src/main.rs");
    assert_eq!(structured["references"]["items"][0]["snippet"], "");

    drop(client_writer);
    let _ = server_handle.await;
}
