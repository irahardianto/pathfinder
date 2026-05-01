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

/// How an LSP server provides diagnostics.
///
/// Determines the validation pipeline strategy for edit tools.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DiagnosticsStrategy {
    /// LSP supports `textDocument/diagnostic` (LSP 3.17 pull model).
    /// Most capable: request diagnostics on demand for any file.
    Pull,

    /// LSP supports `textDocument/publishDiagnostics` (push model).
    /// Requires subscribing to notifications after didOpen/didChange.
    /// Used by gopls, typescript-language-server, and most LSPs.
    Push,

    /// LSP does not support any diagnostics capability.
    #[default]
    None,
}

impl DiagnosticsStrategy {
    /// Returns the strategy name as a string for `capability_status` responses.
    #[must_use]
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Pull => "pull",
            Self::Push => "push",
            Self::None => "none",
        }
    }
}

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
    /// How this LSP provides diagnostics (pull, push, or none).
    pub diagnostics_strategy: DiagnosticsStrategy,
    /// Server supports Pull Diagnostics — `workspace/diagnostic` (LSP 3.17).
    /// Used for catching cross-file regressions during edits.
    /// Only relevant when `diagnostics_strategy == DiagnosticsStrategy::Pull`.
    pub workspace_diagnostic_provider: bool,
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
            caps.get(key)
                .is_some_and(|v| v.as_bool().unwrap_or_else(|| !v.is_null()))
        };

        // Check pull diagnostics first (preferred — more deterministic)
        let has_pull = caps
            .get("diagnosticProvider")
            .is_some_and(|v| v.as_bool().unwrap_or_else(|| !v.is_null()));

        let workspace_diagnostic_provider = caps
            .get("diagnosticProvider")
            .and_then(|v| v.get("workspaceDiagnostics"))
            .is_some_and(|v| v.as_bool().unwrap_or_else(|| !v.is_null()));

        // Push diagnostics: check if textDocumentSync is advertised.
        // Most LSPs that support document sync also push diagnostics.
        // Don't check this if pull is available (pull is preferred).
        let has_push = if has_pull {
            false
        } else {
            caps.get("textDocumentSync").is_some_and(|v| !v.is_null())
        };

        let diagnostics_strategy = if has_pull {
            DiagnosticsStrategy::Pull
        } else if has_push {
            DiagnosticsStrategy::Push
        } else {
            DiagnosticsStrategy::None
        };

        Self {
            definition_provider: is_cap("definitionProvider"),
            call_hierarchy_provider: is_cap("callHierarchyProvider"),
            formatting_provider: is_cap("documentFormattingProvider"),
            diagnostics_strategy,
            workspace_diagnostic_provider,
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
        assert!(matches!(
            detected.diagnostics_strategy,
            DiagnosticsStrategy::None
        ));
        assert!(!detected.workspace_diagnostic_provider);
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
        assert!(matches!(
            detected.diagnostics_strategy,
            DiagnosticsStrategy::Pull
        ));
        assert!(!detected.workspace_diagnostic_provider);
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
        assert!(matches!(
            detected.diagnostics_strategy,
            DiagnosticsStrategy::Pull
        ));
        assert!(detected.workspace_diagnostic_provider);
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
        assert!(matches!(
            detected.diagnostics_strategy,
            DiagnosticsStrategy::None
        ));
        assert!(!detected.workspace_diagnostic_provider);
    }

    #[test]
    fn test_push_diagnostics_detected() {
        // LSP with textDocumentSync but no diagnosticProvider = Push strategy
        // This is how gopls and typescript-language-server advertise
        let response = json!({
            "capabilities": {
                "textDocumentSync": 1, // Full sync mode (as number)
                "definitionProvider": true
            }
        });
        let detected = DetectedCapabilities::from_response_json(&response);
        assert!(detected.definition_provider);
        assert!(matches!(
            detected.diagnostics_strategy,
            DiagnosticsStrategy::Push
        ));
    }

    #[test]
    fn test_push_diagnostics_detected_object_sync() {
        // textDocumentSync can also be an object
        let response = json!({
            "capabilities": {
                "textDocumentSync": {
                    "openClose": true,
                    "change": 2,
                    "willSave": true
                },
                "definitionProvider": true
            }
        });
        let detected = DetectedCapabilities::from_response_json(&response);
        assert!(matches!(
            detected.diagnostics_strategy,
            DiagnosticsStrategy::Push
        ));
    }

    #[test]
    fn test_pull_preferred_over_push() {
        // LSP with both diagnosticProvider AND textDocumentSync
        // Pull should be preferred
        let response = json!({
            "capabilities": {
                "textDocumentSync": 1,
                "diagnosticProvider": true
            }
        });
        let detected = DetectedCapabilities::from_response_json(&response);
        // When pull is available, push should NOT be chosen
        assert!(matches!(
            detected.diagnostics_strategy,
            DiagnosticsStrategy::Pull
        ));
    }

    #[test]
    fn test_diagnostics_strategy_as_str() {
        assert_eq!(DiagnosticsStrategy::Pull.as_str(), "pull");
        assert_eq!(DiagnosticsStrategy::Push.as_str(), "push");
        assert_eq!(DiagnosticsStrategy::None.as_str(), "none");
    }
}
