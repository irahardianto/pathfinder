//! CLI-driven configuration for the mock LSP server.
//!
//! Each field maps to a command-line flag. Integration tests use these flags
//! to exercise specific `LspClient` code paths without modifying the server
//! source code.
//!
//! # Adding new configuration options
//!
//! Future agents: to test a new `LspClient` behavior, add a field here and
//! the corresponding CLI flag in `main.rs::parse_args()`. Keep defaults
//! permissive (everything enabled) so existing tests continue to pass.

use serde_json::{json, Value};

/// Run-time configuration parsed from CLI args.
#[derive(Debug, Clone)]
// The boolean fields represent capability flags that mirror DetectedCapabilities.
// A struct with more than 3 bools is acceptable here because each flag maps
// directly to a distinct LSP server capability — collapsing them into a
// bitmask or enum set would reduce clarity for test authors.
#[allow(clippy::struct_excessive_bools)]
pub struct MockConfig {
    // ── Capability flags ──────────────────────────────────────────────────
    // These mirror `DetectedCapabilities` in `pathfinder-lsp`. The mock server
    // uses them to construct the `initialize` response so LspClient negotiates
    // the exact capability set the test requires.
    /// Advertise `textDocument/definition` support.
    pub definition_provider: bool,
    /// Advertise `textDocument/diagnostic` (pull) support.
    pub diagnostic_provider: bool,
    /// Advertise `workspace/diagnostic` support.
    pub workspace_diagnostic_provider: bool,
    /// Advertise `callHierarchy` support.
    pub call_hierarchy_provider: bool,
    /// Advertise `textDocument/formatting` support.
    pub formatting_provider: bool,

    // ── Response overrides ────────────────────────────────────────────────
    /// Return `null` for `textDocument/definition` instead of a canned location.
    pub definition_returns_null: bool,
    /// Diagnostic items returned by `textDocument/diagnostic`.
    /// Default: one synthetic error so tests can assert non-empty results.
    pub diagnostic_items: Value,

    // ── Fault injection ───────────────────────────────────────────────────
    /// Exit the process after this many requests (0 = never crash).
    /// Used to test `LspClient` crash-recovery paths.
    pub crash_after: usize,
    /// Sleep this many milliseconds before sending the `initialize` response.
    /// Used to test initialization timeout handling in `LspClient`.
    pub init_delay_ms: u64,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            definition_provider: true,
            diagnostic_provider: true,
            workspace_diagnostic_provider: true,
            call_hierarchy_provider: true,
            formatting_provider: true,
            definition_returns_null: false,
            // One synthetic error by default so tests can assert non-empty pulls.
            diagnostic_items: json!([{
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end":   { "line": 0, "character": 4 }
                },
                "severity": 1,
                "message": "mock error from test-mock-lsp",
                "source": "mock"
            }]),
            crash_after: 0,
            init_delay_ms: 0,
        }
    }
}

/// Parse `std::env::args()` into a [`MockConfig`].
///
/// Recognized flags (all optional, defaults are permissive):
///   --no-diagnostic-provider         Omit `diagnostic_provider` from capabilities
///   --no-definition-provider         Omit definitionProvider
///   --no-call-hierarchy-provider     Omit callHierarchyProvider
///   --no-formatting-provider         Omit documentFormattingProvider
///   --definition-returns-null        Make textDocument/definition return null
///   --no-diagnostics                 Return empty diagnostic list
///   --crash-after=<N>                Exit after N requests (1-indexed)
///   --init-delay-ms=<N>              Sleep N ms before initialize response
#[must_use]
pub fn parse_args() -> MockConfig {
    let mut config = MockConfig::default();
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--no-diagnostic-provider" => config.diagnostic_provider = false,
            "--no-definition-provider" => config.definition_provider = false,
            "--no-call-hierarchy-provider" => config.call_hierarchy_provider = false,
            "--no-formatting-provider" => config.formatting_provider = false,
            "--definition-returns-null" => config.definition_returns_null = true,
            "--no-diagnostics" => config.diagnostic_items = json!([]),
            _ if arg.starts_with("--crash-after=") => {
                if let Ok(n) = arg["--crash-after=".len()..].parse::<usize>() {
                    config.crash_after = n;
                }
            }
            _ if arg.starts_with("--init-delay-ms=") => {
                if let Ok(n) = arg["--init-delay-ms=".len()..].parse::<u64>() {
                    config.init_delay_ms = n;
                }
            }
            _ => {
                eprintln!("test-mock-lsp: unknown flag {arg:?} (ignored)");
            }
        }
    }
    config
}
