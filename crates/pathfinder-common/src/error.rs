//! Pathfinder error taxonomy.
//!
//! All tools return errors in a standardized format:
//! `{ "error": "ERROR_CODE", "message": "...", "details": {} }`
//!
//! See PRD §5 for the full error taxonomy.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// A standardized error type for Pathfinder operations.
#[derive(Debug, thiserror::Error)]
pub enum PathfinderError {
    /// File path doesn't exist.
    #[error("file not found: {path}")]
    FileNotFound {
        /// Path to the missing file.
        path: PathBuf,
    },

    /// Semantic path doesn't resolve.
    #[error("symbol not found: {semantic_path}")]
    SymbolNotFound {
        /// The semantic path that wasn't found.
        semantic_path: String,
        /// Similar symbol names suggested by the system (Levenshtein distance).
        did_you_mean: Vec<String>,
        /// Spec 2.4: Suggested retry delay in seconds when LSP is warming up.
        /// None means the symbol doesn't exist (no amount of retrying will help).
        retry_after_seconds: Option<u32>,
    },

    /// Semantic path is malformed or missing required '::' separator.
    #[error("invalid semantic path: {input}")]
    InvalidSemanticPath {
        /// The invalid input string.
        input: String,
        /// Description of what makes it invalid.
        issue: String,
    },

    /// Multiple matches for a semantic path.
    #[error("ambiguous symbol: {semantic_path}")]
    AmbiguousSymbol {
        /// The ambiguous semantic path.
        semantic_path: String,
        /// All matching symbol paths.
        matches: Vec<String>,
    },

    /// No language server available for this file type.
    #[error("no LSP available for language: {language}")]
    NoLspAvailable { language: String },

    /// Language server crashed or returned an error.
    #[error("LSP error: {message}")]
    LspError { message: String },

    /// A generic I/O error occurred.
    #[error("I/O error: {message}")]
    IoError { message: String },

    /// LSP didn't respond within timeout.
    #[error("LSP timeout after {timeout_ms}ms")]
    LspTimeout { timeout_ms: u64 },

    /// File is in the sandbox deny-list.
    #[error("access denied: {path}")]
    AccessDenied { path: PathBuf, tier: SandboxTier },

    /// Tree-sitter couldn't parse the file.
    #[error("parse error in {path}: {reason}")]
    ParseError { path: PathBuf, reason: String },

    /// The language of the semantic path's file is not supported.
    #[error("unsupported language for target file: {path}")]
    UnsupportedLanguage { path: PathBuf },

    /// Response would exceed `max_tokens`.
    #[error("token budget exceeded: {used} / {budget}")]
    TokenBudgetExceeded { used: usize, budget: usize },

    /// Path traversal detected in `resolve_strict`.
    #[error("path traversal rejected: {path} escapes workspace root {workspace_root}")]
    PathTraversal {
        path: PathBuf,
        workspace_root: PathBuf,
    },
}

