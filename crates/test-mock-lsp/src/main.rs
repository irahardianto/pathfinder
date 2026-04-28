//! `test-mock-lsp` — a lightweight mock LSP server for integration testing.
//!
//! # Purpose
//!
//! This binary is the testcontainers-equivalent for LSP integration tests.
//! Just as testcontainers spins up a real `PostgreSQL` instance for database
//! integration tests, integration tests in `crates/pathfinder-lsp/tests/` spin
//! up this binary to give `LspClient` a real JSON-RPC counterpart to talk to.
//!
//! It speaks the real LSP wire protocol (JSON-RPC over stdin/stdout with
//! `Content-Length` framing) so `LspClient` exercises the exact same code paths
//! as production — including `spawn_and_initialize`, `request`, the reader
//! supervisor task, and the progress watcher.
//!
//! # Scope (minimal by design)
//!
//! The server implements only the LSP methods currently exercised by integration
//! tests. Future agents: add new method handlers in `handlers.rs` and route them
//! in `handle_message` below. DO NOT expand scope speculatively — add handlers
//! only when a corresponding integration test exists.
//!
//! # Configuration
//!
//! All behavior is driven by CLI flags (see `config.rs`). Tests pass flags when
//! spawning the binary to exercise specific `LspClient` code paths:
//!
//!   --no-diagnostic-provider     Test `validation_skipped` paths
//!   --crash-after=N              Test crash-recovery / reconnect logic
//!   --init-delay-ms=N            Test initialization timeout handling
//!
//! # Protocol coverage
//!
//! Current minimal set (grow as integration tests expand):
//!   initialize            ✓  (capability negotiation)
//!   initialized           ✓  (notification, no response)
//!   shutdown              ✓  (graceful exit)
//!   exit                  ✓  (process termination)
//!   textDocument/didOpen  ✓  (notification, no response)
//!   textDocument/didChange ✓ (notification, no response)
//!   textDocument/didClose ✓  (notification, no response)
//!   textDocument/definition ✓ (canned Location or null)
//!   textDocument/diagnostic ✓ (canned error list)
//!   workspace/diagnostic   ✓ (empty list)
//!   callHierarchy/*        ✓ (empty/null stubs)

mod config;
mod handlers;
mod protocol;

use config::MockConfig;
use protocol::{read_message, write_response, JsonRpcResponse};

use std::io::{stdin, stdout, BufReader, BufWriter};
use std::thread;
use std::time::Duration;

fn main() {
    let config = config::parse_args();
    run(&config);
}

fn run(config: &MockConfig) {
    let stdin = stdin();
    let stdout = stdout();
    let mut reader = BufReader::new(stdin.lock());
    let mut writer = BufWriter::new(stdout.lock());

    let mut request_count: usize = 0;
    let mut shutdown_received = false;

    loop {
        let msg = match read_message(&mut reader) {
            Ok(Some(m)) => m,
            Ok(None) => {
                // Clean EOF — client closed the pipe (normal after shutdown/exit).
                break;
            }
            Err(e) => {
                eprintln!("test-mock-lsp: read error: {e}");
                break;
            }
        };

        // Count requests for crash-after fault injection.
        // Only increment for messages that have an `id` (requests, not notifications).
        if msg.id.is_some() {
            request_count += 1;
            if config.crash_after > 0 && request_count >= config.crash_after {
                eprintln!("test-mock-lsp: crashing after {request_count} requests (--crash-after)");
                std::process::exit(1);
            }
        }

        let method = match &msg.method {
            Some(m) => m.clone(),
            None => {
                // Response to our own requests — we never send requests, so ignore.
                continue;
            }
        };

        match handle_message(&method, msg.id.clone(), msg.params, config, &mut writer) {
            Some(response) => {
                if let Err(e) = write_response(&mut writer, &response) {
                    eprintln!("test-mock-lsp: write error: {e}");
                    break;
                }
            }
            None if method == "exit" => {
                // `exit` is a notification; process exits immediately after receiving it.
                break;
            }
            None => {
                // Notification with no response (didOpen, didChange, initialized, etc.).
            }
        }

        if shutdown_received && method == "exit" {
            break;
        }
        if method == "shutdown" {
            shutdown_received = true;
        }
    }
}

/// Route an incoming LSP message to its handler.
///
/// Returns `Some(response)` for requests, `None` for notifications.
///
/// Future agents: add new method routes here when adding integration test
/// coverage for additional `Lawyer` trait methods. Keep the match arm ordering
/// consistent with the `Lawyer` trait definition in `pathfinder-lsp/src/lib.rs`.
fn handle_message(
    method: &str,
    id: Option<serde_json::Value>,
    params: Option<serde_json::Value>,
    config: &MockConfig,
    _writer: &mut impl std::io::Write,
) -> Option<JsonRpcResponse> {
    match method {
        // ── Lifecycle ─────────────────────────────────────────────────────
        "initialize" => {
            // Optional delay to test initialization timeout handling.
            if config.init_delay_ms > 0 {
                thread::sleep(Duration::from_millis(config.init_delay_ms));
            }
            Some(JsonRpcResponse::success(
                id,
                handlers::handle_initialize(params, config),
            ))
        }
        // Notifications — no response returned
        "initialized"
        | "exit"
        | "textDocument/didOpen"
        | "textDocument/didChange"
        | "textDocument/didClose"
        | "workspace/didChangeWatchedFiles" => None,
        "shutdown" => Some(JsonRpcResponse::success(id, handlers::handle_shutdown())),

        // ── Navigation ────────────────────────────────────────────────────
        "textDocument/definition" => Some(JsonRpcResponse::success(
            id,
            handlers::handle_definition(params, config),
        )),

        // ── Diagnostics ───────────────────────────────────────────────────
        "textDocument/diagnostic" => Some(JsonRpcResponse::success(
            id,
            handlers::handle_pull_diagnostics(params, config),
        )),
        "workspace/diagnostic" => Some(JsonRpcResponse::success(
            id,
            handlers::handle_workspace_diagnostics(params, config),
        )),

        // ── Call hierarchy ────────────────────────────────────────────────
        "textDocument/prepareCallHierarchy" => Some(JsonRpcResponse::success(
            id,
            handlers::handle_call_hierarchy_prepare(params, config),
        )),
        "callHierarchy/incomingCalls" | "callHierarchy/outgoingCalls" => Some(
            JsonRpcResponse::success(id, handlers::handle_call_hierarchy_calls(params, config)),
        ),

        // ── Formatting ────────────────────────────────────────────────────
        "textDocument/rangeFormatting" | "textDocument/formatting" => Some(
            JsonRpcResponse::success(id, handlers::handle_formatting(params, config)),
        ),

        // ── Unknown methods ───────────────────────────────────────────────
        _ => {
            eprintln!("test-mock-lsp: unhandled method {method:?}");
            // Return method-not-found error only for requests (id is Some).
            id.map(|req_id| {
                JsonRpcResponse::error(Some(req_id), -32601, format!("method not found: {method}"))
            })
        }
    }
}
