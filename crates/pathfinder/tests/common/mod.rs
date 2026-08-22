//! Shared test utilities for pathfinder integration tests.
//!
//! Provides helpers for creating mock MCP servers and simulating
//! JSON-RPC message exchanges over in-memory transport.

use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::WorkspaceRoot;
use pathfinder_lib::server::PathfinderServer;
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;

use std::sync::Arc;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream};

/// Bundle holding a test server alongside its temp directory.
///
/// The `TempDir` must live as long as the server — dropping it
/// invalidates the workspace path.
pub struct TestServerBundle {
    pub server: PathfinderServer,
    pub _temp_dir: TempDir,
}

/// Create a `PathfinderServer` with mock engines (no real LSP/ripgrep).
///
/// Returns the server and the `TempDir` that backs the workspace root.
/// Caller must keep `TempDir` alive for the server's lifetime.
pub fn create_test_server() -> TestServerBundle {
    let temp_dir = tempfile::tempdir().expect("failed to create temp dir");
    let ws = WorkspaceRoot::new(temp_dir.path()).expect("valid workspace root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);

    let server = PathfinderServer::with_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        Arc::new(MockSurgeon::new()),
    );

    TestServerBundle {
        server,
        _temp_dir: temp_dir,
    }
}

/// Create paired in-memory transport streams for MCP client/server testing.
///
/// Returns `(server_read, server_write, client_reader, client_writer)` where:
/// - `(server_read, server_write)` is passed to `server.serve()`
/// - `client_writer` sends JSON-RPC messages to the server
/// - `client_reader` reads JSON-RPC responses from the server
pub fn create_transport_pair() -> (
    DuplexStream,
    DuplexStream,
    BufReader<DuplexStream>,
    DuplexStream,
) {
    // Server reads from server_read, client writes to client_write_end
    let (server_read, client_write) = tokio::io::duplex(8192);
    // Server writes to server_write, client reads from client_read_end
    let (client_read, server_write) = tokio::io::duplex(8192);

    let client_reader = BufReader::new(client_read);

    (server_read, server_write, client_reader, client_write)
}

/// Write a JSON-RPC message (as raw string) followed by a newline.
///
/// MCP over stdio uses newline-delimited JSON — each message
/// is a single JSON object on one line terminated by `\n`.
pub async fn write_jsonrpc(writer: &mut DuplexStream, msg: &str) {
    writer
        .write_all(msg.as_bytes())
        .await
        .expect("write message");
    writer.write_all(b"\n").await.expect("write newline");
    writer.flush().await.expect("flush");
}

/// Read a single JSON-RPC response line and parse it as `serde_json::Value`.
///
/// Blocks until a full newline-delimited line is available.
/// Panics if the line is not valid JSON (test assertion).
pub async fn read_jsonrpc(reader: &mut BufReader<DuplexStream>) -> serde_json::Value {
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .await
        .expect("read response line");
    assert!(!line.is_empty(), "expected a response but got EOF");
    serde_json::from_str(&line).unwrap_or_else(|e| {
        panic!("invalid JSON in response: {e}\nraw: {line}");
    })
}

// ── MCP message builders ────────────────────────────────────────────

/// Build a standard MCP `initialize` request (JSON string).
pub fn initialize_request(id: u64) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-03-26",
            "capabilities": {},
            "clientInfo": {
                "name": "test-client",
                "version": "1.0.0"
            }
        }
    })
    .to_string()
}

/// Build an MCP `notifications/initialized` notification (JSON string).
pub fn initialized_notification() -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "notifications/initialized"
    })
    .to_string()
}

/// Build an MCP `tools/list` request (JSON string).
pub fn tools_list_request(id: u64) -> String {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "method": "tools/list",
        "params": {}
    })
    .to_string()
}

// ── Non-ASCII and Fixture Workspace Helpers (`LSP-A5`, `TS-A2`) ───────

/// Create a test workspace with non-ASCII multi-byte UTF-8 source files.
///
/// Contains characters where byte offsets, UTF-16 code units, and unicode scalars diverge:
/// - Multi-byte UTF-8 letters (e.g. `é`, `ñ`, `ä` — 2 bytes, 1 UTF-16 unit)
/// - CJK ideographs (e.g. `中`, `文` — 3 bytes, 1 UTF-16 unit)
/// - Astral plane emoji (e.g. `🦀`, `🚀` — 4 bytes, 2 UTF-16 surrogate units)
#[allow(dead_code)]
pub fn make_non_ascii_workspace() -> TempDir {
    let dir = tempfile::tempdir().expect("failed to create tempdir");
    let root = dir.path();

    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"non-ascii-workspace\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("failed to write Cargo.toml");

    let src = root.join("src");
    std::fs::create_dir_all(&src).expect("create src dir");

    // Line 1 contains 4-byte emoji 🦀 before fn declaration to test position encoding offsets
    let source_content = "// 🦀 Non-ASCII test fixture with UTF-8 / UTF-16 divergence\n\
                          // 中文注释 and accented café\n\
                          fn calculate_score() -> u32 {\n\
                              42\n\
                          }\n";

    std::fs::write(src.join("lib.rs"), source_content).expect("write lib.rs");

    dir
}
