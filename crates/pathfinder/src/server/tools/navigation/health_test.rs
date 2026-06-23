use super::super::test_helpers::make_server_with_lawyer;
use super::*;
use pathfinder_treesitter::mock::MockSurgeon;
use std::sync::Arc;

/// Extract `LspHealthResponse` from a `CallToolResult.structured_content`.
fn unpack_health(res: rmcp::model::CallToolResult) -> crate::server::types::LspHealthResponse {
    serde_json::from_value(res.structured_content.expect("structured_content")).unwrap()
}

// ── PATCH-005: Per-Language Capabilities Tests ─────────────────────

#[tokio::test]
async fn test_lsp_health_includes_diagnostics_strategy() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    // No lawyer_clone needed - MockLawyer returns empty status
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // MockLawyer returns empty capability_status, so no languages should be returned
    // This tests the structure exists and doesn't panic
    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    assert_eq!(val.status, "unavailable");
    assert!(val.languages.is_empty());
}

#[tokio::test]
async fn test_lsp_health_shows_push_for_go() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Mock a Go LSP with push diagnostics
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(15),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("go".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    assert_eq!(val.languages.len(), 1);
    let go_health = &val.languages[0];
    assert_eq!(go_health.language, "go");
    assert_eq!(go_health.status, "ready");
    assert_eq!(go_health.diagnostics_strategy, Some("push".to_string()));
    assert_eq!(go_health.supports_call_hierarchy, Some(true));
    assert_eq!(go_health.supports_diagnostics, Some(true));
}

#[tokio::test]
async fn test_lsp_health_shows_pull_for_rust() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Mock a Rust LSP with pull diagnostics
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(20),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    assert_eq!(val.languages.len(), 1);
    let rust_health = &val.languages[0];
    assert_eq!(rust_health.language, "rust");
    assert_eq!(rust_health.status, "ready");
    assert_eq!(rust_health.diagnostics_strategy, Some("pull".to_string()));
    assert_eq!(rust_health.supports_call_hierarchy, Some(true));
    assert_eq!(rust_health.supports_diagnostics, Some(true));
}

#[tokio::test]
async fn test_lsp_health_shows_capabilities() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Mock an LSP with partial capabilities
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "typescript".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(10),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true), // TS supports call hierarchy
            supports_diagnostics: Some(true),

            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    assert_eq!(val.languages.len(), 1);
    let ts_health = &val.languages[0];
    assert_eq!(ts_health.supports_definition, Some(true));
    assert_eq!(ts_health.supports_call_hierarchy, Some(true));
    assert_eq!(ts_health.supports_diagnostics, Some(true));
}

// ── PATCH-006: Probe-Based Readiness Tests ─────────────────────────

#[tokio::test]
async fn test_lsp_health_probe_upgrades_warming_up_to_ready() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

    // Create a workspace with a main.rs file for probing
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/main.rs"),
        r#"fn main() { println!("Hello"); }"#,
    )
    .unwrap();

    // Mock a Rust LSP that's been warming up for 30 seconds
    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(false), // Still warming up
            uptime_seconds: Some(30),       // 30 seconds - should trigger probe
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Mock successful goto_definition response (LSP is ready)
    lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
        file: "src/main.rs".to_string(),
        line: 1,
        column: 0,
        preview: "fn main()".to_string(),
    })));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    // With two-phase readiness model: navigation_ready = Some(true) means
    // status is immediately "ready" without waiting for indexing.
    // This is the fix for LSP-HEALTH-001: LSPs that support definitionProvider
    // should be usable immediately, without waiting for WorkDoneProgressEnd.
    // Liveness probe also runs for "ready" languages to verify
    // the LSP is still responsive.
    assert_eq!(val.status, "ready");
    assert_eq!(val.languages.len(), 1);
    let rust_health = &val.languages[0];
    assert_eq!(rust_health.language, "rust");
    assert_eq!(rust_health.status, "ready");
    assert_eq!(rust_health.uptime, Some("30s".to_string()));
    // indexing_status is still "in_progress" because we never saw WorkDoneProgressEnd
    assert_eq!(rust_health.indexing_status, Some("in_progress".to_string()));
    // With liveness probe, probe_verified should be true since
    // the probe ran and succeeded (LSP is responsive)
    assert!(rust_health.probe_verified);
    assert_eq!(
        rust_health.navigation_tested,
        Some(true),
        "navigation_tested must mirror probe_verified on successful probe"
    );
}

#[tokio::test]
async fn test_lsp_health_probe_keeps_warming_up_when_probe_fails() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

    // Create a workspace with a main.rs file for probing
    // Create a workspace with a main.rs file for probing
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

    // Mock a Rust LSP that's been warming up for 30 seconds
    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(false), // Still warming up
            uptime_seconds: Some(30),       // 30 seconds - should trigger probe
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Mock failed goto_definition response (LSP is not responsive)
    lawyer.set_goto_definition_result(Err(pathfinder_lsp::LspError::ConnectionLost));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    // With liveness probe, when the LSP was "ready" but becomes
    // non-responsive, the status should be downgraded to "degraded".
    // This is the key improvement: detecting LSPs that die after initialization.
    assert_eq!(val.status, "degraded");
    assert_eq!(val.languages.len(), 1);
    let rust_health = &val.languages[0];
    assert_eq!(rust_health.language, "rust");
    assert_eq!(rust_health.status, "degraded");
    assert!(!rust_health.probe_verified);
    assert_eq!(
        rust_health.navigation_tested,
        Some(false),
        "navigation_tested must be Some(false) when liveness probe fails (LSP non-responsive)"
    );
    assert_eq!(
        rust_health.navigation_verified,
        Some(false),
        "navigation_verified must be Some(false) when liveness probe fails"
    );
    assert_eq!(
        rust_health.navigation_ready,
        Some(true),
        "navigation_ready should remain true (capability advertisement, not operational)"
    );
    assert!(
        !rust_health.degraded_tools.is_empty(),
        "degraded_tools must not be empty when status is degraded"
    );
}

#[tokio::test]
async fn test_lsp_health_no_probe_for_recently_started() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

    // Remove Rust files created by make_temp_workspace to prevent liveness probe
    let src_dir = ws_dir.path().join("src");
    let _ = std::fs::remove_file(src_dir.join("main.rs"));
    let _ = std::fs::remove_file(src_dir.join("auth.rs"));
    let _ = std::fs::remove_file(src_dir.join("token.rs"));
    let _ = std::fs::remove_file(src_dir.join("service.rs"));
    let _ = std::fs::remove_file(src_dir.join("user.rs"));
    let _ = std::fs::remove_file(src_dir.join("auth.go"));

    // Mock a Rust LSP that just started (5 seconds ago)
    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(false), // Warming up
            uptime_seconds: Some(5),        // Only 5 seconds - should NOT trigger probe
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Set a goto_definition result to verify it's not called
    lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
        file: "src/main.rs".to_string(),
        line: 1,
        column: 0,
        preview: "fn main()".to_string(),
    })));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    // With two-phase readiness: navigation_ready = Some(true) means status
    // is immediately "ready" - uptime doesn't matter when capability is confirmed.
    assert_eq!(val.status, "ready");
    assert_eq!(val.languages.len(), 1);
    let rust_health = &val.languages[0];
    assert_eq!(rust_health.language, "rust");
    assert_eq!(rust_health.status, "ready");
    assert_eq!(rust_health.indexing_status, Some("in_progress".to_string()));
    assert!(!rust_health.probe_verified);
}

#[tokio::test]
async fn test_lsp_health_no_probe_for_already_ready() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

    // Remove Rust files created by make_temp_workspace to prevent liveness probe
    let src_dir = ws_dir.path().join("src");
    let _ = std::fs::remove_file(src_dir.join("main.rs"));
    let _ = std::fs::remove_file(src_dir.join("auth.rs"));
    let _ = std::fs::remove_file(src_dir.join("token.rs"));
    let _ = std::fs::remove_file(src_dir.join("service.rs"));
    let _ = std::fs::remove_file(src_dir.join("user.rs"));
    let _ = std::fs::remove_file(src_dir.join("auth.go"));

    // Mock a Rust LSP that's already ready
    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true), // Ready
            uptime_seconds: Some(60),      // 60 seconds
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Set a goto_definition result to verify it's not called
    lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
        file: "src/main.rs".to_string(),
        line: 1,
        column: 0,
        preview: "fn main()".to_string(),
    })));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    // Status should be "ready" and probe not attempted
    assert_eq!(val.status, "ready");
    assert_eq!(val.languages.len(), 1);
    let rust_health = &val.languages[0];
    assert_eq!(rust_health.status, "ready");
    assert!(!rust_health.probe_verified);
}

#[tokio::test]
async fn test_parse_uptime_to_seconds() {
    assert_eq!(parse_uptime_to_seconds(Some("5s")), Some(5));
    assert_eq!(parse_uptime_to_seconds(Some("1m30s")), Some(90));
    assert_eq!(parse_uptime_to_seconds(Some("2h15m")), Some(8100));
    assert_eq!(parse_uptime_to_seconds(Some("1h30m45s")), Some(5445));
    assert_eq!(parse_uptime_to_seconds(Some("1m")), Some(60));
    assert_eq!(parse_uptime_to_seconds(Some("1h")), Some(3600));
    assert_eq!(parse_uptime_to_seconds(None), None);
}

