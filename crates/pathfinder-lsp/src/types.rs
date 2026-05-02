//! Result types returned by the `Lawyer` trait.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Language server status for a single LSP language slot.
///
/// Returned by the `get_repo_map` and validation tools to communicate the
/// current health of the associated language server process.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct LspLanguageStatus {
    /// Whether validation is enabled.
    pub validation: bool,
    /// Reason explaining the validation status.
    pub reason: String,
    /// Whether the LSP is ready for navigation operations (get_definition, analyze_impact).
    ///
    /// `Some(true)` — initialize handshake completed with `definitionProvider: true`.
    /// Navigation tools are functional regardless of `indexing_complete` status.
    ///
    /// `Some(false)` — process running but navigation not yet available.
    ///
    /// `None` — process not started or unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigation_ready: Option<bool>,
    /// Whether the LSP has completed initial workspace indexing.
    ///
    /// `Some(true)` — the LSP emitted a `WorkDoneProgressEnd` for its initial
    /// indexing token, indicating the workspace index is fully built and navigation
    /// tools (`get_definition`, `analyze_impact`) should return reliable results.
    ///
    /// `Some(false)` — the process is running but indexing is still in progress.
    ///
    /// `None` — the process has not started yet, is unavailable, or does not
    /// report `WorkDoneProgress` (e.g., some LSPs omit it). Agents should treat
    /// `None` the same as `Some(false)` and prefer `validation_skipped_reason` in
    /// edit responses as the authoritative per-operation LSP health signal.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexing_complete: Option<bool>,
    /// Seconds since the LSP process was spawned.
    ///
    /// Useful alongside `indexing_complete = Some(false)` to gauge warmup progress.
    /// `None` when the process has not started or is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_seconds: Option<u64>,
    /// How this LSP provides diagnostics ("pull", "push", or "none").
    ///
    /// `None` when the process hasn't started yet (lazy start).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics_strategy: Option<String>,
    /// LSP supports textDocument/definition (`get_definition`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_definition: Option<bool>,
    /// LSP supports textDocument/prepareCallHierarchy (`analyze_impact`, `read_with_deep_context`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_call_hierarchy: Option<bool>,
    /// LSP supports textDocument/diagnostic or publishDiagnostics (`validate_only`, edit validation).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_diagnostics: Option<bool>,
    /// LSP supports textDocument/rangeFormatting (edit formatting).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_formatting: Option<bool>,
}

/// The location of a symbol's definition in the workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefinitionLocation {
    /// Relative file path within the workspace root.
    pub file: String,
    /// 1-indexed line number of the definition.
    pub line: u32,
    /// 1-indexed column number of the definition.
    pub column: u32,
    /// The first line of the definition (for preview in tool responses).
    pub preview: String,
}

/// Diagnostic severity level as defined by LSP 3.17.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum LspDiagnosticSeverity {
    /// A build error — blocks the edit (severity 1 in LSP).
    Error = 1,
    /// A warning — reported but does not block the edit.
    Warning = 2,
    /// An informational hint.
    Information = 3,
    /// A style/hint level message.
    Hint = 4,
}

/// A single diagnostic message from the language server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LspDiagnostic {
    /// The severity of the diagnostic.
    pub severity: LspDiagnosticSeverity,
    /// An optional error code (e.g., "TS2304", "E0308").
    pub code: Option<String>,
    /// The human-readable error message.
    pub message: String,
    /// The relative file path where the diagnostic occurs.
    pub file: String,
    /// 1-indexed start line.
    pub start_line: u32,
    /// 1-indexed end line.
    pub end_line: u32,
}

/// A node in the call hierarchy (e.g. a function or method).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallHierarchyItem {
    /// The name of the item (e.g., function or method).
    pub name: String,
    /// The kind of the item (e.g., "function", "method").
    pub kind: String, // "function", "method", etc.
    /// Additional details for the item.
    pub detail: Option<String>,
    /// Relative file path representation.
    pub file: String, // Relative path representation
    /// 1-indexed line where the item is located.
    pub line: u32, // 1-indexed
    /// 1-indexed column where the item is located.
    pub column: u32, // 1-indexed

    // Internal generic LSP data needed for incoming/outgoing requests
    #[serde(default)]
    /// Optional raw data for LSP call hierarchy requests.
    pub data: Option<serde_json::Value>,
}

/// Represents an incoming or outgoing call in the hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallHierarchyCall {
    /// The call hierarchy item (caller or callee).
    pub item: CallHierarchyItem, // Caller (if incoming) or Callee (if outgoing)
    /// Lines where calls occur (1-indexed).
    pub call_sites: Vec<u32>, // 1-indexed lines where calls occur
}

impl LspDiagnostic {
    /// Returns `true` if this diagnostic is a blocking error (severity 1).
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.severity == LspDiagnosticSeverity::Error
    }
}

/// A single file system change event for `workspace/didChangeWatchedFiles`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileEvent {
    /// Absolute file URI (e.g., `file:///home/user/project/src/auth.ts`).
    pub uri: String,
    /// Nature of the change.
    pub change_type: FileChangeType,
}

/// LSP `FileChangeType` values (§3.17.20 of the LSP spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum FileChangeType {
    /// File was created.
    Created = 1,
    /// File was changed.
    Changed = 2,
    /// File was deleted.
    Deleted = 3,
}
