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

/// Verify that `did_open` + `pull_diagnostics` returns the mock's canned
/// diagnostic list. This exercises the full document sync + diagnostic pull
/// pipeline on real OS pipes.
#[cfg(feature = "integration")]
#[tokio::test]
async fn test_lsp_client_pull_diagnostics_returns_mock_items() {
    let workspace = common::make_rust_workspace();
    let config = common::mock_lsp_config(common::mock_binary(), &[]);
    let client = LspClient::new(workspace.path(), Arc::new(config))
        .await
        .expect("LspClient::new failed");

    let file_path = workspace.path().join("src/main.rs");
    let content = "fn main() {}";

    // did_open triggers lazy initialization via ensure_process
    client
        .did_open(workspace.path(), &file_path, content)
        .await
        .expect("did_open failed");

    // Pull diagnostics — mock returns one synthetic error by default
    let diagnostics = client
        .pull_diagnostics(workspace.path(), &file_path)
        .await
        .expect("pull_diagnostics failed");

    assert!(
        !diagnostics.is_empty(),
        "mock server returns one synthetic error by default"
    );
    assert_eq!(
        diagnostics[0].severity as u8,
        1, // LspDiagnosticSeverity::Error
        "mock diagnostic must be severity Error (1)"
    );
    assert!(
        diagnostics[0].message.contains("mock error"),
        "mock diagnostic message expected, got: {}",
        diagnostics[0].message
    );

    client.shutdown();
}

/// Verify that `pull_diagnostics` returns an empty list when the mock is
/// configured with `--no-diagnostics`. This exercises the empty-snapshot
/// path in `build_validation_outcome`.
#[cfg(feature = "integration")]
#[tokio::test]
async fn test_lsp_client_pull_diagnostics_empty_when_configured() {
    let workspace = common::make_rust_workspace();
    let config = common::mock_lsp_config(common::mock_binary(), &["--no-diagnostics"]);
    let client = LspClient::new(workspace.path(), Arc::new(config))
        .await
        .expect("LspClient::new failed");

    let file_path = workspace.path().join("src/main.rs");
    client
        .did_open(workspace.path(), &file_path, "fn main() {}")
        .await
        .expect("did_open failed");

    let diagnostics = client
        .pull_diagnostics(workspace.path(), &file_path)
        .await
        .expect("pull_diagnostics failed");

    assert!(
        diagnostics.is_empty(),
        "--no-diagnostics flag must yield empty diagnostic list"
    );

    client.shutdown();
}

// ── Error path tests ─────────────────────────────────────────────────────────

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

/// Verify that `did_change` and `did_close` notifications complete without
/// error. These are fire-and-forget notifications (no LSP response expected).
#[cfg(feature = "integration")]
#[tokio::test]
async fn test_lsp_client_document_sync_notifications_succeed() {
    let workspace = common::make_rust_workspace();
    let config = common::mock_lsp_config(common::mock_binary(), &[]);
    let client = LspClient::new(workspace.path(), Arc::new(config))
        .await
        .expect("LspClient::new failed");

    let file_path = workspace.path().join("src/main.rs");

    client
        .did_open(workspace.path(), &file_path, "fn main() {}")
        .await
        .expect("did_open failed");

    client
        .did_change(workspace.path(), &file_path, "fn main() { println!(); }", 1)
        .await
        .expect("did_change failed");

    client
        .did_close(workspace.path(), &file_path)
        .await
        .expect("did_close failed");

    client.shutdown();
}
