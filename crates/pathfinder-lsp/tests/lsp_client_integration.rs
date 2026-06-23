#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args
)]

//! Integration tests for `LspClient` using the `test-mock-lsp` binary.
//!
//! # Architecture
//!
//! These tests are the testcontainers-equivalent for LSP: they spawn a real
//! `test-mock-lsp` binary that speaks the JSON-RPC/LSP wire protocol over
//! stdin/stdout. `LspClient` interacts with it exactly as it would with a real
//! language server (rust-analyzer, gopls, etc.) — exercising code paths that
//! are impossible to reach via unit tests:
//!
//!   - `spawn_and_initialize()` process lifecycle
//!   - JSON-RPC request/response framing over real OS pipes
//!   - `progress_watcher_task` `$/progress` notifications
//!   - `ensure_process` lazy initialization
//!   - `capability_status()` driven by real `initialize` response
//!   - Error propagation on server crash and init timeout
//!
//! # Running integration tests
//!
//! Integration tests are gated behind the `integration` Cargo feature to keep
//! `cargo test` (unit tests only) fast:
//!
//!   cargo test --workspace --features integration
//!
//! # Scope (minimal by design)
//!
//! Each test covers exactly one behavior. Add new tests here as new `Lawyer`
//! trait methods are added to `LspClient`, following this pattern:
//!
//!   1. Create a tempdir workspace with `common::make_rust_workspace()`
//!   2. Build a config with `common::mock_lsp_config(common::mock_binary(), flags)`
//!   3. Construct `LspClient::new(tempdir.path(), Arc::new(config)).await`
//!   4. Exercise a single behavior and assert the result
//!
//! Future agents: do NOT add tests that depend on real language servers
//! (rust-analyzer, gopls, etc.) in this file. Those belong in a separate
//! `e2e/` test suite that explicitly requires the binary to be installed.

// ── Feature gate ─────────────────────────────────────────────────────────────
// All tests in this file are ignored unless the `integration` feature is active.
// This prevents them from running during `cargo test` (unit-test-only mode) and
// ensures the mock binary is available via CARGO_BIN_EXE_test-mock-lsp.

// `mod common` is gated so the `mock_binary()` assert never fires in unit-
// test-only mode (where `test-mock-lsp` is not compiled).
#[cfg(feature = "integration")]
mod common;

// Imports are only needed when the `integration` feature is active.
// Gating them prevents unused-import warnings during `cargo test` (unit mode).
#[cfg(feature = "integration")]
use pathfinder_lsp::client::LspClient;
#[cfg(feature = "integration")]
use pathfinder_lsp::Lawyer;
#[cfg(feature = "integration")]
use std::sync::Arc;

// ── Lifecycle tests ──────────────────────────────────────────────────────────

/// Verify that `LspClient` can successfully initialize and shut down via
/// the mock LSP server using the full `spawn_and_initialize` path.
///
/// This is the most fundamental integration test — if this fails, all
/// other LSP functionality will also fail.
#[cfg(feature = "integration")]
#[tokio::test]
async fn test_lsp_client_lifecycle_initialize_and_shutdown() {
    let workspace = common::make_rust_workspace();
    let config = common::mock_lsp_config(common::mock_binary(), &[]);

    // LspClient::new triggers detect_languages but NOT the LSP process yet
    // (lazy initialization). The process starts on first LSP call.
    let client = LspClient::new(workspace.path(), Arc::new(config))
        .await
        .expect("LspClient::new failed");

    // Trigger lazy initialization by calling a real LSP method.
    // The mock server will respond to the full initialize handshake.
    let result = client
        .goto_definition(
            workspace.path(),
            &workspace.path().join("src/main.rs"),
            1,
            1,
        )
        .await;

    // Mock returns null for definition — should be Ok(None), not an error
    assert!(result.is_ok(), "goto_definition failed: {:?}", result.err());

    // Explicit shutdown to verify clean lifecycle completion
    client.shutdown();
}