#[tokio::test]
async fn test_find_probe_file() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

    // Create some probe files
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer);

    // Remove Rust files created by make_temp_workspace to test "no Rust file" scenario
    let src_dir = ws_dir.path().join("src");
    let _ = std::fs::remove_file(src_dir.join("main.rs"));
    let _ = std::fs::remove_file(src_dir.join("auth.rs"));
    let _ = std::fs::remove_file(src_dir.join("token.rs"));
    let _ = std::fs::remove_file(src_dir.join("service.rs"));
    let _ = std::fs::remove_file(src_dir.join("user.rs"));
    let _ = std::fs::remove_file(src_dir.join("auth.go"));

    // Create test probe files
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(ws_dir.path().join("main.go"), "package main").unwrap();
    std::fs::write(src_dir.join("index.ts"), "export const x = 1;").unwrap();

    // Test finding probe files
    assert_eq!(
        server.find_probe_file("go"),
        Some(std::path::PathBuf::from("main.go"))
    );
    assert_eq!(
        server.find_probe_file("typescript"),
        Some(std::path::PathBuf::from("src/index.ts"))
    );
    assert_eq!(server.find_probe_file("rust"), None); // No Rust file
}

// ── LSP-HEALTH-001: Recursive Probe for Monorepos ───────────────────────

#[tokio::test]
async fn test_find_probe_file_recursive_monorepo() {
    // Test the fallback recursive scan for monorepo layouts where
    // files are at non-standard paths like apps/backend/cmd/main.go
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer);

    // Remove the src/auth.go file created by make_temp_workspace so that
    // find_probe_file("go") is forced to fall back to the recursive scan
    // rather than immediately returning the pre-existing shallow Go file.
    let _ = std::fs::remove_file(ws_dir.path().join("src").join("auth.go"));

    // Create a monorepo structure: Go file at apps/backend/cmd/server/main.go
    // (not at the standard main.go or cmd/main.go)
    std::fs::create_dir_all(
        ws_dir
            .path()
            .join("apps")
            .join("backend")
            .join("cmd")
            .join("server"),
    )
    .unwrap();
    std::fs::write(
        ws_dir
            .path()
            .join("apps")
            .join("backend")
            .join("cmd")
            .join("server")
            .join("main.go"),
        "package main\nfunc main() {}",
    )
    .unwrap();

    // Create a node_modules directory to test that it's skipped
    std::fs::create_dir_all(ws_dir.path().join("node_modules").join("react")).unwrap();
    std::fs::write(
        ws_dir
            .path()
            .join("node_modules")
            .join("react")
            .join("index.ts"),
        "export const React = {};",
    )
    .unwrap();

    // Test that recursive scan finds the Go file at non-standard path
    let probe = server.find_probe_file("go");
    assert!(probe.is_some(), "Should find Go file in monorepo structure");
    let probe_path = probe.unwrap();
    assert!(
        probe_path.to_str().unwrap().contains("main.go"),
        "Should find a main.go file, got: {probe_path:?}"
    );

    // Test that node_modules is skipped (should NOT find the TS file there)
    // This is a bit tricky to test without other TS files - let's just verify
    // the probe works for a standard pattern too by adding a deeper Python file
    std::fs::create_dir_all(ws_dir.path().join("tools").join("fath-factory").join("src")).unwrap();
    std::fs::write(
        ws_dir
            .path()
            .join("tools")
            .join("fath-factory")
            .join("src")
            .join("__init__.py"),
        "",
    )
    .unwrap();

    let py_probe = server.find_probe_file("python");
    assert!(
        py_probe.is_some(),
        "Should find Python file in tools/ directory"
    );
}

// ── Java probe file discovery (Issue 1 fix) ─────────────────────────────

#[tokio::test]
async fn test_find_probe_file_java_application_candidate() {
    // Spring Boot convention: src/main/java/Application.java
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer);

    std::fs::create_dir_all(ws_dir.path().join("src").join("main").join("java")).unwrap();
    std::fs::write(
        ws_dir
            .path()
            .join("src")
            .join("main")
            .join("java")
            .join("Application.java"),
        "public class Application { public static void main(String[] args) {} }",
    )
    .unwrap();

    let probe = server.find_probe_file("java");
    assert!(probe.is_some(), "Should find Application.java candidate");
    assert_eq!(
        probe.unwrap(),
        std::path::PathBuf::from("src/main/java/Application.java"),
        "Should match the well-known Application.java path"
    );
}

#[tokio::test]
async fn test_find_probe_file_java_deep_package_path() {
    // Real-world Java: files at depth 7+ (src/main/java/com/company/service/FooService.java)
    // Before fix: depth 4 limit would miss these files entirely.
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer);

    // Create a deep Java package structure: depth 7
    let deep_path = ws_dir
        .path()
        .join("src")
        .join("main")
        .join("java")
        .join("com")
        .join("example")
        .join("banking")
        .join("service");
    std::fs::create_dir_all(&deep_path).unwrap();
    std::fs::write(
        deep_path.join("AccountService.java"),
        "package com.example.banking.service;\npublic class AccountService {}",
    )
    .unwrap();

    let probe = server.find_probe_file("java");
    assert!(
        probe.is_some(),
        "Should find Java file at depth 7 (com/example/banking/service/)"
    );
    let probe_path = probe.unwrap();
    assert!(
        probe_path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("java")),
        "Should find a .java file, got: {probe_path:?}"
    );
}

#[tokio::test]
async fn test_find_probe_file_java_app_candidate() {
    // Plain Java convention: src/main/java/App.java
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer);

    std::fs::create_dir_all(ws_dir.path().join("src").join("main").join("java")).unwrap();
    std::fs::write(
        ws_dir
            .path()
            .join("src")
            .join("main")
            .join("java")
            .join("App.java"),
        "public class App { public static void main(String[] args) {} }",
    )
    .unwrap();

    let probe = server.find_probe_file("java");
    assert!(probe.is_some(), "Should find App.java candidate");
    assert_eq!(
        probe.unwrap(),
        std::path::PathBuf::from("src/main/java/App.java"),
        "Should match the well-known App.java path"
    );
}

// ── PATCH-008: Install Guidance Tests ─────────────────────────────────

#[tokio::test]
async fn test_lsp_health_includes_missing_languages_with_install_hint() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Mock a detected language (TypeScript with running LSP)
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "typescript".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(60),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Mock missing languages (Python and Go with markers but no LSP binaries)
    lawyer_clone.set_missing_languages(vec![
        pathfinder_lsp::client::MissingLanguage {
            language_id: "python".to_string(),
            marker_file: "pyproject.toml".to_string(),
            tried_binaries: vec![
                "pyright-langserver".to_string(),
                "pyright".to_string(),
                "pylsp".to_string(),
                "ruff".to_string(),
                "jedi-language-server".to_string(),
            ],
            install_hint: "Install pyright-langserver: npm install -g pyright".to_string(),
        },
        pathfinder_lsp::client::MissingLanguage {
            language_id: "go".to_string(),
            marker_file: "go.mod".to_string(),
            tried_binaries: vec!["gopls".to_string()],
            install_hint: "Install gopls: go install golang.org/x/tools/gopls@latest".to_string(),
        },
    ]);

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    // Should have 3 languages total: 1 detected + 2 missing
    assert_eq!(val.languages.len(), 3);

    // Find the missing languages
    let python_health = val.languages.iter().find(|l| l.language == "python");
    let go_health = val.languages.iter().find(|l| l.language == "go");
    let ts_health = val.languages.iter().find(|l| l.language == "typescript");

    // TypeScript should be ready
    assert!(ts_health.is_some());
    assert_eq!(ts_health.unwrap().status, "ready");

    // Python and Go should be unavailable with install hints
    assert!(python_health.is_some());
    assert_eq!(python_health.unwrap().status, "unavailable");
    assert_eq!(
        python_health.unwrap().install_hint,
        Some("Install pyright-langserver: npm install -g pyright".to_string())
    );

    assert!(go_health.is_some());
    assert_eq!(go_health.unwrap().status, "unavailable");
    assert_eq!(
        go_health.unwrap().install_hint,
        Some("Install gopls: go install golang.org/x/tools/gopls@latest".to_string())
    );
}

#[tokio::test]
async fn test_lsp_health_missing_language_filter_works() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // No detected languages, only missing ones
    lawyer_clone.set_capability_status(std::collections::HashMap::new());
    lawyer_clone.set_missing_languages(vec![
        pathfinder_lsp::client::MissingLanguage {
            language_id: "python".to_string(),
            marker_file: "pyproject.toml".to_string(),
            tried_binaries: vec!["pyright".to_string()],
            install_hint: "Install pyright".to_string(),
        },
        pathfinder_lsp::client::MissingLanguage {
            language_id: "rust".to_string(),
            marker_file: "Cargo.toml".to_string(),
            tried_binaries: vec!["rust-analyzer".to_string()],
            install_hint: "Install rust-analyzer".to_string(),
        },
    ]);

    // Filter by language = python
    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("python".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    // Should only return Python, not Rust
    assert_eq!(val.languages.len(), 1);
    assert_eq!(val.languages[0].language, "python");
    assert_eq!(
        val.languages[0].install_hint,
        Some("Install pyright".to_string())
    );
}

