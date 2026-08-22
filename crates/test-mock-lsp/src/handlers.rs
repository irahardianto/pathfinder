//! LSP method handlers with canned responses and position-aware index support.
//!
//! Covers:
//! - Lifecycle (`initialize`, `shutdown`)
//! - Navigation (`definition`, `references`, `implementation`)
//! - Call hierarchy (`prepareCallHierarchy`, `incomingCalls`, `outgoingCalls`)
//! - Diagnostics (pull & workspace)
//! - Formatting

use crate::config::MockConfig;
use serde_json::{json, Value};

/// Handle an `initialize` request.
///
/// Returns a `ServerCapabilities` object driven by [`MockConfig`].
#[must_use]
pub fn handle_initialize(params: Option<Value>, config: &MockConfig) -> Value {
    let mut capabilities = json!({
        // textDocument/definition — governs get_definition / read_with_deep_context
        "definitionProvider": config.definition_provider,
        // textDocument/references (Task 2.4)
        "referencesProvider": config.references_provider,
        // textDocument/implementation (Task 2.4)
        "implementationProvider": config.implementation_provider,
        // textDocument/diagnostic (LSP 3.17 pull model)
        "diagnosticProvider": if config.diagnostic_provider {
            json!({ "interFileDependencies": false, "workspaceDiagnostics": config.workspace_diagnostic_provider })
        } else {
            json!(false)
        },
        // callHierarchy — governs find_callers_callees
        "callHierarchyProvider": config.call_hierarchy_provider,
        // textDocument/formatting
        "documentFormattingProvider": config.formatting_provider,
    });

    // Position encoding negotiation (Task 2.6 / LSP-A5)
    if let Some(enc) = &config.position_encoding {
        capabilities["positionEncoding"] = json!(enc);
    }

    let mut response = json!({
        "capabilities": capabilities,
        "serverInfo": {
            "name": "test-mock-lsp",
            "version": "0.1.0"
        }
    });

    // Optional echo or tracking if initialize params contained workspace info
    if let Some(p) = params {
        if let Some(root_uri) = p.get("rootUri").and_then(|u| u.as_str()) {
            response["_clientRootUri"] = json!(root_uri);
        }
    }

    response
}

/// Handle a `textDocument/definition` request.
#[must_use]
pub fn handle_definition(params: Option<Value>, config: &MockConfig) -> Value {
    if config.definition_returns_null {
        return json!(null);
    }

    if let (Some(index), Some(p)) = (&config.canned_index, &params) {
        if let Some((uri, line, char_col)) = extract_text_document_position(p) {
            if let Some(val) =
                index.lookup_definition(&uri, line, char_col, config.workspace_root.as_deref())
            {
                return val;
            }
        }
    }

    let default_uri = if let Some(ws) = &config.workspace_root {
        format!("file://{}/src/main.rs", ws.trim_end_matches('/'))
    } else {
        "file:///mock/workspace/src/main.rs".to_string()
    };

    json!({
        "uri": default_uri,
        "range": {
            "start": { "line": 0, "character": 0 },
            "end":   { "line": 0, "character": 4 }
        }
    })
}

/// Handle a `textDocument/references` request (Task 2.4).
#[must_use]
pub fn handle_references(params: Option<Value>, config: &MockConfig) -> Value {
    if let (Some(index), Some(p)) = (&config.canned_index, &params) {
        if let Some((uri, line, char_col)) = extract_text_document_position(p) {
            if let Some(val) =
                index.lookup_references(&uri, line, char_col, config.workspace_root.as_deref())
            {
                return val;
            }
        }
    }

    config.references_items.clone()
}

/// Handle a `textDocument/implementation` request (Task 2.4).
#[must_use]
pub fn handle_implementation(params: Option<Value>, config: &MockConfig) -> Value {
    if let (Some(index), Some(p)) = (&config.canned_index, &params) {
        if let Some((uri, line, char_col)) = extract_text_document_position(p) {
            if let Some(val) =
                index.lookup_implementation(&uri, line, char_col, config.workspace_root.as_deref())
            {
                return val;
            }
        }
    }

    config.implementation_items.clone()
}

/// Handle a `callHierarchy/prepareCallHierarchy` request (Task 2.2).
#[must_use]
pub fn handle_call_hierarchy_prepare(params: Option<Value>, config: &MockConfig) -> Value {
    if let (Some(index), Some(p)) = (&config.canned_index, &params) {
        if let Some((uri, line, char_col)) = extract_text_document_position(p) {
            if let Some(val) = index.lookup_prepare_call_hierarchy(
                &uri,
                line,
                char_col,
                config.workspace_root.as_deref(),
            ) {
                return val;
            }
        }
    }

    json!(null)
}

