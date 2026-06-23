//! Shared test helpers for navigation sub-modules.
//!
//! This module is only compiled during testing and provides common
//! test infrastructure used across all navigation tool test modules.

#![cfg(test)]
#![allow(clippy::expect_used)]

use crate::server::PathfinderServer;
use pathfinder_common::config::PathfinderConfig;
use pathfinder_common::sandbox::Sandbox;
use pathfinder_common::types::{SymbolScope, WorkspaceRoot};
use pathfinder_lsp::MockLawyer;
use pathfinder_search::MockScout;
use pathfinder_treesitter::mock::MockSurgeon;
use std::sync::Arc;

/// Create a `PathfinderServer` with mock engines and a mock lawyer.
///
/// Returns the server and the temp workspace directory (must be kept alive
/// for the duration of the test since `WorkspaceRoot` references it).
pub(super) fn make_server_with_lawyer(
    mock_surgeon: Arc<MockSurgeon>,
    mock_lawyer: Arc<MockLawyer>,
) -> (PathfinderServer, tempfile::TempDir) {
    let ws_dir = make_temp_workspace();
    let ws = WorkspaceRoot::new(ws_dir.path()).expect("valid root");
    let config = PathfinderConfig::default();
    let sandbox = Sandbox::new(ws.path(), &config.sandbox);
    let server = PathfinderServer::with_all_engines(
        ws,
        config,
        sandbox,
        Arc::new(MockScout::default()),
        mock_surgeon,
        mock_lawyer,
    );
    (server, ws_dir)
}

/// Create a tempdir with standard test files so the file existence check passes.
pub(super) fn make_temp_workspace() -> tempfile::TempDir {
    let ws_dir = tempfile::tempdir().expect("temp dir");
    let src_dir = ws_dir.path().join("src");
    std::fs::create_dir_all(&src_dir).expect("create src dir");
    std::fs::write(src_dir.join("auth.rs"), "fn login() { }").expect("create auth.rs");
    std::fs::write(
        src_dir.join("token.rs"),
        "fn validate_token() -> bool { true }",
    )
    .expect("create token.rs");
    std::fs::write(src_dir.join("main.rs"), "fn main() {}").expect("create main.rs");
    std::fs::write(src_dir.join("service.rs"), "fn login() { }").expect("create service.rs");
    std::fs::write(src_dir.join("user.rs"), "struct User { name: String }")
        .expect("create user.rs");
    std::fs::write(src_dir.join("auth.go"), "func login() {}").expect("create auth.go");
    ws_dir
}

/// Create a standard `SymbolScope` for testing.
pub(super) fn make_scope() -> SymbolScope {
    SymbolScope {
        content: "fn login() { }".to_owned(),
        start_line: 9,
        end_line: 9,
        name_column: 0,
        language: "rust".to_owned(),
        ..Default::default()
    }
}