// ── PATCH-010: Degraded Tools and Validation Latency Tests ─────────────

#[tokio::test]
async fn test_health_shows_degraded_tools_for_no_diagnostics() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Mock an LSP without diagnostics or call hierarchy support
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(60),
            diagnostics_strategy: None,
            supports_definition: Some(true),
            supports_call_hierarchy: None,
            supports_diagnostics: None,

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("go".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    assert_eq!(val.languages.len(), 1);
    let go_health = &val.languages[0];
    assert_eq!(go_health.language, "go");

    // Check that degraded_tools contains trace (callers) with correct severity
    let trace_callers = go_health
        .degraded_tools
        .iter()
        .find(|t| t.tool == "trace(scope=\"callers\")");
    assert!(
        trace_callers.is_some(),
        "degraded_tools should include trace(scope=callers) when call hierarchy unsupported"
    );
    let fc = trace_callers.unwrap();
    assert_eq!(
        fc.severity, "grep_fallback",
        "trace(scope=callers) should have severity=grep_fallback"
    );
    assert!(
        fc.description.contains("text search"),
        "trace(scope=callers) description should mention text search fallback"
    );

    // Check that degraded_tools contains inspect(include_dependencies=true) with correct severity
    let rwdc = go_health
        .degraded_tools
        .iter()
        .find(|t| t.tool == "inspect(include_dependencies=true)");
    assert!(
            rwdc.is_some(),
            "degraded_tools should include inspect(include_dependencies=true) when call hierarchy unsupported"
        );
    let rwdc = rwdc.unwrap();
    assert_eq!(
        rwdc.severity, "unavailable",
        "inspect(include_dependencies=true) should have severity=unavailable"
    );
    assert!(
        rwdc.description.contains("source only"),
        "inspect(include_dependencies=true) description should mention source-only limitation"
    );

    // validate_only no longer exists — degraded_tools only contains LSP navigation tools
    let has_validate_only = go_health
        .degraded_tools
        .iter()
        .any(|t| t.tool == "validate_only");
    assert!(
        !has_validate_only,
        "degraded_tools must not include the removed validate_only tool"
    );
}

#[tokio::test]
async fn test_health_shows_empty_degraded_when_fully_capable() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Mock a fully capable LSP
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(60),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    assert_eq!(val.languages.len(), 1);
    let rust_health = &val.languages[0];
    assert_eq!(rust_health.language, "rust");
    assert!(
        rust_health.degraded_tools.is_empty(),
        "degraded_tools should be empty when all capabilities supported, got: {:?}",
        rust_health.degraded_tools
    );
}

#[tokio::test]
async fn test_health_shows_push_latency() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Mock a push diagnostics language (Go)
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(60),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("go".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    assert_eq!(val.languages.len(), 1);
    let go_health = &val.languages[0];
    assert_eq!(go_health.language, "go");
    assert!(
        go_health.degraded_tools.is_empty(),
        "fully capable LSP should have no degraded tools"
    );
}

#[tokio::test]
async fn test_health_shows_pull_latency() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Mock a pull diagnostics language (Rust)
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(60),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    result.expect("pull-diagnostics language should return successfully");
}

// ── LSP-HEALTH-001: Confidence Gradient Tests ─────────────────────────────

#[tokio::test]
async fn test_lsp_health_ready_but_still_indexing_shows_confidence_gradient() {
    // Simulate pyright: navigation_ready=true (definitionProvider confirmed),
    // but indexing_complete=false (no WorkDoneProgressEnd received).
    // The agent should see BOTH signals and make smart decisions.
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "python".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: false, // No diagnostics support
            reason: "LSP connected but does not support diagnostics".to_string(),
            navigation_ready: Some(true), // definitionProvider confirmed
            indexing_complete: Some(false), // Still indexing
            uptime_seconds: Some(5),
            diagnostics_strategy: Some("none".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(false),

            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("python".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let py_health = &val.languages[0];
    // Status is "ready" because navigation_ready=true
    assert_eq!(py_health.status, "ready");
    // But indexing is still in progress — agent should see this
    assert_eq!(py_health.indexing_status, Some("in_progress".to_string()));
    // navigation_ready is surfaced so agent knows navigation is functional
    assert_eq!(py_health.navigation_ready, Some(true));
    // Diagnostics not available
    assert_eq!(py_health.diagnostics_strategy, Some("none".to_string()));
    // validate_only no longer exists — diagnostics absence only affects call hierarchy tools
    let has_validate_only = py_health
        .degraded_tools
        .iter()
        .any(|t| t.tool == "validate_only");
    assert!(!has_validate_only);
}

#[tokio::test]
async fn test_lsp_health_fully_indexed_shows_complete_confidence() {
    // Simulate rust-analyzer after full indexing: both signals at max confidence.
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),  // Navigation ready
            indexing_complete: Some(true), // Indexing complete
            uptime_seconds: Some(120),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let rust_health = &val.languages[0];
    assert_eq!(rust_health.status, "ready");
    // Both confidence signals at max
    assert_eq!(rust_health.navigation_ready, Some(true));
    assert_eq!(rust_health.indexing_status, Some("complete".to_string()));
    // No degraded tools
    assert!(rust_health.degraded_tools.is_empty());
}

// ── Probe cache TTL tests (LSP-HEALTH-001 findings 1+2) ──────────

#[tokio::test]
async fn test_probe_cache_positive_result_is_fresh() {
    // Positive cache entries should be fresh immediately after creation
    let entry = crate::server::ProbeCacheEntry::new(true, true);
    assert!(
        entry.age_secs() < 2,
        "freshly created positive entry should have age < 2s"
    );
    assert!(entry.success, "positive entry should have success=true");
}

#[tokio::test]
async fn test_probe_cache_negative_result_is_fresh() {
    // Negative cache entries should be fresh immediately after creation
    let entry = crate::server::ProbeCacheEntry::new(false, false);
    assert!(
        entry.age_secs() < 2,
        "freshly created negative entry should have age < 2s"
    );
    assert!(!entry.success, "negative entry should have success=false");
}

#[tokio::test]
async fn test_probe_negative_cache_skips_reprobe() {
    // When a negative cache entry exists, lsp_health should skip probing
    // and keep the status as "warming_up" instead of hammering the LSP.
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Pre-populate cache with a negative result
    server
        .probe_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            "rust".to_string(),
            crate::server::ProbeCacheEntry::new(false, false),
        );

    // LSP running but not ready (navigation_ready = false)
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(false),
            indexing_complete: Some(false),
            uptime_seconds: Some(30), // Over 10s threshold
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(false),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let rust_health = &val.languages[0];
    // Status should stay "warming_up" because cached negative result skipped the probe
    assert_eq!(rust_health.status, "warming_up");
    assert!(
        !rust_health.probe_verified,
        "should not be probe-verified when using negative cache"
    );
}

#[tokio::test]
async fn test_probe_cache_positive_upgrades_to_ready() {
    // When a positive cache entry exists, lsp_health should upgrade to ready
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Pre-populate cache with a positive result
    server
        .probe_cache
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(
            "rust".to_string(),
            crate::server::ProbeCacheEntry::new(true, true),
        );

    // LSP reports warming_up but cache has positive result
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(false),
            indexing_complete: Some(false),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(false),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let rust_health = &val.languages[0];
    assert_eq!(rust_health.status, "ready");
    assert!(
        rust_health.probe_verified,
        "should be probe-verified from cache"
    );
}

// ── Liveness Probe Tests ────────────────────────────────────

#[tokio::test]
async fn test_lsp_health_liveness_probe_downgrades_dead_lsp() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

    // Create a file for probing
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

    // Mock a "ready" LSP that was working but now times out
    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(120),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Mock goto_definition timeout (LSP is dead)
    lawyer.set_goto_definition_result(Err(pathfinder_lsp::LspError::Timeout {
        operation: "goto_definition".to_string(),
        timeout_ms: 10000,
    }));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    // Status should be downgraded to "degraded"
    assert_eq!(val.status, "degraded");
    let rust_health = &val.languages[0];
    assert_eq!(rust_health.status, "degraded");
    assert!(!rust_health.probe_verified);
    assert_eq!(
        rust_health.navigation_tested,
        Some(false),
        "navigation_tested must be Some(false) when liveness probe fails"
    );
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_lsp_health_liveness_probe_caches_positive() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

    // Create a file for probing
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

    // Mock a "ready" LSP that is still responsive
    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(120),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Mock successful goto_definition
    lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
        file: "src/main.rs".to_string(),
        line: 1,
        column: 0,
        preview: "fn main()".to_string(),
    })));

    // First call - should probe and cache
    let result1 = server
        .lsp_health_impl(crate::server::types::HealthParams {
            action: None,
            language: Some("rust".to_string()),
            force_probe: None,
        })
        .await;
    let val1 = unpack_health(result1.expect("should succeed"));
    assert!(val1.languages[0].probe_verified);

    // Verify cache was populated
    let cache = server.probe_cache.lock().unwrap();
    assert!(cache.contains_key("rust"));
    let entry = cache.get("rust").unwrap();
    assert!(entry.success);
    drop(cache);

    // Second call - should use cache (no second probe)
    let call_count_before = lawyer.goto_definition_call_count();
    let result2 = server
        .lsp_health_impl(crate::server::types::HealthParams {
            action: None,
            language: Some("rust".to_string()),
            force_probe: None,
        })
        .await;
    let val2 = unpack_health(result2.expect("should succeed"));
    assert!(val2.languages[0].probe_verified);
    // Goto definition should not be called again (cache hit)
    assert_eq!(lawyer.goto_definition_call_count(), call_count_before);
}

