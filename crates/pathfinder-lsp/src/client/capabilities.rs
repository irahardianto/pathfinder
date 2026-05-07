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
/// All four boolean fields represent on/off server capabilities that drive
/// binary degradation decisions in the tool handlers.
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
    /// MT-2: The reported server name from `initialize` → `serverInfo.name`.
    ///
    /// Used internally to select the appropriate push-diagnostics collection
    /// `None` when the server omits
    /// `serverInfo` (common in older LSPs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
    /// MT-3: Tracks dynamic capability registrations from `client/registerCapability`.
    ///
    /// Maps `registration_id → method` so that `apply_unregistration` can
    /// reverse the effect of a previous `apply_registration` call.
    ///
    /// Populated at runtime by the `registration_watcher_task` background task.
    /// Not populated from the `initialize` handshake.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub dynamic_registrations: std::collections::HashMap<String, String>,
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

        // MT-2: Parse server identity from `serverInfo.name` (LSP 3.15+).
        let server_name = response
            .get("serverInfo")
            .and_then(|si| si.get("name"))
            .and_then(|n| n.as_str())
            .map(ToOwned::to_owned);

        Self {
            definition_provider: is_cap("definitionProvider"),
            call_hierarchy_provider: is_cap("callHierarchyProvider"),
            formatting_provider: is_cap("documentFormattingProvider"),
            diagnostics_strategy,
            workspace_diagnostic_provider,
            server_name,
            // MT-3: Populated at runtime by registration_watcher_task, not from initialize.
            dynamic_registrations: std::collections::HashMap::new(),
        }
    }

    /// MT-2: Return the push-diagnostics collection config tuned for this server.
    ///
    /// MT-3: Apply a single LSP dynamic capability registration.
    ///
    /// Called when the server sends `client/registerCapability`. Updates
    /// `self` in place to reflect the newly registered capability.
    ///
    /// Returns `true` if the capabilities actually changed, `false` if the
    /// `registration_id` was already registered (idempotent) or the `method`
    /// is unknown to Pathfinder.
    ///
    /// The `registration_id` is stored so that `apply_unregistration` can
    /// reverse the change when `client/unregisterCapability` arrives.
    pub fn apply_registration(
        &mut self,
        method: &str,
        registration_id: &str,
        options: &serde_json::Value,
    ) -> bool {
        // Guard: idempotent — same id already registered, skip.
        if self.dynamic_registrations.contains_key(registration_id) {
            return false;
        }

        let changed = match method {
            "textDocument/diagnostic" => {
                self.diagnostics_strategy = DiagnosticsStrategy::Pull;
                // If the options include `workspaceDiagnostics: true` we can
                // also service workspace-wide diagnostic requests.
                if options
                    .get("workspaceDiagnostics")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false)
                {
                    self.workspace_diagnostic_provider = true;
                }
                true
            }
            "textDocument/definition" => {
                self.definition_provider = true;
                true
            }
            "callHierarchy/incomingCalls"
            | "callHierarchy/outgoingCalls"
            | "textDocument/prepareCallHierarchy" => {
                self.call_hierarchy_provider = true;
                true
            }
            "textDocument/formatting" | "textDocument/rangeFormatting" => {
                self.formatting_provider = true;
                true
            }
            // Unknown methods: silently ignore — future-proofing for LSP extensions.
            _ => false,
        };

        if changed {
            self.dynamic_registrations
                .insert(registration_id.to_owned(), method.to_owned());
        }

        changed
    }

    /// MT-3: Reverse a previously applied dynamic capability registration.
    ///
    /// Called when the server sends `client/unregisterCapability`. Looks up
    /// the `registration_id` stored by `apply_registration` and reverts the
    /// corresponding capability flag.
    ///
    /// Returns `true` if the capability was found and reverted, `false` if
    /// `registration_id` was never registered.
    pub fn apply_unregistration(&mut self, registration_id: &str) -> bool {
        let Some(method) = self.dynamic_registrations.remove(registration_id) else {
            return false;
        };

        match method.as_str() {
            "textDocument/diagnostic" => {
                // If no other diagnostic registration remains, revert to None.
                // (If a static capability existed from initialize, this is a best-effort
                //  revert — static caps are not tracked in dynamic_registrations.)
                let has_other_diag = self
                    .dynamic_registrations
                    .values()
                    .any(|m| m == "textDocument/diagnostic");
                if !has_other_diag {
                    self.diagnostics_strategy = DiagnosticsStrategy::None;
                    self.workspace_diagnostic_provider = false;
                }
            }
            "textDocument/definition" => {
                self.definition_provider = false;
            }
            "callHierarchy/incomingCalls"
            | "callHierarchy/outgoingCalls"
            | "textDocument/prepareCallHierarchy" => {
                self.call_hierarchy_provider = false;
            }
            "textDocument/formatting" | "textDocument/rangeFormatting" => {
                self.formatting_provider = false;
            }
            _ => {}
        }

        true
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

    // ── MT-2: server_name parsing ─────────────────────────────────────────────

    #[test]
    fn test_server_name_parsed_from_serverinfo() {
        let response = json!({
            "capabilities": {
                "definitionProvider": true,
                "textDocumentSync": 1
            },
            "serverInfo": {
                "name": "rust-analyzer",
                "version": "2024-01-01"
            }
        });
        let detected = DetectedCapabilities::from_response_json(&response);
        assert_eq!(
            detected.server_name.as_deref(),
            Some("rust-analyzer"),
            "server_name should be parsed from serverInfo.name"
        );
    }

    #[test]
    fn test_server_name_absent_when_no_serverinfo() {
        let response = json!({ "capabilities": { "definitionProvider": true } });
        let detected = DetectedCapabilities::from_response_json(&response);
        assert!(
            detected.server_name.is_none(),
            "server_name should be None when serverInfo is missing"
        );
    }

    #[test]
    fn test_server_name_gopls() {
        let response = json!({
            "capabilities": { "textDocumentSync": 1 },
            "serverInfo": { "name": "gopls" }
        });
        let detected = DetectedCapabilities::from_response_json(&response);
        assert_eq!(detected.server_name.as_deref(), Some("gopls"));
    }

    #[test]
    fn test_server_name_tsserver() {
        let response = json!({
            "capabilities": { "textDocumentSync": 2 },
            "serverInfo": { "name": "typescript-language-server" }
        });
        let detected = DetectedCapabilities::from_response_json(&response);
        assert_eq!(
            detected.server_name.as_deref(),
            Some("typescript-language-server")
        );
    }

    // ── MT-3: apply_registration ──────────────────────────────────────────────

    #[test]
    fn test_apply_registration_enables_pull_diagnostics() {
        // gopls sending client/registerCapability for textDocument/diagnostic
        // should upgrade DetectedCapabilities to DiagnosticsStrategy::Pull
        let mut caps = DetectedCapabilities {
            diagnostics_strategy: DiagnosticsStrategy::Push,
            ..Default::default()
        };
        let options = serde_json::json!({});
        let changed = caps.apply_registration("textDocument/diagnostic", "reg-001", &options);
        assert!(
            changed,
            "apply_registration should return true when caps change"
        );
        assert!(
            matches!(caps.diagnostics_strategy, DiagnosticsStrategy::Pull),
            "after registering textDocument/diagnostic, strategy must be Pull"
        );
    }

    #[test]
    fn test_apply_registration_enables_workspace_diagnostics() {
        let mut caps = DetectedCapabilities::default();
        let options = serde_json::json!({ "workspaceDiagnostics": true });
        caps.apply_registration("textDocument/diagnostic", "reg-002", &options);
        assert!(
            caps.workspace_diagnostic_provider,
            "workspaceDiagnostics option should set workspace_diagnostic_provider"
        );
    }

    #[test]
    fn test_apply_registration_definition_provider() {
        let mut caps = DetectedCapabilities {
            definition_provider: false,
            ..Default::default()
        };
        caps.apply_registration("textDocument/definition", "reg-003", &serde_json::json!({}));
        assert!(
            caps.definition_provider,
            "textDocument/definition registration should enable definition_provider"
        );
    }

    #[test]
    fn test_apply_registration_call_hierarchy() {
        let mut caps = DetectedCapabilities {
            call_hierarchy_provider: false,
            ..Default::default()
        };
        caps.apply_registration(
            "callHierarchy/incomingCalls",
            "reg-004",
            &serde_json::json!({}),
        );
        assert!(
            caps.call_hierarchy_provider,
            "callHierarchy registration should enable call_hierarchy_provider"
        );
    }

    #[test]
    fn test_apply_registration_formatting() {
        let mut caps = DetectedCapabilities {
            formatting_provider: false,
            ..Default::default()
        };
        caps.apply_registration("textDocument/formatting", "reg-005", &serde_json::json!({}));
        assert!(
            caps.formatting_provider,
            "textDocument/formatting registration should enable formatting_provider"
        );
    }

    #[test]
    fn test_apply_registration_unknown_method_returns_false() {
        let mut caps = DetectedCapabilities::default();
        let changed = caps.apply_registration(
            "experimental/unknownFeature",
            "reg-006",
            &serde_json::json!({}),
        );
        assert!(
            !changed,
            "unknown registration method should return false (no change)"
        );
    }

    // ── MT-3: apply_unregistration ────────────────────────────────────────────

    #[test]
    fn test_apply_unregistration_reverts_pull_diagnostics() {
        let mut caps = DetectedCapabilities::default();
        // First register
        caps.apply_registration(
            "textDocument/diagnostic",
            "reg-diag-001",
            &serde_json::json!({}),
        );
        assert!(matches!(
            caps.diagnostics_strategy,
            DiagnosticsStrategy::Pull
        ));

        // Then unregister by the same registration id
        let changed = caps.apply_unregistration("reg-diag-001");
        assert!(
            changed,
            "unregistration should return true when it changed caps"
        );
        assert!(
            matches!(caps.diagnostics_strategy, DiagnosticsStrategy::None),
            "after unregistering textDocument/diagnostic, strategy should revert to None"
        );
    }

    #[test]
    fn test_apply_unregistration_unknown_id_returns_false() {
        let mut caps = DetectedCapabilities::default();
        let changed = caps.apply_unregistration("nonexistent-reg-id");
        assert!(
            !changed,
            "unregistering a nonexistent id should return false"
        );
    }

    #[test]
    fn test_apply_registration_idempotent_same_id() {
        let mut caps = DetectedCapabilities::default();
        caps.apply_registration(
            "textDocument/diagnostic",
            "reg-same",
            &serde_json::json!({}),
        );
        // Applying the same registration again with same id should be a no-op
        let changed = caps.apply_registration(
            "textDocument/diagnostic",
            "reg-same",
            &serde_json::json!({}),
        );
        assert!(
            !changed,
            "re-applying same registration id must be idempotent (no change)"
        );
    }
}
