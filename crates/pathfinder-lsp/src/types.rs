//! Result types returned by the `Lawyer` trait.

use serde::{Deserialize, Serialize};

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

impl LspDiagnostic {
    /// Returns `true` if this diagnostic is a blocking error (severity 1).
    #[must_use]
    pub fn is_error(&self) -> bool {
        self.severity == LspDiagnosticSeverity::Error
    }
}