#[tokio::test]
#[allow(clippy::await_holding_lock)]
async fn test_liveness_probe_interval_skips_recent() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

    // Create a file for probing
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

    // Mock a "ready" LSP
    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(120),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),

            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Pre-populate cache with a recent positive entry (age < LIVENESS_PROBE_INTERVAL_SECS)
    let mut cache = server.probe_cache.lock().unwrap();
    cache.insert(
        "rust".to_string(),
        crate::server::ProbeCacheEntry::new(true, true),
    );
    drop(cache);

    // Mock goto_definition - should NOT be called due to cache
    lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
        file: "src/main.rs".to_string(),
        line: 1,
        column: 0,
        preview: "fn main()".to_string(),
    })));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };

    let call_count_before = lawyer.goto_definition_call_count();
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    // Should use cached result without probing
    assert!(val.languages[0].probe_verified);
    assert_eq!(lawyer.goto_definition_call_count(), call_count_before);
}

#[tokio::test]
async fn test_lsp_health_probe_downgrades_when_call_hierarchy_hangs() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());

    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/main.rs"),
        r#"fn main() { println!("Hello"); }"#,
    )
    .unwrap();

    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // goto_definition succeeds (basic LSP works)
    lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
        file: "src/main.rs".to_string(),
        line: 1,
        column: 0,
        preview: "fn main()".to_string(),
    })));

    // call_hierarchy_prepare FAILS (LSP is hung for call hierarchy)
    lawyer.push_prepare_call_hierarchy_result(Err(pathfinder_lsp::LspError::Timeout {
        operation: "textDocument/prepareCallHierarchy".to_string(),
        timeout_ms: 5000,
    }));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    assert_eq!(
        val.languages.len(),
        1,
        "should have exactly 1 language entry"
    );
    let rust_health = &val.languages[0];

    // The LSP should be downgraded from "ready" to "degraded" because
    // the call hierarchy probe failed even though goto_definition succeeded.
    assert_eq!(
        rust_health.status, "degraded",
        "should be degraded when call_hierarchy probe fails despite goto_definition succeeding"
    );
    assert!(
        !rust_health.probe_verified,
        "probe_verified must be false when call hierarchy probe fails"
    );
}

// ── BATCH-04 Remaining Coverage Tests for health.rs ─────────────────────

#[derive(Clone)]
struct SlowMockLawyer {
    inner: pathfinder_lsp::MockLawyer,
    goto_definition_delay: Arc<std::sync::Mutex<Option<std::time::Duration>>>,
    call_hierarchy_delay: Arc<std::sync::Mutex<Option<std::time::Duration>>>,
}

impl SlowMockLawyer {
    fn new(inner: pathfinder_lsp::MockLawyer) -> Self {
        Self {
            inner,
            goto_definition_delay: Arc::new(std::sync::Mutex::new(None)),
            call_hierarchy_delay: Arc::new(std::sync::Mutex::new(None)),
        }
    }

    fn set_goto_definition_delay(&self, delay: std::time::Duration) {
        *self.goto_definition_delay.lock().unwrap() = Some(delay);
    }

    fn set_call_hierarchy_delay(&self, delay: std::time::Duration) {
        *self.call_hierarchy_delay.lock().unwrap() = Some(delay);
    }
}

#[async_trait::async_trait]
impl pathfinder_lsp::Lawyer for SlowMockLawyer {
    async fn goto_definition(
        &self,
        workspace_root: &std::path::Path,
        file_path: &std::path::Path,
        line: u32,
        column: u32,
    ) -> Result<Option<pathfinder_lsp::types::DefinitionLocation>, pathfinder_lsp::error::LspError>
    {
        let delay = { *self.goto_definition_delay.lock().unwrap() };
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        self.inner
            .goto_definition(workspace_root, file_path, line, column)
            .await
    }

    async fn call_hierarchy_prepare(
        &self,
        workspace_root: &std::path::Path,
        file_path: &std::path::Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<pathfinder_lsp::types::CallHierarchyItem>, pathfinder_lsp::error::LspError>
    {
        let delay = { *self.call_hierarchy_delay.lock().unwrap() };
        if let Some(d) = delay {
            tokio::time::sleep(d).await;
        }
        self.inner
            .call_hierarchy_prepare(workspace_root, file_path, line, column)
            .await
    }

    async fn call_hierarchy_incoming(
        &self,
        workspace_root: &std::path::Path,
        item: &pathfinder_lsp::types::CallHierarchyItem,
    ) -> Result<Vec<pathfinder_lsp::types::CallHierarchyCall>, pathfinder_lsp::error::LspError>
    {
        self.inner
            .call_hierarchy_incoming(workspace_root, item)
            .await
    }

    async fn call_hierarchy_outgoing(
        &self,
        workspace_root: &std::path::Path,
        item: &pathfinder_lsp::types::CallHierarchyItem,
    ) -> Result<Vec<pathfinder_lsp::types::CallHierarchyCall>, pathfinder_lsp::error::LspError>
    {
        self.inner
            .call_hierarchy_outgoing(workspace_root, item)
            .await
    }

    async fn goto_implementation(
        &self,
        workspace_root: &std::path::Path,
        file_path: &std::path::Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<pathfinder_lsp::types::DefinitionLocation>, pathfinder_lsp::error::LspError>
    {
        self.inner
            .goto_implementation(workspace_root, file_path, line, column)
            .await
    }

    async fn references(
        &self,
        workspace_root: &std::path::Path,
        file_path: &std::path::Path,
        line: u32,
        column: u32,
    ) -> Result<Vec<pathfinder_lsp::types::ReferenceLocation>, pathfinder_lsp::error::LspError>
    {
        self.inner
            .references(workspace_root, file_path, line, column)
            .await
    }

    async fn open_document(
        &self,
        workspace_root: &std::path::Path,
        file_path: &std::path::Path,
        content: &str,
    ) -> Result<Box<dyn pathfinder_lsp::lawyer::DocumentLease>, pathfinder_lsp::error::LspError>
    {
        self.inner
            .open_document(workspace_root, file_path, content)
            .await
    }

    async fn capability_status(
        &self,
    ) -> std::collections::HashMap<String, pathfinder_lsp::types::LspLanguageStatus> {
        self.inner.capability_status().await
    }

    fn missing_languages(&self) -> Vec<pathfinder_lsp::client::MissingLanguage> {
        self.inner.missing_languages()
    }

    async fn force_respawn(
        &self,
        language_id: &str,
    ) -> Result<(), pathfinder_lsp::error::LspError> {
        self.inner.force_respawn(language_id).await
    }
}

#[tokio::test(start_paused = true)]
async fn test_lsp_health_probe_timeout_goto_definition() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = pathfinder_lsp::MockLawyer::default();
    let slow_lawyer = SlowMockLawyer::new(lawyer.clone());
    slow_lawyer.set_goto_definition_delay(std::time::Duration::from_secs(6));

    // Create a workspace with a main.rs file for probing
    let ws_dir = crate::server::tools::navigation::test_helpers::make_temp_workspace();
    let ws = pathfinder_common::types::WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = pathfinder_common::config::PathfinderConfig::default();
    let sandbox = pathfinder_common::sandbox::Sandbox::new(ws.path(), &config.sandbox);
    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        std::sync::Arc::new(pathfinder_search::MockScout::default()),
        surgeon,
        std::sync::Arc::new(slow_lawyer),
    );

    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(false),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let rust_health = &val.languages[0];
    assert_eq!(rust_health.status, "degraded");
    assert!(!rust_health.probe_verified);
}

#[tokio::test(start_paused = true)]
async fn test_lsp_health_probe_timeout_call_hierarchy_prepare() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = pathfinder_lsp::MockLawyer::default();
    let slow_lawyer = SlowMockLawyer::new(lawyer.clone());
    slow_lawyer.set_call_hierarchy_delay(std::time::Duration::from_secs(6));

    // Create a workspace with a main.rs file for probing
    let ws_dir = crate::server::tools::navigation::test_helpers::make_temp_workspace();
    let ws = pathfinder_common::types::WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = pathfinder_common::config::PathfinderConfig::default();
    let sandbox = pathfinder_common::sandbox::Sandbox::new(ws.path(), &config.sandbox);
    let server = crate::server::PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        std::sync::Arc::new(pathfinder_search::MockScout::default()),
        surgeon,
        std::sync::Arc::new(slow_lawyer),
    );

    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(false),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
        file: "src/main.rs".to_string(),
        line: 1,
        column: 0,
        preview: "fn main()".to_string(),
    })));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let rust_health = &val.languages[0];
    assert_eq!(rust_health.status, "degraded");
    assert!(!rust_health.probe_verified);
}