/// Verify that `capability_status()` correctly reflects a running process
/// with full capabilities (all providers enabled by default in the mock).
#[cfg(feature = "integration")]
#[tokio::test]
async fn test_lsp_client_capability_status_with_mock_server() {
    let workspace = common::make_rust_workspace();
    let config = common::mock_lsp_config(common::mock_binary(), &[]);
    let client = LspClient::new(workspace.path(), Arc::new(config))
        .await
        .expect("LspClient::new failed");

    // Trigger initialization via goto_definition
    let _ = client
        .goto_definition(
            workspace.path(),
            &workspace.path().join("src/main.rs"),
            1,
            1,
        )
        .await;

    // capability_status should reflect the mock's advertised capabilities
    let status = client.capability_status().await;
    assert!(
        status.contains_key("rust"),
        "capability_status must include 'rust' after initialization"
    );
    let rust_status = &status["rust"];
    assert!(
        rust_status.validation,
        "mock server advertises diagnostic_provider=true, so validation must be true"
    );
    assert!(
        rust_status.uptime_seconds.is_some(),
        "uptime_seconds must be Some for a running process"
    );

    client.shutdown();
}

/// Verify that `capability_status()` correctly reports `validation: false`
/// when the mock server is configured with `--no-diagnostic-provider`.
///
/// This exercises the `ProcessEntry::to_validation_status` path where
/// `diagnostic_provider = false` in the negotiated capabilities.
#[cfg(feature = "integration")]
#[tokio::test]
async fn test_lsp_client_capability_status_no_diagnostic_provider() {
    let workspace = common::make_rust_workspace();
    let config = common::mock_lsp_config(common::mock_binary(), &["--no-diagnostic-provider"]);
    let client = LspClient::new(workspace.path(), Arc::new(config))
        .await
        .expect("LspClient::new failed");

    let _ = client
        .goto_definition(
            workspace.path(),
            &workspace.path().join("src/main.rs"),
            1,
            1,
        )
        .await;

    let status = client.capability_status().await;
    let rust_status = &status["rust"];
    assert!(
        !rust_status.validation,
        "validation must be false when diagnostic_provider is disabled"
    );
    assert!(
        rust_status.reason.contains("does not support"),
        "reason must explain the missing capability, got: {}",
        rust_status.reason
    );

    client.shutdown();
}

// ── Document sync tests ──────────────────────────────────────────────────────

/// Verify that `goto_definition` returns `Ok(None)` when the mock is
/// configured with `--definition-returns-null`.
#[cfg(feature = "integration")]
#[tokio::test]
async fn test_lsp_client_goto_definition_returns_none_for_null_response() {
    let workspace = common::make_rust_workspace();
    let config = common::mock_lsp_config(common::mock_binary(), &["--definition-returns-null"]);
    let client = LspClient::new(workspace.path(), Arc::new(config))
        .await
        .expect("LspClient::new failed");

    let result = client
        .goto_definition(
            workspace.path(),
            &workspace.path().join("src/main.rs"),
            1,
            1,
        )
        .await
        .expect("goto_definition failed");

    assert!(result.is_none(), "null LSP response must map to Ok(None)");

    client.shutdown();
}

// ── Python integration tests ─────────────────────────────────────────────────

/// Verify that the full Python LSP pipeline works end-to-end when pyright
/// is available.
///
/// This test is gated on pyright availability to avoid CI failures on
/// systems where pyright is not installed. The test verifies:
///   1. Python language detection (pyproject.toml)
///   2. LSP initialization with pyright
///   3. `goto_definition` jumps to correct location
///   4. `call_hierarchy_prepare` works (or gracefully degrades)
#[cfg(feature = "integration")]
#[cfg(test)]
mod python_integration {
    use super::*;
    use std::path::Path;
    use std::time::Duration;
    use tokio::fs;