/// Handle a `callHierarchy/incomingCalls` request (Task 2.2).
#[must_use]
pub fn handle_call_hierarchy_incoming(params: Option<Value>, config: &MockConfig) -> Value {
    if let (Some(index), Some(p)) = (&config.canned_index, &params) {
        if let Some((uri, line, char_col)) = extract_call_hierarchy_item_position(p) {
            if let Some(val) =
                index.lookup_incoming_calls(&uri, line, char_col, config.workspace_root.as_deref())
            {
                return val;
            }
        }
    }

    json!([])
}

/// Handle a `callHierarchy/outgoingCalls` request (Task 2.2).
#[must_use]
pub fn handle_call_hierarchy_outgoing(params: Option<Value>, config: &MockConfig) -> Value {
    if let (Some(index), Some(p)) = (&config.canned_index, &params) {
        if let Some((uri, line, char_col)) = extract_call_hierarchy_item_position(p) {
            if let Some(val) =
                index.lookup_outgoing_calls(&uri, line, char_col, config.workspace_root.as_deref())
            {
                return val;
            }
        }
    }

    json!([])
}

/// Handle a `textDocument/diagnostic` request.
#[must_use]
pub fn handle_pull_diagnostics(_params: Option<Value>, config: &MockConfig) -> Value {
    json!({
        "kind": "full",
        "items": config.diagnostic_items
    })
}

/// Handle a `workspace/diagnostic` request.
#[must_use]
pub fn handle_workspace_diagnostics(_params: Option<Value>, _config: &MockConfig) -> Value {
    json!({ "items": [] })
}

/// Handle a `textDocument/rangeFormatting` or `textDocument/formatting` request.
#[must_use]
pub fn handle_formatting(_params: Option<Value>, _config: &MockConfig) -> Value {
    json!(null)
}

/// Handle a `shutdown` request — returns null per LSP spec.
#[must_use]
pub const fn handle_shutdown() -> Value {
    json!(null)
}

/// Helper to extract `(uri, line, character)` from `TextDocumentPositionParams`.
fn extract_text_document_position(params: &Value) -> Option<(String, u32, u32)> {
    let uri = params
        .get("textDocument")?
        .get("uri")?
        .as_str()?
        .to_string();
    let pos = params.get("position")?;
    let line = pos.get("line")?.as_u64()? as u32;
    let char_col = pos.get("character")?.as_u64()? as u32;
    Some((uri, line, char_col))
}

/// Helper to extract `(uri, line, character)` from `CallHierarchyIncomingCallsParams` / `CallHierarchyOutgoingCallsParams`.
fn extract_call_hierarchy_item_position(params: &Value) -> Option<(String, u32, u32)> {
    let item = params.get("item")?;
    let uri = item.get("uri")?.as_str()?.to_string();
    let start_pos = item
        .get("selectionRange")
        .or_else(|| item.get("range"))?
        .get("start")?;
    let line = start_pos.get("line")?.as_u64()? as u32;
    let char_col = start_pos.get("character")?.as_u64()? as u32;
    Some((uri, line, char_col))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canned_index::{CannedIndex, PositionEntry};

    #[test]
    fn test_handle_initialize_capabilities() {
        let mut config = MockConfig::default();
        config.position_encoding = Some("utf-8".to_string());
        config.references_provider = true;
        config.implementation_provider = true;

        let res = handle_initialize(None, &config);
        assert_eq!(res["capabilities"]["positionEncoding"], "utf-8");
        assert_eq!(res["capabilities"]["referencesProvider"], true);
        assert_eq!(res["capabilities"]["implementationProvider"], true);
    }

    #[test]
    fn test_handle_references_and_implementation() {
        let mut config = MockConfig::default();
        let mut index = CannedIndex::new();
        index.insert(
            "file:///mock/test.rs",
            5,
            10,
            PositionEntry {
                references: Some(json!([{"uri": "file:///mock/test.rs", "range": {}}])),
                implementation: Some(json!([{"uri": "file:///mock/impl.rs", "range": {}}])),
                ..Default::default()
            },
        );
        config.canned_index = Some(index);

        let params = json!({
            "textDocument": { "uri": "file:///mock/test.rs" },
            "position": { "line": 5, "character": 10 }
        });

        let refs = handle_references(Some(params.clone()), &config);
        assert_eq!(refs[0]["uri"], "file:///mock/test.rs");

        let impls = handle_implementation(Some(params), &config);
        assert_eq!(impls[0]["uri"], "file:///mock/impl.rs");
    }
}