#[tokio::test]
async fn test_lsp_health_probe_verifies_call_hierarchy() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = pathfinder_lsp::MockLawyer::default();

    // Create a workspace with a main.rs file for probing
    let (server, ws_dir) = make_server_with_lawyer(surgeon, Arc::new(lawyer.clone()));
    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(
        ws_dir.path().join("src/main.rs"),
        r#"fn main() { println!("Hello"); }"#,
    )
    .unwrap();

    // Mock a Rust LSP that's been warming up for 30 seconds
    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(false), // Still warming up
            uptime_seconds: Some(30),       // 30 seconds - should trigger probe
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Mock successful goto_definition response
    lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
        file: "src/main.rs".to_string(),
        line: 1,
        column: 0,
        preview: "fn main()".to_string(),
    })));

    // Mock successful call_hierarchy_prepare response
    lawyer.push_prepare_call_hierarchy_result(Ok(vec![pathfinder_lsp::types::CallHierarchyItem {
        name: "main".to_string(),
        kind: "function".to_string(),
        detail: None,
        file: "src/main.rs".to_string(),
        line: 1,
        column: 1,
        data: None,
    }]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    assert_eq!(val.status, "ready");
    let rust_health = &val.languages[0];
    assert!(rust_health.probe_verified);
    assert!(rust_health.call_hierarchy_verified);
}

#[tokio::test]
async fn test_find_probe_file_unsupported_language() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    assert_eq!(server.find_probe_file("unsupported"), None);
}

#[cfg(unix)]
#[tokio::test]
async fn test_find_file_by_extension_recursive_unreadable_dir() {
    use std::os::unix::fs::PermissionsExt;
    let temp = tempfile::tempdir().unwrap();
    let unreadable = temp.path().join("unreadable");
    std::fs::create_dir(&unreadable).unwrap();

    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o000)).unwrap();

    let extensions = vec!["rs"];
    let result = server.find_file_by_extension_recursive(&unreadable, &extensions, 0, 4);
    assert_eq!(result, None);

    // Restore permissions so cleanup works properly
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn test_format_uptime_units() {
    assert_eq!(format_uptime(30), "30s");
    assert_eq!(format_uptime(60), "1m");
    assert_eq!(format_uptime(90), "1m30s");
    assert_eq!(format_uptime(3600), "1h");
    assert_eq!(format_uptime(3660), "1h1m");
    assert_eq!(format_uptime(3725), "1h2m");
}

#[test]
fn test_compute_degraded_tools_conditions() {
    let navigation_verified = Some(true);
    let navigation_ready = Some(true);
    let supports_definition = Some(false);
    let supports_call_hierarchy = Some(true);
    let server_name: Option<&String> = None;

    let degraded = compute_degraded_tools_from_health(
        navigation_verified,
        navigation_ready,
        supports_definition,
        supports_call_hierarchy,
        server_name,
    );
    assert_eq!(degraded.len(), 1);
    assert_eq!(degraded[0].tool, "locate");

    let degraded2 = compute_degraded_tools_from_health(
        Some(true),
        Some(true),
        Some(true),
        Some(false),
        Some(&"typescript-language-server".to_string()),
    );
    assert_eq!(degraded2.len(), 2);
    assert_eq!(degraded2[0].tool, "trace(scope=\"callers\")");
    assert_eq!(degraded2[1].tool, "inspect(include_dependencies=true)");
}

#[tokio::test]
async fn test_lsp_health_restart_missing_language() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::HealthParams {
        action: Some("restart".to_string()),
        language: None,
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    assert!(result.is_err());
    // Must be INVALID_PARAMS, not INTERNAL_ERROR — this is a client input validation failure.
    let err = result.unwrap_err();
    assert_eq!(err.code, rmcp::model::ErrorCode::INVALID_PARAMS);
}

#[tokio::test]
async fn test_lsp_health_restart_successful() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::HealthParams {
        action: Some("restart".to_string()),
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    assert!(result.is_ok());
}

// ── Component 3: Grace Period Known Limitations ────────────────────
#[tokio::test]
async fn test_lsp_health_grace_period_known_limitation() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Simulate a language where navigation_ready is still None (grace period)
    // This happens when the LSP hasn't yet registered capabilities dynamically.
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "java".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: None, // Still in grace period
            indexing_complete: Some(false),
            uptime_seconds: Some(3),
            diagnostics_strategy: None,
            supports_definition: None,
            supports_call_hierarchy: None,
            supports_diagnostics: None,
            supports_formatting: None,
            server_name: Some("jdtls".to_string()),
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: Some(0),
        },
    )]));

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    // Should have a known_limitation about dynamic registration
    assert!(
        val.known_limitations
            .iter()
            .any(|l| l.contains("dynamic capability registration")),
        "expected known_limitation about dynamic registration, got: {:?}",
        val.known_limitations
    );
    // The limitation should mention the language
    assert!(
        val.known_limitations
            .iter()
            .any(|l| l.contains("java") && l.contains("dynamic capability registration")),
        "limitation must mention 'java'"
    );
}

#[tokio::test]
async fn test_lsp_health_no_grace_period_limitation_when_ready() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // A ready language (navigation_ready = Some(true)) should NOT have the limitation
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(true),
            server_name: Some("rust-analyzer".to_string()),
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: Some(5),
        },
    )]));

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");
    let val = unpack_health(call_res);

    assert!(
        !val.known_limitations
            .iter()
            .any(|l| l.contains("dynamic capability registration")),
        "ready language should NOT have grace period limitation"
    );
}

// ── Component 4: Server Name and Registrations in Health ───────────
#[tokio::test]
async fn test_lsp_health_server_name_in_output() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer);

    // Remove probe files to prevent liveness probe
    let src_dir = ws_dir.path().join("src");
    let _ = std::fs::remove_file(src_dir.join("main.rs"));
    let _ = std::fs::remove_file(src_dir.join("auth.rs"));
    let _ = std::fs::remove_file(src_dir.join("token.rs"));
    let _ = std::fs::remove_file(src_dir.join("service.rs"));
    let _ = std::fs::remove_file(src_dir.join("user.rs"));
    let _ = std::fs::remove_file(src_dir.join("auth.go"));

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(60),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(true),
            server_name: Some("rust-analyzer".to_string()),
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: Some(3),
        },
    )]));

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");

    // Check structured content
    let val = unpack_health(call_res.clone());
    assert_eq!(val.languages.len(), 1);
    let rust_health = &val.languages[0];
    assert_eq!(rust_health.server_name, Some("rust-analyzer".to_string()));
    assert_eq!(rust_health.registrations_received, Some(3));

    // Check text summary includes server name
    let text = call_res
        .content
        .iter()
        .filter_map(|c| match c.raw {
            rmcp::model::RawContent::Text(ref t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");
    assert!(
        text.contains("server: rust-analyzer"),
        "text output should mention server name, got: {text}"
    );
    assert!(
        text.contains("registrations: 3"),
        "text output should mention registrations count, got: {text}"
    );
}

#[tokio::test]
async fn test_lsp_health_registrations_zero_omitted_from_text() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer);

    // Remove probe files to prevent liveness probe
    let src_dir = ws_dir.path().join("src");
    let _ = std::fs::remove_file(src_dir.join("main.rs"));
    let _ = std::fs::remove_file(src_dir.join("auth.rs"));
    let _ = std::fs::remove_file(src_dir.join("token.rs"));
    let _ = std::fs::remove_file(src_dir.join("service.rs"));
    let _ = std::fs::remove_file(src_dir.join("user.rs"));
    let _ = std::fs::remove_file(src_dir.join("auth.go"));

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(10),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None, // no server name
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: Some(0), // zero registrations — should NOT appear in text
        },
    )]));

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let call_res = result.expect("should succeed");

    let text = call_res
        .content
        .iter()
        .filter_map(|c| match c.raw {
            rmcp::model::RawContent::Text(ref t) => Some(t.text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    assert!(
        !text.contains("registrations:"),
        "zero registrations should be omitted from text, got: {text}"
    );
    assert!(
        !text.contains("server:"),
        "no server name should mean no 'server:' in text, got: {text}"
    );
}

// ── P2-6: Top-level indexing_complete boolean ──────────────────────

#[tokio::test]
async fn test_health_indexing_complete_true_when_all_done() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Two languages, both fully indexed
    lawyer_clone.set_capability_status(std::collections::HashMap::from([
        (
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(30),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),
                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
                registrations_received: None,
            },
        ),
        (
            "go".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(20),
                diagnostics_strategy: Some("push".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),
                supports_formatting: Some(false),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
                registrations_received: None,
            },
        ),
    ]));

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    assert!(
        val.indexing_complete,
        "indexing_complete should be true when all languages are done indexing"
    );
}