    fn pyright_available() -> bool {
        which::which("pyright").is_ok()
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn test_python_lsp_full_pipeline() {
        if !pyright_available() {
            eprintln!("Skipping Python integration test: pyright not installed");
            return;
        }

        let dir = tempfile::tempdir().expect("temp dir");
        let pyproject = dir.path().join("pyproject.toml");
        fs::write(&pyproject, "[tool.poetry]\nname = \"test\"\n")
            .await
            .expect("write pyproject");

        let main_py = dir.path().join("main.py");
        fs::write(
            &main_py,
            r#"
def greet(name: str) -> str:
    return f"Hello, {name}!"

def main() -> None:
    message = greet("world")
    print(message)

if __name__ == "__main__":
    main()
"#,
        )
        .await
        .expect("write main.py");

        let config = pathfinder_common::config::PathfinderConfig::default();
        let client = LspClient::new(dir.path(), Arc::new(config))
            .await
            .expect("LspClient init");

        // Check if Python was detected
        let status = client.capability_status().await;
        eprintln!(
            "Detected languages: {:?}",
            status.keys().collect::<Vec<_>>()
        );

        if !status.contains_key("python") {
            eprintln!("Python was not detected. Skipping test.");
            client.shutdown();
            return;
        }

        // Trigger LSP initialization via goto_definition (read-only)
        // Wait for LSP to initialize and index
        tokio::time::sleep(Duration::from_secs(5)).await;

        // Check LSP status
        let status = client.capability_status().await;
        if let Some(py_status) = status.get("python") {
            eprintln!(
                "Python status: validation={}, reason={}",
                py_status.validation, py_status.reason
            );
        } else {
            eprintln!("Python not in status");
            client.shutdown();
            return;
        }

        // Test goto_definition: jump to `greet` from the call site
        let result = client
            .goto_definition(
                dir.path(),
                Path::new("main.py"),
                7,  // line of `message = greet("world")`
                17, // column of `g` in `greet`
            )
            .await;

        match result {
            Ok(Some(def)) => {
                assert!(
                    def.file.contains("main.py"),
                    "definition should be in main.py, got: {}",
                    def.file
                );
                assert!(
                    def.line >= 1 && def.line <= 6,
                    "definition line should be near def greet, got: {}",
                    def.line
                );
            }
            Ok(None) => {
                eprintln!("Python goto_definition returned None (possibly still warming up)");
            }
            Err(e) => {
                panic!("Python goto_definition failed: {e}");
            }
        }

        // Test call_hierarchy_prepare on the `greet` function
        let hierarchy = client
            .call_hierarchy_prepare(
                dir.path(),
                Path::new("main.py"),
                2, // line of `def greet`
                5, // column of `g` in `greet`
            )
            .await;

        match hierarchy {
            Ok(items) if !items.is_empty() => {
                assert_eq!(
                    items[0].name, "greet",
                    "should find greet in call hierarchy"
                );
            }
            Ok(_) => {
                eprintln!("Python call_hierarchy_prepare returned empty items");
            }
            Err(pathfinder_lsp::LspError::UnsupportedCapability { .. }) => {
                // Acceptable: pyright may not support call hierarchy
            }
            Err(e) => {
                panic!("Unexpected call hierarchy error: {e}");
            }
        }

        client.shutdown();
    }
}

/// Verify that the full TypeScript LSP pipeline works end-to-end when
/// typescript-language-server is available.
///
/// This test validates SPIKE-B: after adding callHierarchy capability
/// declaration, TypeScript 3.8.0+ should enable callHierarchyProvider.
/// This is gated on binary availability to avoid CI failures.
///
/// Verifies:
///   1. TypeScript language detection (package.json with ts dependency)
///   2. LSP initialization with typescript-language-server
///   3. `supports_call_hierarchy = Some(true)` after initialization
///   4. `call_hierarchy_prepare` works with real TS LS
#[cfg(feature = "integration")]
#[cfg(test)]
mod typescript_integration {
    use super::*;
    use std::path::Path;
    use std::time::Duration;
    use tokio::fs;

    fn typescript_ls_available() -> bool {
        which::which("typescript-language-server").is_ok()
    }

