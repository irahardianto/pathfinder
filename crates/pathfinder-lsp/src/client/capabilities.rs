//! LSP `ServerCapabilities` parsing.
//!
//! On a successful `initialize`, the LSP returns its `ServerCapabilities` inside
//! `result.capabilities`. We extract only the capabilities that Pathfinder cares
//! about into [`DetectedCapabilities`], which drives graceful degradation
//! throughout the tool handlers.
//!
//! We parse directly from `serde_json::Value` rather than deserialising into
//! `lsp_types::InitializeResult` to avoid dependency on lsp-types' internal URI
//! types, which vary between crate versions.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The subset of LSP server capabilities that Pathfinder uses.
///
/// All four fields are booleans by design: each represents an on/off server capability
/// that drives binary degradation decisions in the tool handlers.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DetectedCapabilities {
    /// Server supports `textDocument/definition` (`get_definition`,
    /// `read_with_deep_context`). Falls back to Tree-sitter heuristic if false.
    pub definition_provider: bool,
    /// Server supports `callHierarchy/incomingCalls` + `outgoingCalls`
    /// (`analyze_impact`). Falls back to Tree-sitter scan for outgoing only.
    pub call_hierarchy_provider: bool,
    /// Server supports `textDocument/formatting` (edit tools refinement).
    /// Tree-sitter indentation baseline is always applied first.
    pub formatting_provider: bool,
    /// Server supports Pull Diagnostics — `textDocument/diagnostic` (LSP 3.17).
    /// If false, edit tools use `validation_skipped: true`.
    pub diagnostic_provider: bool,
}

impl DetectedCapabilities {
    /// Parse from the raw `initialize` response JSON.
    ///
    /// Expects the full `result` object from the `initialize` response —
    /// i.e., `response["capabilities"]` will be inspected.
    /// Missing fields default to `false` (absent capability = not supported).
    #[must_use]
    pub fn from_response_json(response: &Value) -> Self {
        let caps = &response["capabilities"];

        // `definitionProvider` can be `true`, `false`, or an object `{}`.
        // An object means "supported" (non-null) — we use is_some_and for the
        // outer Option and separate bool/object handling inside.
        let is_cap = |key: &str| -> bool {
            caps.get(key).is_some_and(|v| match v.as_bool() {
                Some(b) => b,
                None => !v.is_null(), // object form = supported
            })
        };

        Self {
            definition_provider: is_cap("definitionProvider"),
            call_hierarchy_provider: is_cap("callHierarchyProvider"),
            formatting_provider: is_cap("documentFormattingProvider"),
            diagnostic_provider: is_cap("diagnosticProvider"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_empty_capabilities() {
        let response = json!({ "capabilities": {} });
        let detected = DetectedCapabilities::from_response_json(&response);
        assert!(!detected.definition_provider);
        assert!(!detected.call_hierarchy_provider);
        assert!(!detected.formatting_provider);
        assert!(!detected.diagnostic_provider);
    }

    #[test]
    fn test_bool_true_capabilities() {
        let response = json!({
            "capabilities": {
                "definitionProvider": true,
                "callHierarchyProvider": true,
                "documentFormattingProvider": true,
                "diagnosticProvider": true
            }
        });
        let detected = DetectedCapabilities::from_response_json(&response);
        assert!(detected.definition_provider);
        assert!(detected.call_hierarchy_provider);
        assert!(detected.formatting_provider);
        assert!(detected.diagnostic_provider);
    }

    #[test]
    fn test_object_form_capabilities() {
        // Some LSPs return an empty object `{}` rather than `true`
        let response = json!({
            "capabilities": {
                "definitionProvider": {},
                "callHierarchyProvider": {},
                "documentFormattingProvider": {},
                "diagnosticProvider": {
                    "interFileDependencies": true,
                    "workspaceDiagnostics": true
                }
            }
        });
        let detected = DetectedCapabilities::from_response_json(&response);
        assert!(detected.definition_provider);
        assert!(detected.call_hierarchy_provider);
        assert!(detected.formatting_provider);
        assert!(detected.diagnostic_provider);
    }

    #[test]
    fn test_bool_false_capabilities() {
        let response = json!({
            "capabilities": {
                "definitionProvider": false,
                "callHierarchyProvider": false
            }
        });
        let detected = DetectedCapabilities::from_response_json(&response);
        assert!(!detected.definition_provider);
        assert!(!detected.call_hierarchy_provider);
    }
}