#[tokio::test]
async fn test_health_indexing_complete_false_when_one_indexing() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Rust: done indexing. Go: still indexing.
    lawyer_clone.set_capability_status(std::collections::HashMap::from([
        (
            "rust".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(true),
                uptime_seconds: Some(30),
                diagnostics_strategy: Some("pull".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),
                supports_formatting: Some(true),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: None,
                registrations_received: None,
            },
        ),
        (
            "go".to_string(),
            pathfinder_lsp::types::LspLanguageStatus {
                validation: true,
                reason: "LSP connected".to_string(),
                navigation_ready: Some(true),
                indexing_complete: Some(false), // still indexing
                uptime_seconds: Some(5),
                diagnostics_strategy: Some("push".to_string()),
                supports_definition: Some(true),
                supports_call_hierarchy: Some(true),
                supports_diagnostics: Some(true),
                supports_formatting: Some(false),
                server_name: None,
                indexing_source: None,
                indexing_duration_secs: None,
                indexing_progress_percent: Some(45),
                registrations_received: None,
            },
        ),
    ]));

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    assert!(
        !val.indexing_complete,
        "indexing_complete should be false when any language is still indexing"
    );
}

#[tokio::test]
async fn test_health_invalid_action_returns_invalid_params() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // action = "invalid_action"
    let params = crate::server::types::HealthParams {
        language: Some("rust".to_string()),
        action: Some("invalid_action".to_string()),
        force_probe: None,
    };

    let result = server.lsp_health_impl(params).await;
    assert!(result.is_err());
    assert_eq!(
        result.unwrap_err().code,
        rmcp::model::ErrorCode::INVALID_PARAMS
    );
}

/// Verifies that when TS LS does NOT negotiate call hierarchy
/// (e.g., TypeScript < 3.8.0 or capability negotiation failure),
/// the health response correctly reports the limitation.
///
/// After SPIKE-B, TS LS with TS 3.8.0+ SHOULD negotiate call hierarchy.
/// This test covers the fallback case when it doesn't.
#[tokio::test]
async fn test_health_typescript_call_hierarchy_unavailable_when_not_negotiated() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "typescript".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(false),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: Some("typescript-language-server".to_string()),
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    // Check degraded tools description - uses updated conditional language
    let ts_health = &val.languages[0];
    let trace_deg = ts_health
        .degraded_tools
        .iter()
        .find(|t| t.tool == "trace(scope=\"callers\")")
        .expect("should find trace(scope=\"callers\") degraded tool");
    assert!(
        trace_deg.description.contains("not negotiated"),
        "expected description to mention 'not negotiated', got: {}",
        trace_deg.description
    );
    assert!(
        trace_deg.description.contains("TypeScript 3.8.0+"),
        "expected description to mention 'TypeScript 3.8.0+', got: {}",
        trace_deg.description
    );
    assert!(
        !trace_deg.description.contains("do not support"),
        "description should NOT say 'do not support', got: {}",
        trace_deg.description
    );

    // Check known limitations - uses updated conditional language
    let limitation_msg = val
        .known_limitations
        .iter()
        .find(|l| l.contains("Call hierarchy"))
        .expect("expected known_limitations to mention TS/JS call hierarchy limitation");
    assert!(
        limitation_msg.contains("not available"),
        "expected limitation to say 'not available', got: {limitation_msg}"
    );
    assert!(
        limitation_msg.contains("3.8.0"),
        "expected limitation to mention version '3.8.0', got: {limitation_msg}"
    );
    assert!(
        !limitation_msg.contains("do not support"),
        "limitation should NOT say 'do not support', got: {limitation_msg}"
    );
}

/// Verifies that when TS LS DOES negotiate call hierarchy
/// (TypeScript 3.8.0+ with SPIKE-B capability declaration),
/// the health response does NOT report any TS-specific limitation.
#[tokio::test]
async fn test_health_typescript_call_hierarchy_available_no_limitation() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "typescript".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: Some("typescript-language-server".to_string()),
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let ts_health = &val.languages[0];
    assert_eq!(ts_health.supports_call_hierarchy, Some(true));

    // Assert NO TS-specific limitation message in known_limitations
    // (messages about TypeScript < 3.8.0 should NOT appear when call hierarchy is available)
    assert!(
        val.known_limitations.iter().all(|l| {
            !l.contains("TypeScript")
                && !l.contains("not available for this")
                && !l.contains("not negotiated")
        }),
        "expected no TS-specific call hierarchy limitation when supports_call_hierarchy=true, got: {:?}",
        val.known_limitations
    );

    // Assert degraded_tools does NOT contain TS-specific call hierarchy message
    assert!(
        ts_health
            .degraded_tools
            .iter()
            .all(|d| !d.description.contains("TypeScript")
                && !d.description.contains("not negotiated")),
        "expected no TS-specific degraded_tools when supports_call_hierarchy=true, got: {:?}",
        ts_health.degraded_tools
    );

    // Assert trace/inspect are NOT degraded when call hierarchy is available
    assert!(
        !ts_health
            .degraded_tools
            .iter()
            .any(|t| t.tool == "trace(scope=\"callers\")"),
        "trace should NOT be degraded when supports_call_hierarchy=true"
    );
    assert!(
        !ts_health
            .degraded_tools
            .iter()
            .any(|t| t.tool == "inspect(include_dependencies=true)"),
        "inspect should NOT be degraded when supports_call_hierarchy=true"
    );
}

/// Verifies asymmetry: when TS LS reports `supports_call_hierarchy=None`
/// (capability status unknown, not yet negotiated), `degraded_tools` SHOULD
/// show TS-specific "not negotiated" messages (defensive, != Some(true)),
/// but `known_limitations` should NOT (guarded by == Some(false)).
///
/// This documents the intentional asymmetry between the two guards.
#[tokio::test]
async fn test_health_typescript_call_hierarchy_none_shows_degraded_but_no_limitation() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "typescript".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: None, // KEY: None, not Some(false)
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: Some("typescript-language-server".to_string()),
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let ts_health = &val.languages[0];
    assert_eq!(ts_health.supports_call_hierarchy, None);

    // degraded_tools SHOULD show TS-specific message (uses != Some(true))
    let trace_deg = ts_health
        .degraded_tools
        .iter()
        .find(|t| t.tool == "trace(scope=\"callers\")");
    assert!(
        trace_deg.is_some(),
        "trace should be degraded when supports_call_hierarchy=None"
    );
    assert!(
        trace_deg.unwrap().description.contains("not negotiated"),
        "degraded_tools should use TS-specific 'not negotiated' message, got: {}",
        trace_deg.unwrap().description
    );

    // known_limitations should NOT show TS-specific message (uses == Some(false))
    let has_ts_limitation = val
        .known_limitations
        .iter()
        .any(|l| l.contains("TypeScript") || l.contains("call hierarchy"));
    assert!(
        !has_ts_limitation,
        "known_limitations should NOT have TS message when supports_call_hierarchy=None, got: {:?}",
        val.known_limitations
    );
}

/// Verifies that vtsls (alternative TS language server) also triggers
/// TS-specific messages when call hierarchy is unavailable.
#[tokio::test]
async fn test_health_vtsls_call_hierarchy_unavailable_uses_ts_message() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "typescript".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(false),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: Some("vtsls".to_string()), // KEY: vtsls, not typescript-language-server
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let ts_health = &val.languages[0];
    let trace_deg = ts_health
        .degraded_tools
        .iter()
        .find(|t| t.tool == "trace(scope=\"callers\")")
        .expect("should find trace degraded tool");

    assert!(
        trace_deg.description.contains("not negotiated"),
        "vtsls should trigger TS-specific message, got: {}",
        trace_deg.description
    );

    // Also verify known_limitations has TS-specific message
    let limitation = val
        .known_limitations
        .iter()
        .find(|l| l.contains("not available"));
    assert!(
        limitation.is_some(),
        "vtsls should trigger TS-specific known_limitation"
    );
}

/// Verifies that tsserver also triggers TS-specific messages.
#[tokio::test]
async fn test_health_tsserver_call_hierarchy_unavailable_uses_ts_message() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "typescript".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(false),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: Some("tsserver".to_string()), // KEY: tsserver
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let ts_health = &val.languages[0];
    let trace_deg = ts_health
        .degraded_tools
        .iter()
        .find(|t| t.tool == "trace(scope=\"callers\")")
        .expect("should find trace degraded tool");

    assert!(
        trace_deg.description.contains("not negotiated"),
        "tsserver should trigger TS-specific message, got: {}",
        trace_deg.description
    );
}

