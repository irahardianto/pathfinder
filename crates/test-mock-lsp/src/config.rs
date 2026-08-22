//! CLI-driven configuration for the mock LSP server.
//!
//! Integration tests pass flags to exercise specific `LspClient` code paths
//! without modifying server source code.

use crate::canned_index::CannedIndex;
use crate::protocol::MalformMode;
use crate::script::ScriptEngine;
use serde_json::{json, Value};
use std::fs;
use std::str::FromStr;

/// Run-time configuration parsed from CLI args.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct MockConfig {
    // ── Capability flags ──────────────────────────────────────────────────
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
    /// Advertise `textDocument/references` support.
    pub references_provider: bool,
    /// Advertise `textDocument/implementation` support.
    pub implementation_provider: bool,
    /// Position encoding advertised in initialize response (`"utf-8"`, `"utf-16"`, or `None`).
    pub position_encoding: Option<String>,

    // ── Response overrides ────────────────────────────────────────────────
    /// Return `null` for `textDocument/definition` instead of a canned location.
    pub definition_returns_null: bool,
    /// Diagnostic items returned by `textDocument/diagnostic`.
    pub diagnostic_items: Value,
    /// Default items returned by `textDocument/references` when not in canned index.
    pub references_items: Value,
    /// Default items returned by `textDocument/implementation` when not in canned index.
    pub implementation_items: Value,

    // ── Fault injection & Scripting ───────────────────────────────────────
    /// Exit the process after this many requests (0 = never crash).
    pub crash_after: usize,
    /// Sleep this many milliseconds before sending the `initialize` response.
    pub init_delay_ms: u64,
    /// Malformation mode at framing layer (`LSP-A4`).
    pub malform: MalformMode,
    /// Specific method to malform (None = all or first).
    pub malform_method: Option<String>,
    /// Scriptable response timeline engine.
    pub script: Option<ScriptEngine>,
    /// Position-aware canned index for fixture repos.
    pub canned_index: Option<CannedIndex>,

    // ── Server-initiated message emitter ──────────────────────────────────
    /// Delay before emitting dynamic registration request.
    pub register_after_ms: Option<u64>,
    /// Methods to dynamically register.
    pub register_methods: Vec<String>,
    /// ID for dynamic registration.
    pub register_id: Option<String>,
    /// Progress script steps (`["begin", "report:50", "end"]`).
    pub progress_script: Option<Value>,
    /// Number of rapid progress notifications to emit (testing 64-slot lag path).
    pub progress_count: Option<usize>,
    /// Delay before emitting progress sequence.
    pub progress_after_ms: Option<u64>,
    /// Whether to send a `workspace/configuration` request.
    pub config_request: bool,
    /// Delay before sending `workspace/configuration` request.
    pub config_request_after_ms: Option<u64>,

    // ── Workspace tracking ────────────────────────────────────────────────
    /// Workspace root directory path (for URI normalization / interpolation).
    pub workspace_root: Option<String>,
}

impl Default for MockConfig {
    fn default() -> Self {
        Self {
            definition_provider: true,
            diagnostic_provider: true,
            workspace_diagnostic_provider: true,
            call_hierarchy_provider: true,
            formatting_provider: true,
            references_provider: true,
            implementation_provider: true,
            position_encoding: None,

            definition_returns_null: false,
            diagnostic_items: json!([{
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end":   { "line": 0, "character": 4 }
                },
                "severity": 1,
                "message": "mock error from test-mock-lsp",
                "source": "mock"
            }]),
            references_items: json!([]),
            implementation_items: json!(null),

            crash_after: 0,
            init_delay_ms: 0,
            malform: MalformMode::None,
            malform_method: None,
            script: None,
            canned_index: None,

            register_after_ms: None,
            register_methods: Vec::new(),
            register_id: None,
            progress_script: None,
            progress_count: None,
            progress_after_ms: None,
            config_request: false,
            config_request_after_ms: None,
            workspace_root: None,
        }
    }
}

impl MockConfig {
    fn apply_capability_flag(&mut self, arg: &str) -> bool {
        match arg {
            "--no-diagnostic-provider" => self.diagnostic_provider = false,
            "--no-definition-provider" => self.definition_provider = false,
            "--no-call-hierarchy-provider" => self.call_hierarchy_provider = false,
            "--no-formatting-provider" => self.formatting_provider = false,
            "--no-references-provider" => self.references_provider = false,
            "--no-implementation-provider" => self.implementation_provider = false,
            "--definition-returns-null" => self.definition_returns_null = true,
            "--no-diagnostics" => self.diagnostic_items = json!([]),
            _ if arg.starts_with("--position-encoding=") => {
                let val = &arg["--position-encoding=".len()..];
                self.position_encoding = if val.eq_ignore_ascii_case("none") {
                    None
                } else {
                    Some(val.to_string())
                };
            }
            _ => return false,
        }
        true
    }

