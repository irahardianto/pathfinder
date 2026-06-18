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
async fn test_probe_cache_positive_result_never_expires() {
    // Positive cache entries should be valid indefinitely
    let entry = crate::server::ProbeCacheEntry::new(true, true);
    assert!(entry.is_valid(), "positive entry should always be valid");
}

#[tokio::test]
async fn test_probe_cache_negative_result_is_initially_valid() {
    // Negative cache entries should be valid immediately after creation
    let entry = crate::server::ProbeCacheEntry::new(false, false);
    assert!(entry.is_valid(), "fresh negative entry should be valid");
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
    let mut status = pathfinder_lsp::types::LspLanguageStatus {
        validation: true,
        reason: "LSP connected".to_string(),
        navigation_ready: Some(true),
        indexing_complete: Some(true),
        uptime_seconds: Some(60),
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
    };
    let degraded = compute_degraded_tools(&status);
    assert_eq!(degraded.len(), 1);
    assert_eq!(degraded[0].tool, "locate");

    status.supports_definition = Some(true);
    status.supports_call_hierarchy = Some(false);
    let degraded2 = compute_degraded_tools(&status);
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
    };
    let result = server.lsp_health_impl(params).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_lsp_health_restart_successful() {
    let surgeon = Arc::new(MockSurgeon::default());
    let lawyer = Arc::new(pathfinder_lsp::MockLawyer::default());
    let (server, _ws) = make_server_with_lawyer(surgeon, lawyer);

    let params = crate::server::types::HealthParams {
        action: Some("restart".to_string()),
        language: Some("rust".to_string()),
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