impl PathfinderError {
    /// Returns the MCP-facing error code string.
    #[must_use]
    pub const fn error_code(&self) -> &'static str {
        match self {
            Self::FileNotFound { .. } => "FILE_NOT_FOUND",
            Self::SymbolNotFound { .. } => "SYMBOL_NOT_FOUND",
            Self::AmbiguousSymbol { .. } => "AMBIGUOUS_SYMBOL",
            Self::NoLspAvailable { .. } => "NO_LSP_AVAILABLE",
            Self::LspError { .. } => "LSP_ERROR",
            Self::LspTimeout { .. } => "LSP_TIMEOUT",
            Self::AccessDenied { .. } => "ACCESS_DENIED",
            Self::IoError { .. } => "INTERNAL_ERROR",
            Self::ParseError { .. } => "PARSE_ERROR",
            Self::UnsupportedLanguage { .. } => "UNSUPPORTED_LANGUAGE",
            Self::TokenBudgetExceeded { .. } => "TOKEN_BUDGET_EXCEEDED",
            Self::InvalidSemanticPath { .. } => "INVALID_SEMANTIC_PATH",
            Self::PathTraversal { .. } => "PATH_TRAVERSAL",
        }
    }

    /// Returns an actionable hint for the agent to self-correct without additional round-trips.
    ///
    /// `SYMBOL_NOT_FOUND` hints are dynamic and built from the `did_you_mean` suggestions.
    /// All other hints are static strings referencing specific Pathfinder tools.
    #[must_use]
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::SymbolNotFound {
                semantic_path,
                did_you_mean,
                retry_after_seconds,
            } => Some(Self::hint_symbol_not_found(semantic_path, did_you_mean, *retry_after_seconds)),
            Self::AccessDenied { .. } => {
                Some("File is outside workspace sandbox. Check .pathfinderignore rules.".to_owned())
            }
            Self::UnsupportedLanguage { .. } => Some(
                "No tree-sitter grammar for this file type. Use read for raw content."
                    .to_owned(),
            ),
            Self::FileNotFound { .. } => Some(
                "Verify the file path is relative to the workspace root and the file exists. \
                 Use search(query=\"...\") to find the correct path, or explore to see all files."
                    .to_owned(),
            ),
            Self::InvalidSemanticPath { input, issue } => {
                Some(Self::hint_invalid_semantic_path(input, issue))
            }
            Self::PathTraversal { .. } => Some(
                "Path traversal is not allowed. Use a relative path without '..' components or absolute paths."
                    .to_owned(),
            ),
            Self::LspError { message } => Some(Self::hint_lsp_error(message)),
            Self::LspTimeout { timeout_ms } => Some(format!(
                "LSP timed out after {timeout_ms}ms. The language server may still be indexing, under memory pressure, or deadlocked. \
                 Workaround: use search + inspect (tree-sitter) instead of \
                 LSP-dependent tools (locate, trace, inspect). \
                 Check health for current status."
            )),
            Self::NoLspAvailable { language } => Some(format!(
                "No LSP available for {language}. Install a language server to enable LSP-dependent features. \
                 Tree-sitter tools (inspect, search, read) still work without LSP."
            )),
            _ => None,
        }
    }
    fn hint_symbol_not_found(
        semantic_path: &str,
        did_you_mean: &[String],
        retry_after_seconds: Option<u32>,
    ) -> String {
        // Spec 2.4: Include retry hint when warm-up is in progress
        if let Some(seconds) = retry_after_seconds {
            return format!(
                "LSP is still warming up (initial indexing). Retry in {seconds} seconds."
            );
        }

        // Detect path separator confusion: agent may have used `.` instead of `::`
        // (e.g., `src/lib.rs.MyStruct.method` instead of `src/lib.rs::MyStruct.method`)
        // or used `::` between nested symbols where `.` is expected.
        let separator_hint = if !semantic_path.contains("::") {
            Some(
                " Note: semantic paths require '::' between the file and symbol \
                 (e.g., 'src/lib.rs::MyStruct.method'). \
                 Nested symbols within the same file use '.' (e.g., 'MyStruct.method').",
            )
        } else if semantic_path.matches("::").count() > 1 {
            Some(
                " Note: only one '::' is allowed — between the file path and the symbol. \
                  Nested symbols within the file use '.' (e.g., 'src/lib.rs::Outer.Inner.method').",
            )
        } else {
            None
        };

        if did_you_mean.is_empty() {
            // No suggestions — the symbol might be in a different file than what the agent guessed.
            // Suggest search to locate the correct file.
            let base_name = semantic_path
                .split("::")
                .last()
                .unwrap_or(semantic_path)
                .split('.')
                .next()
                .unwrap_or(semantic_path);
            format!(
                "Symbol not found in the specified file. Use search(mode=\"symbol\", query=\"{base_name}\") to locate the correct file, or search(query=\"{base_name}\") to search the entire workspace.{}",
                separator_hint.unwrap_or("")
            )
        } else {
            format!(
                "Did you mean: {}? Use search if the symbol is in a different file, or read to see available symbols in this file.{}",
                did_you_mean.join(", "),
                separator_hint.unwrap_or("")
            )
        }
    }

    fn hint_invalid_semantic_path(input: &str, issue: &str) -> String {
        if issue.contains("symbol target") {
            format!(
                "'{input}' is a file path without a symbol target. This tool requires 'file.rs::symbol' format (e.g., 'src/auth.ts::AuthService.login'). If you want the full file content without symbol resolution, use read(filepath=\"{input}\")."
            )
        } else {
            format!(
                "'{input}' is not a valid semantic path. Use 'file.rs::symbol' format (e.g., 'src/auth.ts::AuthService.login'). The '::' separator is required between the file path and symbol name. Nested symbols within the same file use '.' (e.g., 'src/lib.rs::MyStruct.method')."
            )
        }
    }

    fn hint_lsp_error(message: &str) -> String {
        let hint = if message.contains("timed out") || message.contains("timeout") {
            format!(
                 "LSP timed out. The language server may still be indexing, under memory pressure, or deadlocked. \
                  Workaround: use search + inspect (tree-sitter) instead of \
                  LSP-dependent tools (locate, trace, inspect). \
                  Original error: {message}"
            )
        } else if message.contains("connection lost") || message.contains("crashed") {
            format!(
                "LSP process crashed or disconnected. Pathfinder will attempt to restart it. \
                 Workaround: use tree-sitter-based tools (search, inspect, read). \
                 Original error: {message}"
            )
        } else {
            format!(
                "LSP error: {message}. Workaround: use search for text-based navigation \
                 or check health for current status."
            )
        };
        hint
    }

    /// Serialize to the standard MCP error JSON format.
    #[must_use]
    pub fn to_error_response(&self) -> ErrorResponse {
        ErrorResponse {
            error: self.error_code().to_owned(),
            message: self.to_string(),
            details: self.to_details(),
            hint: self.hint(),
        }
    }

    fn to_details(&self) -> serde_json::Value {
        match self {
            Self::SymbolNotFound {
                did_you_mean,
                retry_after_seconds,
                ..
            } => {
                let mut details = serde_json::json!({ "did_you_mean": did_you_mean });
                if let Some(seconds) = retry_after_seconds {
                    details["retry_after_seconds"] = serde_json::json!(seconds);
                }
                details
            }
            Self::AmbiguousSymbol { matches, .. } => {
                serde_json::json!({ "matches": matches })
            }
            Self::AccessDenied { tier, .. } => {
                serde_json::json!({ "tier": tier })
            }
            Self::TokenBudgetExceeded { used, budget } => {
                serde_json::json!({ "used": used, "budget": budget })
            }
            Self::InvalidSemanticPath { issue, .. } => {
                serde_json::json!({ "issue": issue })
            }
            Self::PathTraversal {
                path,
                workspace_root,
            } => {
                serde_json::json!({ "path": path, "workspace_root": workspace_root })
            }
            _ => serde_json::Value::Object(serde_json::Map::new()),
        }
    }
}

/// Standard MCP error response format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ErrorResponse {
    /// Error code identifying the type of error.
    pub error: String,
    /// Human-readable message describing the error.
    pub message: String,
    /// Additional details about the error in JSON format.
    pub details: serde_json::Value,
    /// Actionable recovery hint for the agent. Present on most error variants.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

/// Sandbox tier that denied access.
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub enum SandboxTier {
    /// Always excluded, not configurable.
    HardcodedDeny,
    /// Excluded by default, overridable in config.
    DefaultDeny,
    /// User-defined via `.pathfinderignore`.
    UserDefined,
}

#[cfg(test)]
#[path = "error_test.rs"]
mod tests;
