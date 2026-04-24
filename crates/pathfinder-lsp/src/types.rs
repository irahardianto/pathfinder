//! Result types returned by the `Lawyer` trait.

use serde::{Deserialize, Serialize};

/// Status of a specific language server (for diagnostics/capabilities).
///
/// **Note:** This is a point-in-time snapshot taken when `get_repo_map` was called.
/// If the LSP process crashes or is restarted after this call, the `per_language`
/// map may be stale. Always treat `validation_skipped_reason` in edit responses
/// as the authoritative signal for the current edit operation's actual LSP status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct LspLanguageStatus {
    pub validation: bool,
    pub reason: String,
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
    pub name: String,
    pub kind: String, // "function", "method", etc.
    pub detail: Option<String>,
    pub file: String, // Relative path representation
    pub line: u32,    // 1-indexed
    pub column: u32,  // 1-indexed

    // Internal generic LSP data needed for incoming/outgoing requests
    #[serde(default)]
    pub data: Option<serde_json::Value>,
}

/// Represents an incoming or outgoing call in the hierarchy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallHierarchyCall {
    pub item: CallHierarchyItem, // Caller (if incoming) or Callee (if outgoing)
    pub call_sites: Vec<u32>,    // 1-indexed lines where calls occur
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
    Created = 1,
    Changed = 2,
    Deleted = 3,
}