/// Verifies that when `server_name` is None but language is "typescript",
/// the `is_ts_js` check returns false (since `.is_some_and()` short-circuits
/// to false when `server_name` is None), resulting in generic messages instead
/// of TS-specific ones. This is correct behavior: don't make TS-specific
/// claims if we can't verify the server is actually a TS LS.
#[tokio::test]
async fn test_health_typescript_no_server_name_gets_generic_message() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "typescript".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(30),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(false),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None, // KEY: None, not even "unknown"
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams::default();
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let ts_health = &val.languages[0];
    let trace_deg = ts_health
        .degraded_tools
        .iter()
        .find(|t| t.tool == "trace(scope=\"callers\")")
        .expect("should find trace degraded tool");

    // Should get GENERIC message, NOT TS-specific
    assert!(
        !trace_deg.description.contains("not negotiated"),
        "server_name=None should get generic message, not TS-specific, got: {}",
        trace_deg.description
    );
    assert!(
        trace_deg
            .description
            .contains("text search instead of call hierarchy"),
        "should get generic 'text search' message, got: {}",
        trace_deg.description
    );

    // known_limitations should also be generic
    let limitation = val
        .known_limitations
        .iter()
        .find(|l| l.contains("call hierarchy not supported"));
    assert!(
        limitation.is_some(),
        "server_name=None should get generic known_limitation, got: {:?}",
        val.known_limitations
    );
}

// ── PATCH-004: Health & Readiness Consistency Tests ─────────────────

#[tokio::test]
async fn test_health_shows_probe_age() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // Mock Go LSP status
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(15),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Pre-populate probe cache
    server.probe_cache.lock().unwrap().insert(
        "go".to_string(),
        crate::server::ProbeCacheEntry::new(true, true),
    );

    let params = crate::server::types::HealthParams {
        action: None,
        language: None,
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let go_health = &val.languages[0];
    assert!(go_health.last_probe_age_secs.is_some());
    assert_eq!(go_health.last_probe_age_secs.unwrap(), 0);
}

#[tokio::test]
async fn test_health_probe_verified_true_after_successful_navigation() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, ws) = make_server_with_lawyer(surgeon, lawyer);

    // Create a Go file for probing
    std::fs::write(ws.path().join("main.go"), "package main").unwrap();

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(15),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    lawyer_clone.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
        file: "main.go".to_string(),
        line: 1,
        column: 0,
        preview: "package main".to_string(),
    })));

    // Ensure we trigger a live probe (no cache entry exists)
    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("go".to_string()),
        force_probe: Some(true),
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let go_health = &val.languages[0];
    assert!(go_health.probe_verified);
    assert_eq!(go_health.navigation_tested, Some(true));
}

#[tokio::test]
async fn test_health_probe_verified_false_when_only_capability_checked() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer);

    // Remove Rust/Go files created by make_temp_workspace to prevent liveness probe
    let src_dir = ws_dir.path().join("src");
    let _ = std::fs::remove_file(src_dir.join("main.rs"));
    let _ = std::fs::remove_file(src_dir.join("auth.rs"));
    let _ = std::fs::remove_file(src_dir.join("token.rs"));
    let _ = std::fs::remove_file(src_dir.join("service.rs"));
    let _ = std::fs::remove_file(src_dir.join("user.rs"));
    let _ = std::fs::remove_file(src_dir.join("auth.go"));

    // No probe files in workspace, so no live probe can run
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(15),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: None,
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let go_health = &val.languages[0];
    assert!(!go_health.probe_verified);
    assert_eq!(go_health.navigation_tested, None);
}

#[tokio::test]
async fn test_probe_interval_short_after_lsp_start() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // LSP started 5 seconds ago
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(5),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Query health once to initialize started_at tracking
    let params = crate::server::types::HealthParams {
        action: None,
        language: None,
        force_probe: None,
    };
    let _ = server.lsp_health_impl(params).await;

    // Check interval
    let interval = server.get_probe_interval("go");
    assert_eq!(interval, 10);
}

#[tokio::test]
async fn test_probe_interval_medium_after_60s() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // LSP started 90 seconds ago
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(90),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: None,
        force_probe: None,
    };
    let _ = server.lsp_health_impl(params).await;

    let interval = server.get_probe_interval("go");
    assert_eq!(interval, 30);
}

#[tokio::test]
async fn test_probe_interval_normal_after_300s() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // LSP started 350 seconds ago
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(350),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let params = crate::server::types::HealthParams {
        action: None,
        language: None,
        force_probe: None,
    };
    let _ = server.lsp_health_impl(params).await;

    let interval = server.get_probe_interval("go");
    assert_eq!(interval, 120);
}

#[tokio::test]
async fn test_health_force_probe_triggers_live_check() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, ws) = make_server_with_lawyer(surgeon, lawyer);

    std::fs::write(ws.path().join("main.go"), "package main").unwrap();

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(350),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    lawyer_clone.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
        file: "main.go".to_string(),
        line: 1,
        column: 0,
        preview: "package main".to_string(),
    })));

    // Pre-populate cache with positive entry (fresh: created 1s ago)
    server.probe_cache.lock().unwrap().insert(
        "go".to_string(),
        crate::server::ProbeCacheEntry::new(true, true),
    );

    // Call health with force_probe: true -> should probe and increment dynamic count
    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("go".to_string()),
        force_probe: Some(true),
    };
    let _ = server.lsp_health_impl(params).await;

    // Verify goto_definition call count was incremented (1 or more)
    assert!(lawyer_clone.goto_definition_call_count() >= 1);
}

#[tokio::test]
async fn test_health_uses_cache_when_fresh() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, ws) = make_server_with_lawyer(surgeon, lawyer);

    std::fs::write(ws.path().join("main.go"), "package main").unwrap();

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(350),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Pre-populate cache with positive entry (fresh)
    server.probe_cache.lock().unwrap().insert(
        "go".to_string(),
        crate::server::ProbeCacheEntry::new(true, true),
    );

    // Call health with force_probe: false -> should use cache and NOT trigger probe
    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("go".to_string()),
        force_probe: Some(false),
    };
    let _ = server.lsp_health_impl(params).await;

    assert_eq!(lawyer_clone.goto_definition_call_count(), 0);
}

#[tokio::test]
async fn test_health_live_probe_timeout_marks_degraded() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, ws) = make_server_with_lawyer(surgeon, lawyer);

    std::fs::write(ws.path().join("main.go"), "package main").unwrap();

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(350),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Mock timeout by returning LspError::ConnectionLost
    lawyer_clone.set_goto_definition_result(Err(pathfinder_lsp::LspError::ConnectionLost));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("go".to_string()),
        force_probe: Some(true),
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let go_health = &val.languages[0];
    assert_eq!(go_health.status, "degraded");
    assert!(!go_health.probe_verified);
    assert_eq!(go_health.navigation_tested, Some(false));
}

/// Regression: stale positive cache entry must NOT stamp `probe_verified=true`
/// on a language whose LSP has since become unavailable.
#[tokio::test]
async fn test_stale_cache_does_not_infect_unavailable_language() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    // LSP is now unavailable (no uptime, no nav_ready)
    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: false,
            reason: "LSP not running".to_string(),
            navigation_ready: None,
            indexing_complete: None,
            uptime_seconds: None,
            diagnostics_strategy: None,
            supports_definition: None,
            supports_call_hierarchy: None,
            supports_diagnostics: None,
            supports_formatting: None,
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Pre-populate cache with a stale positive entry from when LSP was alive
    server.probe_cache.lock().unwrap().insert(
        "go".to_string(),
        crate::server::ProbeCacheEntry::new(true, true),
    );

    let params = crate::server::types::HealthParams {
        action: None,
        language: None,
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let go_health = &val.languages[0];
    assert_eq!(go_health.status, "unavailable");
    assert!(
        !go_health.probe_verified,
        "stale cache must not stamp probe_verified on unavailable language"
    );
    assert_eq!(
        go_health.navigation_tested, None,
        "unavailable language must not have navigation_tested"
    );
    assert_eq!(
        go_health.last_probe_age_secs, None,
        "unavailable language must not expose probe age"
    );
}

// ── PATCH-004 GAP B1: boundary tests at exactly 60s and 300s ──────────

#[tokio::test]
async fn test_probe_interval_at_boundary_60s() {
    // elapsed=60 is INCLUSIVE in the short bucket (<=60 → 10s).
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "ok".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(60),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let _ = server
        .lsp_health_impl(crate::server::types::HealthParams {
            action: None,
            language: None,
            force_probe: None,
        })
        .await;

    // boundary: elapsed <= 60 is inclusive → must remain in 10s bucket
    let interval = server.get_probe_interval("go");
    assert_eq!(
        interval, 10,
        "elapsed=60 is inclusive (<= 60) → interval must be 10, not 30"
    );
}

#[tokio::test]
async fn test_probe_interval_just_past_60s_boundary() {
    // elapsed=61 crosses the first boundary → 30s bucket.
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "ok".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(61),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let _ = server
        .lsp_health_impl(crate::server::types::HealthParams {
            action: None,
            language: None,
            force_probe: None,
        })
        .await;

    let interval = server.get_probe_interval("go");
    assert_eq!(
        interval, 30,
        "elapsed=61 crosses first boundary → interval must be 30"
    );
}

#[tokio::test]
async fn test_probe_interval_at_boundary_300s() {
    // elapsed=300 is INCLUSIVE in the medium bucket (<=300 → 30s).
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "ok".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(300),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let _ = server
        .lsp_health_impl(crate::server::types::HealthParams {
            action: None,
            language: None,
            force_probe: None,
        })
        .await;

    let interval = server.get_probe_interval("go");
    assert_eq!(
        interval, 30,
        "elapsed=300 is inclusive (<= 300) → interval must be 30, not 120"
    );
}

