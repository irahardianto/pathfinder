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
    /// Whether the LSP is ready for navigation operations (`get_definition`, `analyze_impact`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub navigation_ready: Option<bool>,
    /// Whether the LSP has completed initial workspace indexing.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexing_complete: Option<bool>,
    /// Seconds since the LSP process was spawned.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub uptime_seconds: Option<u64>,
    /// How this LSP provides diagnostics ("pull", "push", or "none").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics_strategy: Option<String>,
    /// LSP supports textDocument/definition (`get_definition`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_definition: Option<bool>,
    /// LSP supports textDocument/prepareCallHierarchy (`analyze_impact`, `read_with_deep_context`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_call_hierarchy: Option<bool>,
    /// LSP supports textDocument/diagnostic or publishDiagnostics (diagnostic health reporting).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_diagnostics: Option<bool>,
    /// LSP supports textDocument/rangeFormatting (edit formatting).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_formatting: Option<bool>,
    /// MT-2: The server identity reported in `serverInfo.name` during initialize.
    ///
    /// Examples: `"rust-analyzer"`, `"gopls"`, `"typescript-language-server"`.
    /// `None` when the server omits `serverInfo` or the process is unavailable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_name: Option<String>,
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
