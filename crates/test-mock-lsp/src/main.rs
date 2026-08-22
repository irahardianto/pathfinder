#![allow(
    clippy::pedantic,
    clippy::missing_errors_doc,
    clippy::cast_possible_truncation,
    clippy::doc_markdown,
    clippy::needless_pass_by_value,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::field_reassign_with_default
)]

//! `test-mock-lsp` — a scriptable mock LSP server for integration testing.
//!
//! # Purpose
//!
//! This binary acts as an LSP test fixture for `pathfinder-lsp` integration tests.
//! It speaks the real LSP wire protocol (JSON-RPC 2.0 over stdin/stdout with
//! `Content-Length` framing) and supports:
//!
//! - Scriptable response timelines (`--script`)
//! - Position-aware canned index (`--index`)
//! - Server-initiated messages (dynamic registrations, progress, config queries)
//! - Handlers for references, implementation, call hierarchy, definition, diagnostics
//! - Malformed response injection (`--malform`) for defensibility testing
//! - Position encoding negotiation (`--position-encoding`)

use std::io::{stdin, stdout, BufReader, BufWriter, Stdout};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use test_mock_lsp::config::{self, MockConfig};
use test_mock_lsp::emitter::MessageEmitter;
use test_mock_lsp::protocol::{
    read_message, write_malformed_response, write_response, JsonRpcResponse, MalformMode,
};
use test_mock_lsp::script::ScriptAction;

fn main() {
    let config = config::parse_args();
    run(config);
}

fn run(mut config: MockConfig) {
    let stdin = stdin();
    let stdout_handle = stdout();
    let mut reader = BufReader::new(stdin.lock());
    let writer: Arc<Mutex<BufWriter<Stdout>>> = Arc::new(Mutex::new(BufWriter::new(stdout_handle)));

    // Launch server-initiated emitter if configured
    let emitter = MessageEmitter::new(config.clone());
    emitter.start_background_emitter(Arc::clone(&writer));

    let mut request_count: usize = 0;
    let mut shutdown_received = false;

    loop {
        let msg = match read_message(&mut reader) {
            Ok(Some(m)) => m,
            Ok(None) => {
                // Clean EOF — client closed the pipe.
                break;
            }
            Err(e) => {
                eprintln!("test-mock-lsp: read error: {e}");
                break;
            }
        };

        // Count requests for crash-after fault injection.
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
                // Response from client to our server-initiated request (e.g. create progress / registerCapability).
                continue;
            }
        };

        // Capture workspace root from initialize params if not explicitly configured
        if method == "initialize" && config.workspace_root.is_none() {
            if let Some(p) = &msg.params {
                if let Some(root_uri) = p.get("rootUri").and_then(|u| u.as_str()) {
                    let cleaned = root_uri.trim_start_matches("file://");
                    config.workspace_root = Some(cleaned.to_string());
                } else if let Some(root_path) = p.get("rootPath").and_then(|u| u.as_str()) {
                    config.workspace_root = Some(root_path.to_string());
                }
            }
        }

        // ── 1. Evaluate scripted timeline ──────────────────────────────────
        let mut handled_by_script = false;
        if let Some(script) = &mut config.script {
            if let Some(eval) = script.evaluate(&method) {
                handled_by_script = true;
                if let Some(delay) = eval.delay {
                    thread::sleep(delay);
                }

                match eval.action {
                    ScriptAction::Hang => {
                        // Sleep indefinitely until killed
                        thread::sleep(Duration::from_secs(3600));
                    }
                    ScriptAction::Crash => {
                        eprintln!("test-mock-lsp: crashing on method {method} via script");
                        std::process::exit(1);
                    }
                    ScriptAction::Error {
                        code,
                        message,
                        data,
                    } => {
                        let resp =
                            JsonRpcResponse::error_with_data(msg.id.clone(), code, message, data);
                        send_response(&writer, &resp, Some(&method), &config);
                    }
                    ScriptAction::Respond(val) => {
                        let resp = JsonRpcResponse::success(msg.id.clone(), val);
                        send_response(&writer, &resp, Some(&method), &config);
                    }
                    ScriptAction::Malformed { mode, payload } => {
                        let resp = JsonRpcResponse::success(msg.id.clone(), payload);
                        if let Ok(mut guard) = writer.lock() {
                            let _ =
                                write_malformed_response(&mut *guard, &resp, Some(&method), mode);
                        }
                    }
                }
            }
        }

        if handled_by_script {
            if shutdown_received && method == "exit" {
                break;
            }
            if method == "shutdown" {
                shutdown_received = true;
            }
            continue;
        }

        // ── 2. Standard Handler Dispatch ───────────────────────────────────
        let response_opt = handle_message(&method, msg.id.clone(), msg.params, &config);

        if let Some(resp) = response_opt {
            send_response(&writer, &resp, Some(&method), &config);
        } else if method == "exit" {
            break;
        }

        if shutdown_received && method == "exit" {
            break;
        }
        if method == "shutdown" {
            shutdown_received = true;
        }
    }
}

