//! Result types returned by the `Lawyer` trait.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// How indexing completion was determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum IndexingCompletionSource {
    /// LSP sent `WorkDoneProgressEnd` notification — indexing completion confirmed.
    Progress,
    /// Timeout elapsed without `WorkDoneProgressEnd` notification; indexing assumed complete.
    ///
    /// **What this means:**
    /// The LSP did not send a progress-end notification within the expected window, so
    /// Pathfinder assumed indexing was done and allowed tool requests to proceed.
    ///
    /// **When this is normal:**
    /// Some LSP servers (e.g., older versions of gopls, pyright) never send
    /// `WorkDoneProgressEnd` — they only send diagnostics. In this case,
    /// `timeout_fallback` is expected and LSP-backed tools work correctly.
    ///
    /// **When this indicates a problem:**
    /// If `navigation_ready=false` AND `indexing_source="timeout_fallback"`, the LSP
    /// may have crashed or stalled before completing indexing. In this case:
    ///   1. Check `health` again in 10-30 seconds.
    ///   2. Use `health { action: "restart" }` to force-restart the LSP.
    ///   3. If it persists, check LSP server logs.
    ///
    /// **Impact on tool results:**
    /// `timeout_fallback` alone does not degrade tool results — only `navigation_ready`
    /// determines whether LSP-backed features (`locate`, `trace`) are available.
    TimeoutFallback,
}

/// Language server status for a single LSP language slot.
///
/// Returned by the `explore` tool and validation tools to communicate the
/// current health of the associated language server process.
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct LspLanguageStatus {
    /// Whether validation is enabled.
    pub validation: bool,
    /// Reason explaining the validation status.
    pub reason: String,
    /// Whether the LSP is ready for navigation operations (`locate`, `trace`).
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
    /// LSP supports textDocument/definition (`locate`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub supports_definition: Option<bool>,
    /// LSP supports textDocument/prepareCallHierarchy (`trace`, `inspect`).
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
    /// How indexing completion was determined: `"progress"` or `"timeout_fallback"`.
    ///
    /// - `"progress"`: LSP confirmed via `WorkDoneProgressEnd` — reliable.
    /// - `"timeout_fallback"`: LSP never sent completion signal; assumed complete after timeout.
    ///   This is normal for some servers (gopls, pyright). Only a concern when
    ///   combined with `navigation_ready=false`. See [`IndexingCompletionSource::TimeoutFallback`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexing_source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexing_duration_secs: Option<u64>,
    /// Indexing progress percentage (0-100) if the LSP reports it via workDoneProgress.
    /// `None` when the LSP does not report progress or indexing is complete.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub indexing_progress_percent: Option<u8>,
    /// Number of dynamic capability registrations received from the LSP server.
    /// Useful for diagnosing dynamic registration delays (e.g., jdtls).
    /// `None` when the process is not running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registrations_received: Option<u32>,
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
    /// Optional raw data for LSP call hierarchy requests.
    #[serde(default)]
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

/// A location where a symbol is referenced (used, called, or accessed).
///
/// Returned by `textDocument/references` LSP request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReferenceLocation {
    /// Relative file path within the workspace root.
    pub file: String,
    /// 1-indexed line number where the reference occurs.
    pub line: u32,
    /// 1-indexed column number where the reference occurs.
    pub column: u32,
    /// A short code snippet showing the reference (e.g., function call or variable access).
    pub snippet: String,
}

/// A single file system change event for `workspace/didChangeWatchedFiles`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FileEvent {
    /// Absolute file URI (e.g., `file:///home/user/project/src/auth.ts`).
    pub uri: String,
    /// Nature of the change.
    pub change_type: FileChangeType,
}

/// LSP `FileChangeType` values (§3.17.20 of the LSP spec).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[repr(u8)]
pub enum FileChangeType {
    /// File was created.
    Created = 1,
    /// File was changed.
    Changed = 2,
    /// File was deleted.
    Deleted = 3,
}
