#![allow(
    dead_code,
    clippy::doc_markdown,
    clippy::doc_lazy_continuation,
    clippy::doc_link_with_quotes,
    clippy::needless_pass_by_value,
    clippy::default_trait_access
)]

//! Shared test helpers for `pathfinder-lsp` integration tests.
//!
//! # Usage pattern
//!
//! Every integration test file should declare:
//!   mod common;
//! and call these helpers to spin up the mock LSP server:
//!
//!   let tempdir = common::make_rust_workspace();
//!   let config  = common::mock_lsp_config(common::mock_binary(), &[]);
//!   let client  = LspClient::new(tempdir.path(), Arc::new(config)).await.unwrap();
//!
//! # Expanding this module
//!
//! Future agents: add workspace factory functions here for new languages:
//!   - `make_go_workspace()` — tempdir + go.mod
//!   - `make_ts_workspace()` — tempdir + tsconfig.json
//! Mirror the pattern of `make_rust_workspace` and document what the
//! workspace contains and why.

use pathfinder_common::config::{LspConfig, PathfinderConfig};
use std::collections::HashMap;
use std::path::PathBuf;
use tempfile::TempDir;

/// Returns the path to the compiled `test-mock-lsp` binary.
///
/// `CARGO_BIN_EXE_<name>` is only injected for binaries that belong to the
/// *same* crate being tested. Because `test-mock-lsp` is a sibling crate we
/// locate it by walking up from the running test executable:
///   `.../target/debug/deps/<test-binary>` → pop filename → `deps/`\
///   `deps/` is a child of the profile dir → pop `deps/` → `target/debug/`\
///   Join the binary name to find `target/debug/test-mock-lsp`.
///
/// # Panics
/// Panics if the binary has not been compiled yet. Always run integration tests
/// via `cargo test --workspace --features integration` so the full workspace
/// (including `test-mock-lsp`) is built before the tests execute.
pub fn mock_binary() -> PathBuf {
    // Walk up from the test exe to the profile output directory.
    let mut dir = std::env::current_exe().expect("current_exe failed");
    dir.pop(); // remove test binary filename
    if dir.file_name().is_some_and(|n| n == "deps") {
        dir.pop(); // step out of deps/ into target/debug/ (or target/release/)
    }
    let mock_bin = dir.join("test-mock-lsp");
    assert!(
        mock_bin.exists(),
        "Mock binary not found at {}. \
         Run `cargo build -p test-mock-lsp` first, or use \
         `cargo test --workspace --features integration` to build everything together.",
        mock_bin.display()
    );
    mock_bin
}

/// Create a minimal Rust workspace in a temporary directory.
///
/// Places a `Cargo.toml` at the root so `detect_languages` identifies the
/// workspace as a Rust project and routes it to the mock LSP binary.
pub fn make_rust_workspace() -> TempDir {
    let dir = tempfile::tempdir().expect("failed to create tempdir");
    let root = dir.path();
    std::fs::write(
        root.join("Cargo.toml"),
        "[package]\nname = \"mock-workspace\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .expect("failed to write Cargo.toml");
    std::fs::create_dir_all(root.join("src")).expect("failed to create src/");
    std::fs::write(root.join("src/main.rs"), "fn main() {}\n")
        .expect("failed to write src/main.rs");
    dir
}

/// Build a `PathfinderConfig` that routes the Rust LSP to the mock binary.
///
/// `extra_flags` are passed as `LspConfig::args` to the mock server binary.
/// They map directly to the mock's CLI flags, e.g.:
///   &["--no-diagnostic-provider"]    → mock serves no diagnostic capability
///   &["--crash-after=2"]             → mock exits after 2 requests
///   &["--init-delay-ms=5000"]        → mock delays initialize response
///
/// The `get_args_override!` macro in `detect_languages` ensures these args
/// are forwarded when a command override is present.
pub fn mock_lsp_config(mock_bin: PathBuf, extra_flags: &[&str]) -> PathfinderConfig {
    let mut lsp_map = HashMap::new();
    lsp_map.insert(
        "rust".to_string(),
        LspConfig {
            command: mock_bin.display().to_string(),
            args: extra_flags.iter().map(|s| (*s).to_string()).collect(),
            idle_timeout_minutes: 15,
            settings: serde_json::Value::Null,
            root_override: None,
        },
    );

    PathfinderConfig {
        lsp: lsp_map,
        log_level: "info".to_string(),
        ..Default::default()
    }
}
