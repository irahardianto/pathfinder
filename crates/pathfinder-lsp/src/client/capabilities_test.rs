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

#[test]
fn test_from_response_json_definition_provider_object() {
    let response = json!({
        "capabilities": {
            "definitionProvider": { "workDoneProgress": false }
        }
    });
    let detected = DetectedCapabilities::from_response_json(&response);
    assert!(
        detected.definition_provider,
        "object form definitionProvider should be treated as true"
    );
}

#[test]
fn test_from_response_json_all_capabilities_enabled() {
    let response = json!({
        "capabilities": {
            "definitionProvider": true,
            "callHierarchyProvider": true,
            "documentFormattingProvider": true,
            "diagnosticProvider": {
                "interFileDependencies": true,
                "workspaceDiagnostics": true
            }
        },
        "serverInfo": { "name": "test-server" }
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
    assert_eq!(detected.server_name.as_deref(), Some("test-server"));
}

#[test]
fn test_from_response_json_null_capabilities() {
    let response = json!({ "capabilities": { "definitionProvider": null } });
    let detected = DetectedCapabilities::from_response_json(&response);
    assert!(
        !detected.definition_provider,
        "null definitionProvider should be false"
    );
}

#[test]
fn test_apply_unregistration_reverts_definition_provider_dynamic_only() {
    let mut caps = DetectedCapabilities::default();
    caps.apply_registration("textDocument/definition", "reg-def-001", &json!({}));
    assert!(caps.definition_provider);

    caps.apply_unregistration("reg-def-001");
    assert!(
        !caps.definition_provider,
        "dynamic definition registration should be reverted"
    );
}

#[test]
fn test_apply_unregistration_does_not_revert_static_capability() {
    let mut caps = DetectedCapabilities {
        definition_provider: true,
        static_definition_provider: true,
        ..Default::default()
    };
    caps.apply_registration("textDocument/definition", "reg-static-001", &json!({}));
    assert!(caps.definition_provider);

    caps.apply_unregistration("reg-static-001");
    assert!(
        caps.definition_provider,
        "should NOT revert static definition_provider"
    );
}

#[test]
fn test_apply_unregistration_reverts_call_hierarchy_dynamic_only() {
    let mut caps = DetectedCapabilities::default();
    caps.apply_registration("callHierarchy/incomingCalls", "reg-ch-001", &json!({}));
    assert!(caps.call_hierarchy_provider);

    caps.apply_unregistration("reg-ch-001");
    assert!(
        !caps.call_hierarchy_provider,
        "dynamic call hierarchy registration should be reverted"
    );
}

#[test]
fn test_apply_unregistration_reverts_formatting_dynamic_only() {
    let mut caps = DetectedCapabilities::default();
    caps.apply_registration("textDocument/formatting", "reg-fmt-001", &json!({}));
    assert!(caps.formatting_provider);

    caps.apply_unregistration("reg-fmt-001");
    assert!(
        !caps.formatting_provider,
        "dynamic formatting registration should be reverted"
    );
}

#[test]
fn test_multiple_dynamic_registrations_same_method() {
    let mut caps = DetectedCapabilities::default();
    caps.apply_registration("textDocument/diagnostic", "reg-d1", &json!({}));
    caps.apply_registration("textDocument/diagnostic", "reg-d2", &json!({}));

    caps.apply_unregistration("reg-d1");
    assert!(
        matches!(caps.diagnostics_strategy, DiagnosticsStrategy::Pull),
        "should remain Pull because reg-d2 still active"
    );

    caps.apply_unregistration("reg-d2");
    assert!(
        matches!(caps.diagnostics_strategy, DiagnosticsStrategy::None),
        "should revert to None when all registrations removed (static was None)"
    );
}

#[test]
fn test_apply_unregistration_restore_static_push_after_dynamic_pull() {
    let response = json!({
        "capabilities": {
            "textDocumentSync": 1
        }
    });
    let mut caps = DetectedCapabilities::from_response_json(&response);
    assert!(
        matches!(caps.static_diagnostics_strategy, DiagnosticsStrategy::Push),
        "textDocumentSync=1 means static Push diagnostics"
    );
    assert!(
        matches!(caps.diagnostics_strategy, DiagnosticsStrategy::Push),
        "diagnostics_strategy starts as Push"
    );

    caps.apply_registration(
        "textDocument/diagnostic",
        "reg-pull",
        &json!({ "workspaceDiagnostics": true }),
    );
    assert!(
        matches!(caps.diagnostics_strategy, DiagnosticsStrategy::Pull),
        "dynamic registration should set Pull"
    );
    assert!(caps.workspace_diagnostic_provider);

    caps.apply_unregistration("reg-pull");
    assert!(
        matches!(caps.diagnostics_strategy, DiagnosticsStrategy::Push),
        "should revert to static Push after unregistering dynamic Pull"
    );
    assert!(
        !caps.workspace_diagnostic_provider,
        "workspace_diagnostic_provider should be cleared on unregistration"
    );
}

#[test]
fn test_text_document_sync_false_does_not_enable_push_diagnostics() {
    let response = json!({
        "capabilities": {
            "textDocumentSync": false,
            "definitionProvider": true
        }
    });
    let detected = DetectedCapabilities::from_response_json(&response);
    assert!(
        matches!(detected.diagnostics_strategy, DiagnosticsStrategy::None),
        "textDocumentSync: false should NOT enable Push diagnostics (per LSP spec)"
    );
    assert!(matches!(
        detected.static_diagnostics_strategy,
        DiagnosticsStrategy::None
    ));
}

#[test]
fn test_from_response_json_static_capabilities_snapshot() {
    let response = json!({
        "capabilities": {
            "definitionProvider": true,
            "callHierarchyProvider": true,
            "documentFormattingProvider": false,
            "textDocumentSync": 1
        }
    });
    let detected = DetectedCapabilities::from_response_json(&response);
    assert!(detected.static_definition_provider);
    assert!(detected.static_call_hierarchy_provider);
    assert!(!detected.static_formatting_provider);
    assert!(matches!(
        detected.static_diagnostics_strategy,
        DiagnosticsStrategy::Push
    ));
}

#[test]
fn test_diagnostics_strategy_default_is_none() {
    assert!(matches!(
        DiagnosticsStrategy::default(),
        DiagnosticsStrategy::None
    ));
}

#[test]
fn test_detected_capabilities_default() {
    let caps = DetectedCapabilities::default();
    assert!(!caps.definition_provider);
    assert!(!caps.call_hierarchy_provider);
    assert!(!caps.formatting_provider);
    assert!(!caps.workspace_diagnostic_provider);
    assert!(caps.server_name.is_none());
    assert!(caps.dynamic_registrations.is_empty());
    assert_eq!(caps.registrations_received, 0);
}

#[test]
fn test_registrations_received_counter() {
    let mut caps = DetectedCapabilities::default();
    assert_eq!(caps.registrations_received, 0);

    // First registration: definition provider
    let changed =
        caps.apply_registration("textDocument/definition", "reg-1", &serde_json::Value::Null);
    assert!(changed);
    assert_eq!(caps.registrations_received, 1);

    // Second registration: references provider
    let changed =
        caps.apply_registration("textDocument/references", "reg-2", &serde_json::Value::Null);
    assert!(changed);
    assert_eq!(caps.registrations_received, 2);

    // Idempotent: same registration_id should not increment
    let changed =
        caps.apply_registration("textDocument/definition", "reg-1", &serde_json::Value::Null);
    assert!(!changed);
    assert_eq!(
        caps.registrations_received, 2,
        "duplicate registration_id must not increment counter"
    );

    // Unknown method: should not increment
    let changed = caps.apply_registration(
        "textDocument/unknownMethod",
        "reg-3",
        &serde_json::Value::Null,
    );
    assert!(!changed);
    assert_eq!(
        caps.registrations_received, 2,
        "unknown method must not increment counter"
    );

    // Unregistration should NOT decrement (counter only goes up)
    let reverted = caps.apply_unregistration("reg-1");
    assert!(reverted);
    assert_eq!(
        caps.registrations_received, 2,
        "unregistration must not decrement counter"
    );
}