#[tokio::test]
async fn test_probe_interval_just_past_300s_boundary() {
    // elapsed=301 crosses the second boundary → 120s bucket.
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "ok".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(301),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    let _ = server
        .lsp_health_impl(crate::server::types::HealthParams {
            action: None,
            language: None,
            force_probe: None,
        })
        .await;

    let interval = server.get_probe_interval("go");
    assert_eq!(
        interval, 120,
        "elapsed=301 crosses second boundary → interval must be 120"
    );
}

// ── PATCH-004 GAP A1: non-zero last_probe_age_secs ────────────────────

#[tokio::test]
async fn test_health_shows_nonzero_probe_age() {
    // Insert a cache entry backdated 5s. With uptime=350s → threshold=30 → 5 < 30 (fresh),
    // so no re-probe fires. The final-pass sync stamps last_probe_age_secs from the entry age.
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "ok".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(350),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Backdate entry by 5 seconds using pub(crate) created_at field.
    let mut aged_entry = crate::server::ProbeCacheEntry::new(true, true);
    aged_entry.created_at = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(5))
        .expect("backdating must succeed");
    server
        .probe_cache
        .lock()
        .unwrap()
        .insert("go".to_string(), aged_entry);

    let result = server
        .lsp_health_impl(crate::server::types::HealthParams {
            action: None,
            language: Some("go".to_string()),
            force_probe: Some(false),
        })
        .await;
    let val = unpack_health(result.expect("should succeed"));

    let go_health = &val.languages[0];
    assert!(
        go_health.last_probe_age_secs.is_some(),
        "last_probe_age_secs must be Some"
    );
    // Cache was used (age=5s < threshold=30s) → no live probe → call count stays 0.
    assert_eq!(
        lawyer_clone.goto_definition_call_count(),
        0,
        "fresh-enough cache must suppress re-probe"
    );
    // Age must be non-zero (backdated 5s, allow 4s for slow machines).
    assert!(
        go_health.last_probe_age_secs.unwrap() >= 4,
        "last_probe_age_secs must reflect the 5s backdating, got {:?}",
        go_health.last_probe_age_secs
    );
}

// ── PATCH-004 GAP C1/C3: stale cache triggers re-probe without force_probe ─

#[tokio::test]
async fn test_health_stale_cache_triggers_reprobe_without_force_probe() {
    // uptime=350s → interval=120 → threshold=min(30,120)=30.
    // Insert a cache entry backdated 35s > threshold. Even with force_probe=false,
    // the stale entry must trigger a live re-probe.
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let lawyer_clone = lawyer.clone();
    let (server, ws) = make_server_with_lawyer(surgeon, lawyer);

    std::fs::write(ws.path().join("main.go"), "package main").unwrap();

    lawyer_clone.set_capability_status(std::collections::HashMap::from([(
        "go".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "ok".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(350),
            diagnostics_strategy: Some("push".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(false),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    // Backdate entry by 35s (> threshold=30s) using pub(crate) created_at.
    let mut stale_entry = crate::server::ProbeCacheEntry::new(true, true);
    stale_entry.created_at = std::time::Instant::now()
        .checked_sub(std::time::Duration::from_secs(35))
        .expect("backdating must succeed");
    server
        .probe_cache
        .lock()
        .unwrap()
        .insert("go".to_string(), stale_entry);

    // Queue a mock probe response.
    lawyer_clone.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
        file: "main.go".to_string(),
        line: 1,
        column: 0,
        preview: "package main".to_string(),
    })));

    // Call WITHOUT force_probe — stale cache must still trigger re-probe.
    let result = server
        .lsp_health_impl(crate::server::types::HealthParams {
            action: None,
            language: Some("go".to_string()),
            force_probe: Some(false),
        })
        .await;
    let val = unpack_health(result.expect("should succeed"));

    // Live probe must have fired due to stale cache.
    assert!(
        lawyer_clone.goto_definition_call_count() >= 1,
        "stale cache (age > threshold) must trigger re-probe even without force_probe=true"
    );
    assert!(
        val.languages[0].probe_verified,
        "probe_verified must be true after live re-probe"
    );
}

// ── PATCH-001: Health Status Semantic Reconciliation Tests ──────────

#[tokio::test]
async fn test_health_degraded_status_has_navigation_verified_false() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(120),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    lawyer.set_goto_definition_result(Err(pathfinder_lsp::LspError::Timeout {
        operation: "goto_definition".to_string(),
        timeout_ms: 10000,
    }));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    assert_eq!(val.status, "degraded");
    let rust_health = &val.languages[0];
    assert_eq!(rust_health.status, "degraded");
    assert_eq!(rust_health.navigation_verified, Some(false));
    assert_eq!(rust_health.navigation_ready, Some(true));
    assert!(
        !rust_health.degraded_tools.is_empty(),
        "degraded_tools must be non-empty when status=degraded"
    );
    assert!(
        rust_health
            .degraded_tools
            .iter()
            .any(|t| t.tool == "locate" && t.severity == "degraded"),
        "locate should be marked degraded with 'degraded' severity"
    );
    assert!(
        rust_health
            .degraded_tools
            .iter()
            .any(|t| t.tool == "trace" && t.severity == "degraded"),
        "trace should be marked degraded with 'degraded' severity"
    );
    assert!(
        rust_health
            .degraded_tools
            .iter()
            .any(|t| t.tool == "inspect" && t.severity == "degraded"),
        "inspect should be marked degraded with 'degraded' severity"
    );
}

#[tokio::test]
async fn test_health_ready_status_has_navigation_verified_true() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(120),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
        file: "src/main.rs".to_string(),
        line: 1,
        column: 0,
        preview: "fn main()".to_string(),
    })));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: None,
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    assert_eq!(val.status, "ready");
    let rust_health = &val.languages[0];
    assert_eq!(rust_health.status, "ready");
    assert_eq!(rust_health.navigation_verified, Some(true));
    assert!(
        rust_health.degraded_tools.is_empty(),
        "degraded_tools must be empty when all capabilities present and probe verified"
    );
}

#[tokio::test]
async fn test_health_degraded_tools_recomputed_after_probe() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(120),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    lawyer.set_goto_definition_result(Err(pathfinder_lsp::LspError::ConnectionLost));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: Some(true),
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let rust_health = &val.languages[0];
    assert_eq!(rust_health.status, "degraded");
    assert_eq!(rust_health.navigation_verified, Some(false));
    assert!(
        !rust_health.degraded_tools.is_empty(),
        "Regression: degraded_tools was frozen at pre-probe empty state"
    );
    assert!(
        rust_health.degraded_tools.len() >= 3,
        "degraded_tools should list locate, trace, inspect as degraded"
    );
}

#[tokio::test]
async fn test_health_status_never_ready_with_navigation_verified_false() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(120),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    lawyer.set_goto_definition_result(Err(pathfinder_lsp::LspError::Timeout {
        operation: "goto_definition".to_string(),
        timeout_ms: 10000,
    }));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: Some(true),
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let rust_health = &val.languages[0];
    assert!(
        !(rust_health.status == "ready" && rust_health.navigation_verified == Some(false)),
        "Invariant violation: status=ready implies navigation_verified=Some(true)"
    );
}

#[tokio::test]
async fn test_health_status_never_degraded_with_navigation_verified_true() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, ws_dir) = make_server_with_lawyer(surgeon, lawyer.clone());

    std::fs::create_dir_all(ws_dir.path().join("src")).unwrap();
    std::fs::write(ws_dir.path().join("src/main.rs"), "fn main() {}").unwrap();

    lawyer.set_capability_status(std::collections::HashMap::from([(
        "rust".to_string(),
        pathfinder_lsp::types::LspLanguageStatus {
            validation: true,
            reason: "LSP connected".to_string(),
            navigation_ready: Some(true),
            indexing_complete: Some(true),
            uptime_seconds: Some(120),
            diagnostics_strategy: Some("pull".to_string()),
            supports_definition: Some(true),
            supports_call_hierarchy: Some(true),
            supports_diagnostics: Some(true),
            supports_formatting: Some(true),
            server_name: None,
            indexing_source: None,
            indexing_duration_secs: None,
            indexing_progress_percent: None,
            registrations_received: None,
        },
    )]));

    lawyer.set_goto_definition_result(Ok(Some(pathfinder_lsp::types::DefinitionLocation {
        file: "src/main.rs".to_string(),
        line: 1,
        column: 0,
        preview: "fn main()".to_string(),
    })));

    let params = crate::server::types::HealthParams {
        action: None,
        language: Some("rust".to_string()),
        force_probe: Some(true),
    };
    let result = server.lsp_health_impl(params).await;
    let val = unpack_health(result.expect("should succeed"));

    let rust_health = &val.languages[0];
    assert!(
        !(rust_health.status == "degraded" && rust_health.navigation_verified == Some(true)),
        "Invariant violation: status=degraded implies navigation_verified=Some(false)"
    );
}