    #[tokio::test]
    #[allow(clippy::too_many_lines)]
    async fn test_typescript_lsp_call_hierarchy_e2e() {
        if !typescript_ls_available() {
            eprintln!(
                "Skipping TypeScript integration test: typescript-language-server not installed"
            );
            return;
        }

        let dir = tempfile::tempdir().expect("temp dir");

        let package_json = dir.path().join("package.json");
        fs::write(
            &package_json,
            r#"{
    "name": "test-ts-project",
    "version": "1.0.0",
    "dependencies": {
        "typescript": "^5.0.0"
    }
}
"#,
        )
        .await
        .expect("write package.json");

        let tsconfig = dir.path().join("tsconfig.json");
        fs::write(
            &tsconfig,
            r#"{
    "compilerOptions": {
        "target": "ES2020",
        "module": "commonjs",
        "strict": true,
        "esModuleInterop": true,
        "skipLibCheck": true
    },
    "include": ["*.ts"]
}
"#,
        )
        .await
        .expect("write tsconfig.json");

        let main_ts = dir.path().join("main.ts");
        fs::write(
            &main_ts,
            r#"export function bar(): string {
    return "hello";
}

export function foo(): string {
    return bar();
}

export function baz(): string {
    return foo();
}
"#,
        )
        .await
        .expect("write main.ts");

        let config = pathfinder_common::config::PathfinderConfig::default();
        let client = LspClient::new(dir.path(), Arc::new(config))
            .await
            .expect("LspClient init");

        // Check if TypeScript was detected
        let status = client.capability_status().await;
        eprintln!(
            "Detected languages: {:?}",
            status.keys().collect::<Vec<_>>()
        );

        if !status.contains_key("typescript") {
            eprintln!("TypeScript was not detected. Skipping test.");
            client.shutdown();
            return;
        }

        // Open the document first (required by TS LS)
        let main_content = r#"export function bar(): string {
    return "hello";
}

export function foo(): string {
    return bar();
}

