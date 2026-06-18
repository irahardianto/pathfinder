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
    /// Server supports `textDocument/definition` (`locate`,
    /// `inspect`). Falls back to Tree-sitter heuristic if false.
    pub definition_provider: bool,
    /// Server supports `textDocument/references` (`trace`).
    /// Falls back to Tree-sitter heuristic if false.
    pub references_provider: bool,
    /// Server supports `textDocument/implementation` (`goto_implementation`).
    /// Falls back to Tree-sitter heuristic if false.
    pub implementation_provider: bool,
    /// Server supports `callHierarchy/incomingCalls` + `outgoingCalls`
    /// (`trace`). Falls back to Tree-sitter scan for outgoing only.
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
    /// M-7: Snapshot of capabilities from the `initialize` handshake.
    /// Used to prevent `apply_unregistration` from reverting a capability
    /// that was statically advertised by the server.
    #[serde(skip)]
    pub(crate) static_definition_provider: bool,
    #[serde(skip)]
    pub(crate) static_references_provider: bool,
    #[serde(skip)]
    pub(crate) static_implementation_provider: bool,
    #[serde(skip)]
    pub(crate) static_call_hierarchy_provider: bool,
    #[serde(skip)]
    pub(crate) static_formatting_provider: bool,
    #[serde(skip)]
    pub(crate) static_diagnostics_strategy: DiagnosticsStrategy,
    /// Number of dynamic capability registrations received.
    /// Incremented by `apply_registration` on each successful registration.
    #[serde(default)]
    pub registrations_received: u32,
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
        // Per LSP spec, textDocumentSync: false means "no sync".
        let has_push = if has_pull {
            false
        } else {
            caps.get("textDocumentSync")
                .is_some_and(|v| v.as_bool().unwrap_or_else(|| !v.is_null()))
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

        let definition_provider = is_cap("definitionProvider");
        let references_provider = is_cap("referencesProvider");
        let implementation_provider = is_cap("implementationProvider");
        let call_hierarchy_provider = is_cap("callHierarchyProvider");
        let formatting_provider = is_cap("documentFormattingProvider");

        Self {
            definition_provider,
            references_provider,
            implementation_provider,
            call_hierarchy_provider,
            formatting_provider,
            diagnostics_strategy,
            workspace_diagnostic_provider,
            server_name,
            // MT-3: Populated at runtime by registration_watcher_task, not from initialize.
            dynamic_registrations: std::collections::HashMap::new(),
            // M-7: Snapshot static capabilities for safe unregistration.
            static_definition_provider: definition_provider,
            static_references_provider: references_provider,
            static_implementation_provider: implementation_provider,
            static_call_hierarchy_provider: call_hierarchy_provider,
            static_formatting_provider: formatting_provider,
            static_diagnostics_strategy: diagnostics_strategy,
            // Counts successful dynamic registrations (monotonic counter).
            registrations_received: 0,
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
            "textDocument/references" => {
                self.references_provider = true;
                true
            }
            "textDocument/implementation" => {
                self.implementation_provider = true;
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
            self.registrations_received += 1;
        }

        changed
    }

    /// MT-3: Reverse a previously applied dynamic capability registration.
    ///
    /// Called when the server sends `client/unregisterCapability`. Looks up
    /// the `registration_id` stored by `apply_registration` and reverts the
    /// corresponding capability flag.
    ///
    /// M-7: Does NOT revert capabilities that were statically advertised by
    /// the server during the `initialize` handshake. Only dynamically added
    /// capabilities are reverted.
    ///
    /// Returns `true` if the capability was found and reverted, `false` if
    /// `registration_id` was never registered.
    pub fn apply_unregistration(&mut self, registration_id: &str) -> bool {
        let Some(method) = self.dynamic_registrations.remove(registration_id) else {
            return false;
        };

        match method.as_str() {
            "textDocument/diagnostic" => {
                // If no other diagnostic registration remains, revert — but only
                // if the static initialize didn't already have this capability.
                let has_other_diag = self
                    .dynamic_registrations
                    .values()
                    .any(|m| m == "textDocument/diagnostic");
                if !has_other_diag
                    && !matches!(self.static_diagnostics_strategy, DiagnosticsStrategy::Pull)
                {
                    self.diagnostics_strategy = self.static_diagnostics_strategy;
                    self.workspace_diagnostic_provider = false;
                }
            }
            "textDocument/definition" if !self.static_definition_provider => {
                self.definition_provider = false;
            }
            "textDocument/references" if !self.static_references_provider => {
                self.references_provider = false;
            }
            "textDocument/implementation" if !self.static_implementation_provider => {
                self.implementation_provider = false;
            }
            "callHierarchy/incomingCalls"
            | "callHierarchy/outgoingCalls"
            | "textDocument/prepareCallHierarchy"
                if !self.static_call_hierarchy_provider =>
            {
                self.call_hierarchy_provider = false;
            }
            "textDocument/formatting" | "textDocument/rangeFormatting"
                if !self.static_formatting_provider =>
            {
                self.formatting_provider = false;
            }
            _ => {}
        }

        true
    }
}

#[cfg(test)]
#[path = "capabilities_test.rs"]
mod tests;
