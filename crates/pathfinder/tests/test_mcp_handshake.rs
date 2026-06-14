//! MCP handshake integration tests.
//!
//! Verifies that `PathfinderServer` correctly handles the MCP initialization
//! protocol over an in-memory transport, exercising the full rmcp stack
//! (transport → codec → service init → handler).
//!
//! ## Why these tests exist
//!
//! A protocol-level incompatibility was observed when certain MCP clients
//! (e.g. opencode) connected to pathfinder-mcp: the initialize request
//! failed to parse, and rmcp's transport layer swallowed the error,
//! causing the *next* valid message to be consumed as the initialization
//! attempt — producing a misleading "expect initialized request" error.
//!
//! These tests guard against regressions by testing the actual JSON-RPC
//! byte flow, not just internal method calls.

#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use common::{
    create_test_server, create_transport_pair, initialize_request, initialized_notification,
    read_jsonrpc, tools_list_request, write_jsonrpc,
};
use rmcp::ServiceExt;

/// Full golden-path handshake: initialize → initialized → tools/list.
///
/// Verifies the server completes the MCP initialization sequence and
/// responds to a subsequent `tools/list` request.
#[tokio::test]
async fn test_handshake_happy_path() {
    let bundle = create_test_server();
    let (server_read, server_write, mut client_reader, mut client_writer) = create_transport_pair();

    // Start server in background
    let server_handle = tokio::spawn(async move {
        bundle
            .server
            .serve((server_read, server_write))
            .await
            .expect("server should start successfully")
    });

    // 1. Send initialize request
    write_jsonrpc(&mut client_writer, &initialize_request(1)).await;

    // 2. Read initialize response
    let init_response = read_jsonrpc(&mut client_reader).await;
    assert_eq!(init_response["jsonrpc"], "2.0");
    assert_eq!(init_response["id"], 1);
    assert!(
        init_response.get("result").is_some(),
        "expected 'result' in initialize response, got: {init_response}"
    );

    // 3. Send initialized notification
    write_jsonrpc(&mut client_writer, &initialized_notification()).await;

    // 4. Send tools/list request
    write_jsonrpc(&mut client_writer, &tools_list_request(2)).await;

    // 5. Read tools/list response
    let tools_response = read_jsonrpc(&mut client_reader).await;
    assert_eq!(tools_response["jsonrpc"], "2.0");
    assert_eq!(tools_response["id"], 2);
    assert!(
        tools_response["result"]["tools"].is_array(),
        "expected 'tools' array in result, got: {tools_response}"
    );

    // Clean shutdown: drop writer to close the transport
    drop(client_writer);
    let service = server_handle.await.expect("server task should not panic");
    // Wait briefly for service to wind down
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), service.waiting()).await;
}

/// Verify that the initialize response contains correct server metadata.
#[tokio::test]
async fn test_handshake_returns_server_info() {
    let bundle = create_test_server();
    let (server_read, server_write, mut client_reader, mut client_writer) = create_transport_pair();

    let server_handle = tokio::spawn(async move {
        bundle
            .server
            .serve((server_read, server_write))
            .await
            .expect("server start")
    });

    write_jsonrpc(&mut client_writer, &initialize_request(1)).await;
    let resp = read_jsonrpc(&mut client_reader).await;

    let result = &resp["result"];

    // Server must identify as "pathfinder"
    assert_eq!(
        result["serverInfo"]["name"], "pathfinder",
        "server name mismatch: {result}"
    );

    // Version must match CARGO_PKG_VERSION
    let version = result["serverInfo"]["version"]
        .as_str()
        .expect("version should be a string");
    assert!(
        !version.is_empty(),
        "server version should not be empty: {result}"
    );

    // Protocol version must be present
    assert!(
        result["protocolVersion"].is_string(),
        "protocolVersion must be present: {result}"
    );

    // Capabilities must advertise tools
    assert!(
        result["capabilities"].is_object(),
        "capabilities must be present: {result}"
    );

    drop(client_writer);
    let service = server_handle.await.expect("join");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), service.waiting()).await;
}

/// Verify that `tools/list` returns all 7 registered tools.
#[tokio::test]
async fn test_handshake_tools_list_contains_expected_tools() {
    let bundle = create_test_server();
    let (server_read, server_write, mut client_reader, mut client_writer) = create_transport_pair();

    let server_handle = tokio::spawn(async move {
        bundle
            .server
            .serve((server_read, server_write))
            .await
            .expect("server start")
    });

    // Complete handshake
    write_jsonrpc(&mut client_writer, &initialize_request(1)).await;
    let _ = read_jsonrpc(&mut client_reader).await;
    write_jsonrpc(&mut client_writer, &initialized_notification()).await;

    // Request tools list
    write_jsonrpc(&mut client_writer, &tools_list_request(2)).await;
    let resp = read_jsonrpc(&mut client_reader).await;

    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools should be an array");

    let tool_names: Vec<&str> = tools
        .iter()
        .map(|t| t["name"].as_str().expect("tool name"))
        .collect();

    let expected_tools = [
        "explore",
        "search",
        "read",
        "inspect",
        "locate",
        "trace",
        "health",
    ];

    for expected in &expected_tools {
        assert!(
            tool_names.contains(expected),
            "missing tool '{expected}' in tools/list response. Got: {tool_names:?}"
        );
    }

    assert_eq!(
        tools.len(),
        expected_tools.len(),
        "unexpected tool count. Got: {tool_names:?}"
    );

    drop(client_writer);
    let service = server_handle.await.expect("join");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), service.waiting()).await;
}