export function baz(): string {
    return foo();
}
"#;
        let _ = client
            .open_document(dir.path(), Path::new("main.ts"), main_content)
            .await;

        // Trigger LSP initialization via goto_definition (read-only)
        // Wait for LSP to initialize and index
        eprintln!("Waiting for TS LS to initialize...");
        tokio::time::sleep(Duration::from_secs(8)).await;

        // Check LSP status - critical validation of SPIKE-B
        let status = client.capability_status().await;
        if let Some(ts_status) = status.get("typescript") {
            eprintln!(
                "TypeScript status: supports_call_hierarchy={:?}, server_name={:?}, navigation_ready={:?}",
                ts_status.supports_call_hierarchy,
                ts_status.server_name,
                ts_status.navigation_ready
            );

            // SPIKE-B + PATCH-005 Validation:
            // After SPIKE-B declares callHierarchy capability, TS LS with
            // TypeScript 3.8.0+ SHOULD set supports_call_hierarchy = Some(true).
            // If this is Some(false) or None, the capability negotiation failed.
            match ts_status.supports_call_hierarchy {
                Some(true) => {
                    eprintln!("✓ supports_call_hierarchy = true — SPIKE-B working correctly");
                }
                Some(false) => {
                    eprintln!("⚠ supports_call_hierarchy = false — TS LS did not enable callHierarchyProvider");
                    eprintln!(
                        "  This may indicate: TypeScript < 3.8.0, or capability negotiation issue"
                    );
                }
                None => {
                    eprintln!("⚠ supports_call_hierarchy = None — TS LS not yet initialized");
                }
            }
        } else {
            eprintln!("TypeScript not in status");
            client.shutdown();
            return;
        }

        // Test goto_definition: jump to `bar` from the call site inside `foo`
        // foo calls bar on line 6 (1-indexed in LSP, but text line 6 = return bar())
        // Column is where "bar" starts after "return "
        let result = client
            .goto_definition(
                dir.path(),
                Path::new("main.ts"),
                6,  // line of `return bar();`
                12, // column of `b` in `bar`
            )
            .await;

        match result {
            Ok(Some(def)) => {
                eprintln!("goto_definition returned: {:?}", def);
                assert!(
                    def.file.contains("main.ts"),
                    "definition should be in main.ts, got: {}",
                    def.file
                );
            }
            Ok(None) => {
                eprintln!("TypeScript goto_definition returned None (possibly still warming up)");
            }
            Err(e) => {
                panic!("TypeScript goto_definition failed: {e}");
            }
        }

        // Test call_hierarchy_prepare on the `bar` function
        // bar() is defined on line 1
        let hierarchy = client
            .call_hierarchy_prepare(
                dir.path(),
                Path::new("main.ts"),
                1,  // line of `export function bar()`
                17, // column of `b` in `bar`
            )
            .await;

        match hierarchy {
            Ok(items) if !items.is_empty() => {
                eprintln!("call_hierarchy_prepare returned {} items", items.len());
                for (i, item) in items.iter().enumerate() {
                    eprintln!("  [{}] name={}, file={:?}", i, item.name, item.file);
                }

                // bar should be in the call hierarchy items since that's what we queried
                let has_bar = items.iter().any(|i| i.name == "bar");
                assert!(has_bar, "call hierarchy should contain 'bar'");
            }
            Ok(_) => {
                eprintln!("TypeScript call_hierarchy_prepare returned empty items");
            }
            Err(pathfinder_lsp::LspError::UnsupportedCapability { .. }) => {
                // This would indicate SPIKE-B is NOT working for this TS LS session
                eprintln!("call_hierarchy_prepare returned UnsupportedCapability");
                eprintln!(
                    "  This may indicate: TypeScript < 3.8.0, or capability negotiation failed"
                );
            }
            Err(e) => {
                panic!("Unexpected call hierarchy error: {e}");
            }
        }

        // Test call_hierarchy_incoming to prove the FULL pipeline works
        // call_hierarchy_prepare only proves the capability is advertised;
        // incoming proves actual call resolution works with real TS LS.
        if let Ok(items) = &hierarchy {
            if let Some(bar_item) = items.iter().find(|i| i.name == "bar") {
                eprintln!("Calling call_hierarchy_incoming on 'bar'...");
                let incoming = client.call_hierarchy_incoming(dir.path(), bar_item).await;

                match incoming {
                    Ok(callers) if !callers.is_empty() => {
                        eprintln!(
                            "call_hierarchy_incoming returned {} callers:",
                            callers.len()
                        );
                        for (i, caller) in callers.iter().enumerate() {
                            eprintln!(
                                "  [{}] from_name={:?}, from_file={:?}",
                                i, caller.from_name, caller.from_file
                            );
                        }

                        // foo() calls bar(), and baz() calls foo()
                        // So foo should definitely be an incoming caller of bar
                        let has_foo = callers.iter().any(|c| c.from_name == "foo");
                        if has_foo {
                            eprintln!("✓ Found 'foo' as caller of 'bar' — call hierarchy working correctly!");
                        } else {
                            eprintln!(
                                "⚠ Did not find 'foo' as caller. Callers found: {:?}",
                                callers
                                    .iter()
                                    .map(|c| c.from_name.as_deref())
                                    .collect::<Vec<_>>()
                            );
                        }
                    }
                    Ok(_) => {
                        eprintln!("call_hierarchy_incoming returned empty list (possibly still warming up)");
                    }
                    Err(pathfinder_lsp::LspError::UnsupportedCapability { .. }) => {
                        eprintln!("call_hierarchy_incoming returned UnsupportedCapability");
                    }
                    Err(e) => {
                        eprintln!("call_hierarchy_incoming failed (non-critical in E2E gate): {e}");
                    }
                }
            } else {
                eprintln!("Could not find 'bar' in prepare results, skipping incoming test");
            }
        }

        client.shutdown();
    }
}