/// Send a response, applying framing malformation if configured.
fn send_response<W: std::io::Write>(
    writer: &Arc<Mutex<W>>,
    response: &JsonRpcResponse,
    method: Option<&str>,
    config: &MockConfig,
) {
    let should_malform = config.malform != MalformMode::None
        && config
            .malform_method
            .as_deref()
            .is_none_or(|m| method == Some(m));

    if let Ok(mut guard) = writer.lock() {
        let res = if should_malform {
            write_malformed_response(&mut *guard, response, method, config.malform)
        } else {
            write_response(&mut *guard, response)
        };
        if let Err(e) = res {
            eprintln!("test-mock-lsp: write error: {e}");
        }
    }
}

/// Route an incoming LSP message to its default handler.
fn handle_message(
    method: &str,
    id: Option<serde_json::Value>,
    params: Option<serde_json::Value>,
    config: &MockConfig,
) -> Option<JsonRpcResponse> {
    match method {
        // ── Lifecycle ─────────────────────────────────────────────────────
        "initialize" => {
            if config.init_delay_ms > 0 {
                thread::sleep(Duration::from_millis(config.init_delay_ms));
            }
            Some(JsonRpcResponse::success(
                id,
                test_mock_lsp::handlers::handle_initialize(params, config),
            ))
        }
        "initialized"
        | "exit"
        | "textDocument/didOpen"
        | "textDocument/didChange"
        | "textDocument/didClose"
        | "workspace/didChangeWatchedFiles" => None,
        "shutdown" => Some(JsonRpcResponse::success(
            id,
            test_mock_lsp::handlers::handle_shutdown(),
        )),

        // ── Navigation ────────────────────────────────────────────────────
        "textDocument/definition" => Some(JsonRpcResponse::success(
            id,
            test_mock_lsp::handlers::handle_definition(params, config),
        )),
        "textDocument/references" => Some(JsonRpcResponse::success(
            id,
            test_mock_lsp::handlers::handle_references(params, config),
        )),
        "textDocument/implementation" => Some(JsonRpcResponse::success(
            id,
            test_mock_lsp::handlers::handle_implementation(params, config),
        )),

        // ── Diagnostics ───────────────────────────────────────────────────
        "textDocument/diagnostic" => Some(JsonRpcResponse::success(
            id,
            test_mock_lsp::handlers::handle_pull_diagnostics(params, config),
        )),
        "workspace/diagnostic" => Some(JsonRpcResponse::success(
            id,
            test_mock_lsp::handlers::handle_workspace_diagnostics(params, config),
        )),

        // ── Call hierarchy ────────────────────────────────────────────────
        "textDocument/prepareCallHierarchy" => Some(JsonRpcResponse::success(
            id,
            test_mock_lsp::handlers::handle_call_hierarchy_prepare(params, config),
        )),
        "callHierarchy/incomingCalls" => Some(JsonRpcResponse::success(
            id,
            test_mock_lsp::handlers::handle_call_hierarchy_incoming(params, config),
        )),
        "callHierarchy/outgoingCalls" => Some(JsonRpcResponse::success(
            id,
            test_mock_lsp::handlers::handle_call_hierarchy_outgoing(params, config),
        )),

        // ── Formatting ────────────────────────────────────────────────────
        "textDocument/rangeFormatting" | "textDocument/formatting" => {
            Some(JsonRpcResponse::success(
                id,
                test_mock_lsp::handlers::handle_formatting(params, config),
            ))
        }

        // ── Unknown methods ───────────────────────────────────────────────
        _ => {
            eprintln!("test-mock-lsp: unhandled method {method:?}");
            id.map(|req_id| {
                JsonRpcResponse::error(Some(req_id), -32601, format!("method not found: {method}"))
            })
        }
    }
}