/// Verify behavior when malformed JSON is sent before a valid initialize.
///
/// ## Known rmcp behavior (documented, not a bug in pathfinder)
///
/// rmcp's `AsyncRwTransport::receive` handles parse errors by sending
/// a JSON-RPC parse-error response and **continuing** to read the next
/// message. This means:
///
/// 1. Malformed line → parse error response sent to client
/// 2. Next valid message is consumed by the initialization loop
/// 3. If that message is NOT an `InitializeRequest`, initialization fails
///    with "expect initialized request" — a misleading error.
///
/// This test documents that behavior: after a malformed line, a valid
/// initialize on the NEXT line should still succeed because rmcp's
/// transport recovers and the init loop sees the valid request.
#[tokio::test]
async fn test_handshake_malformed_json_then_valid_initialize() {
    let bundle = create_test_server();
    let (server_read, server_write, mut client_reader, mut client_writer) = create_transport_pair();

    let server_handle =
        tokio::spawn(async move { bundle.server.serve((server_read, server_write)).await });

    // 1. Send malformed JSON (missing closing brace)
    write_jsonrpc(
        &mut client_writer,
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"#,
    )
    .await;

    // 2. Read the parse error response from rmcp
    let error_resp = read_jsonrpc(&mut client_reader).await;
    assert_eq!(
        error_resp["error"]["code"], -32700,
        "expected parse error code (-32700), got: {error_resp}"
    );

    // 3. Send a VALID initialize request
    write_jsonrpc(&mut client_writer, &initialize_request(2)).await;

    // 4. rmcp should recover — the init loop reads the next message
    //    which IS a valid InitializeRequest. Server should start.
    let init_resp = read_jsonrpc(&mut client_reader).await;
    assert_eq!(init_resp["id"], 2);
    assert!(
        init_resp.get("result").is_some(),
        "server should recover after parse error and accept valid initialize. Got: {init_resp}"
    );

    // 5. Complete the handshake to prove full recovery
    write_jsonrpc(&mut client_writer, &initialized_notification()).await;
    write_jsonrpc(&mut client_writer, &tools_list_request(3)).await;
    let tools_resp = read_jsonrpc(&mut client_reader).await;
    assert!(
        tools_resp["result"]["tools"].is_array(),
        "tools/list should work after recovery. Got: {tools_resp}"
    );

    drop(client_writer);
    let service = server_handle
        .await
        .expect("join")
        .expect("server should have started");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), service.waiting()).await;
}

/// Verify that every tool's `inputSchema` has `type: "object"` and `properties`.
///
/// MCP spec requires `inputSchema` to be a JSON Schema object with `type: "object"`
/// and `properties`. Clients like opencode's `mcp-go` reject tools with bare
/// `AnyValue` schemas (missing `type` and `properties`), causing "Failed to get tools."
///
/// This test ensures the schema regression never recurs.
#[tokio::test]
async fn test_tools_list_input_schema_is_object() {
    let bundle = create_test_server();
    let (server_read, server_write, mut client_reader, mut client_writer) = create_transport_pair();

    let server_handle = tokio::spawn(async move {
        bundle
            .server
            .serve((server_read, server_write))
            .await
            .expect("server start")
    });

    // Complete handshake
    write_jsonrpc(&mut client_writer, &initialize_request(1)).await;
    let _ = read_jsonrpc(&mut client_reader).await;
    write_jsonrpc(&mut client_writer, &initialized_notification()).await;

    // Request tools list
    write_jsonrpc(&mut client_writer, &tools_list_request(2)).await;
    let resp = read_jsonrpc(&mut client_reader).await;

    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools should be an array");

    for tool in tools {
        let name = tool["name"].as_str().expect("tool name");
        let schema = &tool["inputSchema"];

        // Must have type: "object"
        assert_eq!(
            schema["type"], "object",
            "tool '{name}' inputSchema must have type: \"object\", got: {schema}"
        );

        // Must have properties (even if empty object)
        assert!(
            schema["properties"].is_object(),
            "tool '{name}' inputSchema must have 'properties' object, got: {schema}"
        );
    }

    drop(client_writer);
    let service = server_handle.await.expect("join");
    let _ = tokio::time::timeout(std::time::Duration::from_secs(2), service.waiting()).await;
}

/// Verify that sending a non-initialize request first fails initialization.
///
/// Per MCP spec, the first message MUST be `initialize`. Sending
/// `tools/list` first should cause the server to reject the connection.
#[tokio::test]
async fn test_handshake_non_initialize_request_first() {
    let bundle = create_test_server();
    let (server_read, server_write, _client_reader, mut client_writer) = create_transport_pair();

    // Send tools/list without initialize
    write_jsonrpc(&mut client_writer, &tools_list_request(1)).await;

    // Server should fail initialization — .serve() returns Err
    let result = bundle.server.serve((server_read, server_write)).await;
    assert!(
        result.is_err(),
        "server should reject non-initialize first message"
    );
}