    fn apply_fault_flag(&mut self, arg: &str) -> bool {
        if let Some(rest) = arg.strip_prefix("--crash-after=") {
            if let Ok(n) = rest.parse::<usize>() {
                self.crash_after = n;
                return true;
            }
        }
        if let Some(rest) = arg.strip_prefix("--init-delay-ms=") {
            if let Ok(n) = rest.parse::<u64>() {
                self.init_delay_ms = n;
                return true;
            }
        }
        if let Some(rest) = arg.strip_prefix("--malform=") {
            if let Ok(mode) = MalformMode::from_str(rest) {
                self.malform = mode;
                return true;
            }
        }
        if let Some(rest) = arg.strip_prefix("--malform-method=") {
            self.malform_method = Some(rest.to_string());
            return true;
        }
        false
    }

    fn apply_script_and_index_flag(&mut self, arg: &str) -> bool {
        if let Some(rest) = arg.strip_prefix("--script=") {
            self.script = ScriptEngine::from_json_str(rest).ok();
            return true;
        }
        if let Some(rest) = arg.strip_prefix("--script-file=") {
            if let Ok(content) = fs::read_to_string(rest) {
                self.script = ScriptEngine::from_json_str(&content).ok();
            }
            return true;
        }
        if let Some(rest) = arg
            .strip_prefix("--index=")
            .or_else(|| arg.strip_prefix("--canned-index="))
        {
            self.canned_index = CannedIndex::from_json_str(rest).ok();
            return true;
        }
        if let Some(rest) = arg.strip_prefix("--index-file=") {
            if let Ok(content) = fs::read_to_string(rest) {
                self.canned_index = CannedIndex::from_json_str(&content).ok();
            }
            return true;
        }
        false
    }

    fn apply_emitter_flag(&mut self, arg: &str) -> bool {
        if arg == "--config-request" {
            self.config_request = true;
            return true;
        }
        if let Some(rest) = arg.strip_prefix("--config-request-after-ms=") {
            if let Ok(n) = rest.parse::<u64>() {
                self.config_request_after_ms = Some(n);
                self.config_request = true;
            }
            return true;
        }
        if let Some(rest) = arg.strip_prefix("--register-after-ms=") {
            self.register_after_ms = rest.parse::<u64>().ok();
            return true;
        }
        if let Some(rest) = arg.strip_prefix("--register-method=") {
            for m in rest.split(',') {
                let trimmed = m.trim();
                if !trimmed.is_empty() {
                    self.register_methods.push(trimmed.to_string());
                }
            }
            return true;
        }
        if let Some(rest) = arg.strip_prefix("--register-id=") {
            self.register_id = Some(rest.to_string());
            return true;
        }
        if let Some(rest) = arg.strip_prefix("--progress-script=") {
            self.progress_script = serde_json::from_str(rest).ok();
            return true;
        }
        if let Some(rest) = arg.strip_prefix("--progress-count=") {
            self.progress_count = rest.parse::<usize>().ok();
            return true;
        }
        if let Some(rest) = arg.strip_prefix("--progress-after-ms=") {
            self.progress_after_ms = rest.parse::<u64>().ok();
            return true;
        }
        if let Some(rest) = arg.strip_prefix("--workspace-root=") {
            self.workspace_root = Some(rest.to_string());
            return true;
        }
        false
    }

    fn process_arg(&mut self, arg: &str) {
        if self.apply_capability_flag(arg)
            || self.apply_fault_flag(arg)
            || self.apply_script_and_index_flag(arg)
            || self.apply_emitter_flag(arg)
        {
            return;
        }
        eprintln!("test-mock-lsp: unknown flag {arg:?} (ignored)");
    }
}

/// Parse CLI arguments into a [`MockConfig`].
#[must_use]
pub fn parse_args() -> MockConfig {
    parse_args_from(std::env::args().skip(1))
}

/// Parse arguments from any iterator over strings into a [`MockConfig`].
pub fn parse_args_from<I, S>(args: I) -> MockConfig
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let mut config = MockConfig::default();
    for item in args {
        config.process_arg(item.as_ref());
    }
    config
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_args_comprehensive() {
        let args = vec![
            "--position-encoding=utf-8",
            "--malform=bad-content-length",
            "--register-after-ms=500",
            "--register-method=textDocument/definition,callHierarchy",
            "--progress-count=100",
            "--config-request",
            "--crash-after=10",
        ];

        let config = parse_args_from(args);
        assert_eq!(config.position_encoding.as_deref(), Some("utf-8"));
        assert_eq!(config.malform, MalformMode::BadContentLength);
        assert_eq!(config.register_after_ms, Some(500));
        assert_eq!(
            config.register_methods,
            vec!["textDocument/definition", "callHierarchy"]
        );
        assert_eq!(config.progress_count, Some(100));
        assert!(config.config_request);
        assert_eq!(config.crash_after, 10);
    }
}
