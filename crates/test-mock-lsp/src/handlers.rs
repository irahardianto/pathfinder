//! LSP method handlers with canned responses.
//!
//! # Minimal scope (intentional)
//!
//! This module intentionally implements only the LSP methods exercised by
//! `LspClient`'s integration tests. The design follows the "start minimal,
//! grow organically" principle (Phase 3C of the test infrastructure plan).
//!
//! Future agents: to add a new LSP method to integration tests, add its handler
//! here and invoke it from `handle_message` in `main.rs`. Keep each handler
//! as a standalone `fn` that takes `params: Option<Value>` and returns `Value`
//! so it can be unit-tested independently if needed.

use crate::config::MockConfig;
use serde_json::{json, Value};

/// Handle an `initialize` request.
///
/// Returns a `ServerCapabilities` object driven by [`MockConfig`].
/// The capabilities advertised here directly affect `LspClient::capability_status()`
/// and `ProcessEntry::to_validation_status()` via `DetectedCapabilities`.
pub fn handle_initialize(_params: Option<Value>, config: &MockConfig) -> Value {
    json!({
        "capabilities": {
            // textDocument/definition — governs get_definition / read_with_deep_context
            "definitionProvider": config.definition_provider,
            // textDocument/diagnostic (LSP 3.17 pull model) — governs validation
            "diagnosticProvider": if config.diagnostic_provider {
                json!({ "interFileDependencies": false, "workspaceDiagnostics": config.workspace_diagnostic_provider })
            } else {
                json!(false)
            },
            // callHierarchy — governs analyze_impact
            "callHierarchyProvider": config.call_hierarchy_provider,
            // textDocument/formatting
            "documentFormattingProvider": config.formatting_provider,
        },
        "serverInfo": {
            "name": "test-mock-lsp",
            "version": "0.1.0"
        }
    })
}

/// Handle a `textDocument/definition` request.
///
/// Returns a canned Location response. Pass `None` params to get a null
/// response (symbol not found). The mock always returns the same location
/// unless `MockConfig::definition_returns_null` is true.
pub fn handle_definition(_params: Option<Value>, config: &MockConfig) -> Value {
    if config.definition_returns_null {
        return json!(null);
    }
    json!({
        "uri": "file:///mock/workspace/src/main.rs",
        "range": {
            "start": { "line": 0, "character": 0 },
            "end":   { "line": 0, "character": 4 }
        }
    })
}

/// Handle a `textDocument/diagnostic` request.
///
/// Returns a canned `DocumentDiagnosticReport`. If `MockConfig::diagnostic_items`
/// is non-empty, those are returned as `full` results. Otherwise returns an
/// empty `full` report (no errors).
pub fn handle_pull_diagnostics(_params: Option<Value>, config: &MockConfig) -> Value {
    json!({
        "kind": "full",
        "items": config.diagnostic_items
    })
}

/// Handle a `workspace/diagnostic` request.
pub fn handle_workspace_diagnostics(_params: Option<Value>, _config: &MockConfig) -> Value {
    json!({ "items": [] })
}

/// Handle a `callHierarchy/prepareCallHierarchy` request.
pub fn handle_call_hierarchy_prepare(_params: Option<Value>, _config: &MockConfig) -> Value {
    json!(null)
}

/// Handle a `callHierarchy/incomingCalls` or `callHierarchy/outgoingCalls` request.
pub fn handle_call_hierarchy_calls(_params: Option<Value>, _config: &MockConfig) -> Value {
    json!([])
}

/// Handle a `textDocument/rangeFormatting` or `textDocument/formatting` request.
pub fn handle_formatting(_params: Option<Value>, _config: &MockConfig) -> Value {
    json!(null)
}

/// Handle a `shutdown` request — returns null per LSP spec.
pub fn handle_shutdown() -> Value {
    json!(null)
}
